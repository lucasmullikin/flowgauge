//! Samples on a background thread so slow reads never block the menu bar. Everything is stored
//! in history here, whether or not a panel is open, so graphs have no holes.

use crate::fan_control::{FanDriver, FanState};
use crate::view::{DiskRate, memory_percent};
use gauge_core::alerts::AlertTracker;
use gauge_core::battery::{self, Battery, DeviceBattery, PowerSample, charge_change};
use gauge_core::drives::{self, DiskActivity, Volume};
use gauge_core::gpu::{self, Gpu};
use gauge_core::history::{History, local_day, local_midnight, month_start_day};
use gauge_core::memory::{self, Memory, Pressure};
use gauge_core::names::Names;
use gauge_core::network::{self, Counters, InterfaceRate};
use gauge_core::pathcheck::{self, Tracker};
use gauge_core::traffic;
use crate::net_view::{DnsServer, PathNow, Recent, TrafficNow};
use gauge_core::ping;
use gauge_core::processor::{Cpu, Sampler, TopProcess};
use gauge_core::sensors::{AreaTemperature, Fan, Kind, Reading, Sensors, curve_temperature, temperature_areas};
use gauge_core::settings::{FanMode, Settings};
use gauge_core::smart::{self, DriveHealth};
use gauge_core::wifi::{self, Wifi};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: Duration = Duration::from_secs(1);
const SYSTEM_EVERY: u64 = 2;
const FAN_ACTIVE_EVERY: u64 = 2;
const FAN_IDLE_EVERY: u64 = 3;
const DRIVE_EVERY: u64 = 5;
const WIFI_EVERY: u64 = 10;
const ROUTE_EVERY: u64 = 10;
const POWER_EVERY: u64 = 2;
const CHARGE_EVERY: u64 = 3;
const DEVICE_BATTERY_EVERY: u64 = 30;
/// Often enough that plugging in a phone shows up quickly.
const USB_DEVICE_EVERY: u64 = 5;
const FLUSH_EVERY: u64 = 60;
const SMART_EVERY: u64 = 60;
const SENSOR_LIST_EVERY: u64 = 3;
const PING_INTERVAL: Duration = Duration::from_secs(5);
const PING_TIMEOUT: Duration = Duration::from_secs(2);
/// The DNS and internet checks run on every other ping.
const PATH_EVERY_PINGS: u64 = 2;
const RESOLVE_EVERY_PINGS: u64 = 60;
const DNS_SERVERS_EVERY_PINGS: u64 = 6;
const TRAFFIC_OPEN: Duration = Duration::from_secs(2);
const TRAFFIC_CLOSED: Duration = Duration::from_secs(10);
const GEO_RETRY: Duration = Duration::from_secs(600);
const ALERT_HOLD_SECONDS: i64 = 30;
pub const NO_MODULE: usize = usize::MAX;

pub enum Update {
    Network(Vec<InterfaceRate>),
    Totals((u64, u64), (u64, u64)),
    Traffic(Box<TrafficNow>),
    /// What's happening with the location file, or `None` once it's ready or not wanted.
    GeoStatus(Option<String>),
    Path(PathNow),
    Wifi(Option<Wifi>),
    Ping(Option<f32>),
    Cpu(Cpu),
    Processes(Vec<TopProcess>),
    Gpu(Option<Gpu>),
    Memory(Memory),
    Drives(Vec<Volume>, Vec<DiskRate>),
    Health(Vec<DriveHealth>),
    Fans(Vec<Fan>, Option<(String, f32)>),
    FanState(FanState),
    Battery(Option<Box<Battery>>),
    Sensors(Vec<Reading>),
    Temperatures(Vec<AreaTemperature>),
    DeviceBatteries(Vec<DeviceBattery>),
    UsbDevices(Vec<battery::UsbDevice>),
    EnergyToday(f64),
    Power(PowerSample),
    ChargeLimit(Option<Option<u8>>, Option<String>),
    TopUpFinished,
}

pub struct Shared {
    pub history: Arc<Mutex<History>>,
    pub settings: Arc<Mutex<Settings>>,
    /// Index into `MODULES` of the panel that's open, or `NO_MODULE`.
    pub open_module: AtomicUsize,
    pub install_message: Mutex<Option<String>>,
    /// Reading every sensor is costly, so it only happens while their window is open.
    pub sensors_open: AtomicBool,
    pub network_window_open: AtomicBool,
    pub names: Names,
    /// Country lookups, loaded when locations are turned on.
    pub geo: Mutex<Option<Arc<gauge_core::geo::Geo>>>,
    /// Asks for a fresh download of the location file.
    pub geo_refresh: AtomicBool,
}

fn now_seconds() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn disk_rates(before: &[DiskActivity], after: Vec<DiskActivity>, seconds: f64) -> Vec<DiskRate> {
    after
        .into_iter()
        .map(|disk| {
            let previous = before.iter().find(|b| b.bsd_name == disk.bsd_name);
            DiskRate {
                read_per_sec: previous.map_or(0.0, |p| network::rate(p.bytes_read, disk.bytes_read, seconds)),
                write_per_sec: previous.map_or(0.0, |p| network::rate(p.bytes_written, disk.bytes_written, seconds)),
                disk,
            }
        })
        .collect()
}

fn notify(message: &str) {
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", "display notification (item 1 of argv) with title \"FlowGauge\"", "-e", "end run", message])
        .spawn()
        .map(|mut child| std::thread::spawn(move || child.wait()));
}

/// Looks the check address up on its own thread, so a DNS outage can't stall the pings.
fn resolve_in_background(target: &Arc<Mutex<(Option<std::net::Ipv4Addr>, Option<String>)>>, host: String) {
    let target = target.clone();
    std::thread::spawn(move || {
        let resolved = pathcheck::resolve_target(&host);
        if let Ok(mut slot) = target.lock() {
            *slot = resolved;
        }
    });
}

fn dns_servers(gateway: Option<std::net::Ipv4Addr>) -> Vec<DnsServer> {
    gauge_core::dns::read()
        .into_iter()
        .flat_map(|resolver| {
            resolver.nameservers.into_iter().map(move |address| {
                let provider = gauge_core::dns::provider(&address, resolver.interface.as_deref(), gateway.map(std::net::IpAddr::V4));
                DnsServer { address, provider, interface: resolver.interface.clone() }
            })
        })
        .collect()
}

fn spawn_ping(updates: Sender<Update>, shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let mut sequence = 0u16;
        let mut count = 0u64;
        let (mut router, mut dns, mut internet) = (Tracker::default(), Tracker::default(), Tracker::default());
        let mut checked_host = String::new();
        let target = Arc::new(Mutex::new((None, None)));
        let mut servers: Vec<DnsServer> = Vec::new();
        loop {
            std::thread::sleep(PING_INTERVAL);
            sequence = sequence.wrapping_add(1);
            count += 1;
            let (custom, check_host) = shared
                .settings
                .lock()
                .map(|s| (s.ping_host.clone(), s.check_host.clone()))
                .unwrap_or_default();
            let gateway = ping::default_gateway();
            let ping_target = custom.parse().ok().or(gateway);
            let result = ping_target.and_then(|address| ping::ping(address, sequence, PING_TIMEOUT));
            if let (Some(ms), Ok(mut history)) = (result, shared.history.lock()) {
                history.record("ping", now_seconds(), ms);
            }
            if updates.send(Update::Ping(result)).is_err() {
                return;
            }
            // The router step always pings the router, even when the ping row uses another address.
            let router_result = if custom.is_empty() { result } else { gateway.and_then(|g| ping::ping(g, sequence, PING_TIMEOUT)) };
            router.push(router_result);
            if count == 1 || count.is_multiple_of(DNS_SERVERS_EVERY_PINGS) {
                servers = dns_servers(gateway);
            }
            if check_host != checked_host {
                checked_host = check_host.clone();
                internet.clear();
                if let Ok(mut slot) = target.lock() {
                    *slot = (None, None);
                }
                resolve_in_background(&target, checked_host.clone());
            } else if count.is_multiple_of(RESOLVE_EVERY_PINGS) && !checked_host.is_empty() {
                resolve_in_background(&target, checked_host.clone());
            }
            if count == 1 || count.is_multiple_of(PATH_EVERY_PINGS) {
                // Asks about the root, which every DNS server answers and which says nothing about you.
                if let Some(server) = servers.first().map(|s| s.address.clone()).or_else(pathcheck::system_resolver) {
                    dns.push(pathcheck::time_dns(&server, ".", sequence, PING_TIMEOUT));
                }
                if !checked_host.is_empty() {
                    let address = target.lock().ok().and_then(|t| t.0);
                    internet.push(address.and_then(|a| ping::ping(a, sequence, PING_TIMEOUT)));
                }
            }
            let path = PathNow {
                router: router.step(f32::MAX),
                dns: dns.step(f32::MAX),
                internet: if checked_host.is_empty() { pathcheck::Step::off() } else { internet.step(f32::MAX) },
                checks: count,
                gateway,
                dns_servers: servers.clone(),
            };
            if updates.send(Update::Path(path)).is_err() {
                return;
            }
        }
    });
}

/// Loads the location file, downloading it first if it's missing or a refresh was asked for.
/// A failed download is tried again after `GEO_RETRY`, not in a loop.
fn spawn_geo_loader(updates: Sender<Update>, shared: Arc<Shared>, busy: Arc<AtomicBool>, refresh: bool) {
    busy.store(true, Ordering::SeqCst);
    std::thread::spawn(move || {
        let support = gauge_core::settings::support_dir();
        let mut geo = if refresh { None } else { gauge_core::geo::load(&support) };
        let mut problem = None;
        if geo.is_none() {
            let _ = updates.send(Update::GeoStatus(Some("Downloading location data from db-ip.com…".into())));
            match gauge_core::geo::download(&support, now_seconds()) {
                Ok(_) => geo = gauge_core::geo::load(&support),
                Err(error) => problem = Some(format!("Location data didn't download: {error}")),
            }
        }
        if let Some(geo) = geo
            && let Ok(mut slot) = shared.geo.lock()
        {
            *slot = Some(Arc::new(geo));
        } else if problem.is_none() {
            problem = Some("Location data couldn't be read".into());
        }
        let failed = problem.is_some();
        if let Some(problem) = &problem {
            eprintln!("gauge: {problem}");
        }
        let _ = updates.send(Update::GeoStatus(problem));
        if failed {
            // Lets the next sample try again later, without retrying in a tight loop.
            std::thread::sleep(GEO_RETRY);
            busy.store(false, Ordering::SeqCst);
        }
    });
}

/// Samples per-app traffic: often while Network is open, and slowly otherwise so the timeline
/// still fills in. Nothing runs when app traffic is turned off in settings.
fn spawn_traffic(updates: Sender<Update>, shared: Arc<Shared>, network_index: usize) {
    std::thread::spawn(move || {
        let names = shared.names.clone();
        let mut tracker = traffic::Tracker::default();
        let mut recent = Recent::default();
        let mut last = Instant::now();
        let mut was_on = false;
        let geo_busy = Arc::new(AtomicBool::new(false));
        let mut country_names: HashMap<String, String> = HashMap::new();
        loop {
            let open = shared.open_module.load(Ordering::Relaxed) == network_index
                || shared.network_window_open.load(Ordering::Relaxed);
            std::thread::sleep(if open { TRAFFIC_OPEN } else { TRAFFIC_CLOSED });
            let (on, resolve, countries) = shared
                .settings
                .lock()
                .map(|s| (s.show_top_apps, s.resolve_names, s.show_countries))
                .unwrap_or((false, false, false));
            names.set_enabled(on && resolve);
            let refresh = shared.geo_refresh.swap(false, Ordering::SeqCst);
            if countries && (refresh || !geo_busy.load(Ordering::SeqCst)) {
                spawn_geo_loader(updates.clone(), shared.clone(), geo_busy.clone(), refresh);
            } else if !countries && geo_busy.swap(false, Ordering::SeqCst) {
                // Frees the lookup table, and lets turning it back on try again.
                if let Ok(mut slot) = shared.geo.lock() {
                    *slot = None;
                }
                let _ = updates.send(Update::GeoStatus(None));
            }
            if !on {
                if was_on {
                    tracker = traffic::Tracker::default();
                    recent.clear();
                    names.clear();
                    let _ = updates.send(Update::Traffic(Box::default()));
                }
                was_on = false;
                continue;
            }
            was_on = true;
            let Ok(snapshot) = traffic::read_snapshot() else { continue };
            let seconds = last.elapsed().as_secs_f64().max(0.5);
            last = Instant::now();
            let interval = tracker.step(&snapshot, seconds);
            let now = now_seconds();
            if let Ok(mut history) = shared.history.lock() {
                for (app, usage) in &interval.apps {
                    history.add_app_usage(now, app, usage.bytes_in, usage.bytes_out);
                }
                for (app, destinations) in &interval.destinations {
                    for (address, usage) in destinations {
                        history.add_host_usage(now, app, &address.to_string(), usage.bytes_in, usage.bytes_out);
                    }
                }
            }
            let rate = |bytes: u64| bytes as f64 / seconds;
            let apps: BTreeMap<String, (f64, f64)> =
                interval.apps.iter().map(|(app, u)| (app.clone(), (rate(u.bytes_in), rate(u.bytes_out)))).collect();
            let places: BTreeMap<(String, std::net::IpAddr), (f64, f64)> = interval
                .destinations
                .iter()
                .flat_map(|(app, destinations)| {
                    destinations.iter().map(move |(address, u)| ((app.clone(), *address), (rate(u.bytes_in), rate(u.bytes_out))))
                })
                .collect();
            recent.update(&apps, &places, &interval.rtt_ms, seconds);
            let mut snapshot = recent.snapshot(&|address| names.get(address));
            let geo = if countries { shared.geo.lock().ok().and_then(|g| g.clone()) } else { None };
            if let Some(geo) = geo {
                for destination in snapshot.apps.iter_mut().flat_map(|a| a.destinations.iter_mut()) {
                    if traffic::is_local(&destination.address) {
                        continue;
                    }
                    destination.country = geo.country(destination.address).map(|code| {
                        country_names.entry(code.clone()).or_insert_with(|| crate::net_view::country_name(&code)).clone()
                    });
                }
            }
            if updates.send(Update::Traffic(Box::new(snapshot))).is_err() {
                return;
            }
        }
    });
}

/// Runs for the app's lifetime. Ends quietly once the main thread has gone away.
pub fn run(updates: Sender<Update>, shared: Arc<Shared>) {
    let sensors = Sensors::open().ok();
    let mut sampler = Sampler::new();
    let mut counters: Vec<Counters> = network::read_counters().unwrap_or_default();
    let mut counters_at = Instant::now();
    let mut disks = drives::read_activity();
    let mut disks_at = Instant::now();
    let mut addresses = BTreeMap::new();
    let mut default = None;
    let mut last_smart: Option<u64> = None;
    let mut driver = FanDriver::default();
    let mut alerts = AlertTracker::default();
    let mut tick: u64 = 0;
    spawn_ping(updates.clone(), shared.clone());
    let network_index = 0;
    spawn_traffic(updates.clone(), shared.clone(), network_index);
    let memory_index = 1;
    let cpu_index = 2;
    let drives_index = 4;
    let fans_index = 5;
    loop {
        std::thread::sleep(TICK);
        let now = now_seconds();
        let open = shared.open_module.load(Ordering::Relaxed);
        let settings = shared.settings.lock().map(|s| s.clone()).unwrap_or_default();

        if tick.is_multiple_of(ROUTE_EVERY) {
            addresses = network::read_addresses();
            default = network::default_interface();
        }
        if let Ok(current) = network::read_counters() {
            let seconds = counters_at.elapsed().as_secs_f64();
            let rates = network::rates(&counters, &current, seconds, &addresses, default.as_deref());
            if let Some(route) = rates.iter().find(|r| r.is_default) {
                if let Ok(mut history) = shared.history.lock() {
                    history.record("net.down", now, route.in_per_sec as f32);
                    history.record("net.up", now, route.out_per_sec as f32);
                }
                let before = counters.iter().find(|c| c.name == route.name);
                if let (Some(before), Ok(mut history)) = (before, shared.history.lock()) {
                    history.add_total(
                        &local_day(now, 0),
                        &route.name,
                        route.bytes_in_total.saturating_sub(before.bytes_in),
                        route.bytes_out_total.saturating_sub(before.bytes_out),
                    );
                }
            }
            let connected = rates.iter().any(|r| r.is_default);
            if settings.alerts.connection_down_enabled
                && alerts.check("connection", !connected, ALERT_HOLD_SECONDS, now)
            {
                notify("Network connection is down");
            }
            counters = current;
            counters_at = Instant::now();
            if updates.send(Update::Network(rates)).is_err() {
                return;
            }
        }

        if tick.is_multiple_of(SYSTEM_EVERY) {
            let want_processes = open == cpu_index || open == memory_index;
            let (cpu, processes) = sampler.poll(want_processes);
            if let Ok(mut history) = shared.history.lock() {
                history.record("cpu.total", now, cpu.total_percent);
            }
            let _ = updates.send(Update::Cpu(cpu));
            if let Some(processes) = processes {
                let _ = updates.send(Update::Processes(processes));
            }
            if let Some(memory) = memory::read() {
                if let Ok(mut history) = shared.history.lock() {
                    history.record("mem.used", now, memory_percent(&memory) as f32);
                    history.record(
                        "mem.compressed",
                        now,
                        (memory.compressed_bytes as f64 / memory.total_bytes.max(1) as f64 * 100.0) as f32,
                    );
                }
                if settings.alerts.memory_pressure_enabled
                    && alerts.check("pressure", memory.pressure == Pressure::Critical, ALERT_HOLD_SECONDS, now)
                {
                    notify("Memory pressure is critical");
                }
                let _ = updates.send(Update::Memory(memory));
            }
            let gpu = gpu::read().into_iter().next();
            if let Some(gpu) = &gpu
                && let Ok(mut history) = shared.history.lock()
            {
                if let Some(percent) = gpu.device_percent {
                    history.record("gpu.device", now, percent as f32);
                }
                if let (Some(used), Some(total)) = (gpu.memory_in_use_bytes, memory::read().map(|m| m.total_bytes)) {
                    history.record("gpu.memory", now, (used as f64 / total.max(1) as f64 * 100.0) as f32);
                }
            }
            let _ = updates.send(Update::Gpu(gpu));
        }

        let controlling = driver.is_controlling() || !matches!(settings.preset().map(|p| &p.mode), None | Some(FanMode::Auto));
        let fan_every = if controlling { FAN_ACTIVE_EVERY } else { FAN_IDLE_EVERY };
        if let Some(sensors) = &sensors
            && tick.is_multiple_of(fan_every)
            && let Ok(fans) = sensors.fans()
        {
            let hot = sensors.hottest();
            if let Ok(mut history) = shared.history.lock() {
                for fan in &fans {
                    history.record(&format!("fan.{}", fan.index), now, fan.actual_rpm);
                }
                if let Some((_, c)) = &hot {
                    let shown = match settings.temperature_unit {
                        gauge_core::settings::TemperatureUnit::Celsius => *c,
                        gauge_core::settings::TemperatureUnit::Fahrenheit => *c * 9.0 / 5.0 + 32.0,
                    };
                    history.record("temp.hot", now, shown);
                }
            }
            let mode = settings.preset().map_or(FanMode::Auto, |p| p.mode.clone());
            // Reading every sensor costs a little, so it only happens when a curve needs it.
            let curve_temperature_c = matches!(mode, FanMode::Curve { .. })
                .then(|| curve_temperature(&temperature_areas(&sensors.read_kind(Kind::Temperature))))
                .flatten();
            let state = driver.step(&mode, &fans, curve_temperature_c);
            let _ = updates.send(Update::FanState(state));
            if settings.alerts.hot_enabled
                && alerts.check("hot", hot.as_ref().is_some_and(|h| h.1 >= settings.alerts.hot_c), ALERT_HOLD_SECONDS, now)
            {
                notify("This Mac is running very hot");
            }
            let _ = updates.send(Update::Fans(fans, hot));
            if open == fans_index {
                let _ = updates.send(Update::Temperatures(temperature_areas(&sensors.read_kind(Kind::Temperature))));
            }
        }

        if let Some(sensors) = &sensors
            && shared.sensors_open.load(Ordering::Relaxed)
            && tick.is_multiple_of(SENSOR_LIST_EVERY)
        {
            let _ = updates.send(Update::Sensors(sensors.read()));
        }

        if tick.is_multiple_of(DRIVE_EVERY) {
            let current = drives::read_activity();
            let rates = disk_rates(&disks, current.clone(), disks_at.elapsed().as_secs_f64());
            disks = current;
            disks_at = Instant::now();
            if let Ok(mut history) = shared.history.lock() {
                history.record("disk.read", now, rates.iter().map(|d| d.read_per_sec).sum::<f64>() as f32);
                history.record("disk.write", now, rates.iter().map(|d| d.write_per_sec).sum::<f64>() as f32);
            }
            let volumes = drives::read_volumes();
            if let Some(root) = volumes.iter().find(|v| v.mount_point == "/") {
                let free = root.available_bytes as f64 / root.total_bytes.max(1) as f64 * 100.0;
                if settings.alerts.disk_low_enabled
                    && alerts.check("disk", free < f64::from(settings.alerts.disk_low_percent), 0, now)
                {
                    notify("Your startup disk is low on space");
                }
            }
            let _ = updates.send(Update::Drives(volumes, rates));
        }

        if settings.show_wifi && tick.is_multiple_of(WIFI_EVERY) {
            let wifi = wifi::read();
            if let Some(wifi) = &wifi
                && let Ok(mut history) = shared.history.lock()
            {
                history.record("wifi.rssi", now, wifi.rssi_dbm as f32);
            }
            let _ = updates.send(Update::Wifi(wifi));
        }

        if tick.is_multiple_of(POWER_EVERY) {
            let battery = battery::read();
            if let Some(battery) = &battery
                && let Ok(mut history) = shared.history.lock()
            {
                if let Some(percent) = battery.percent {
                    history.record("battery.percent", now, percent as f32);
                }
                if let Some(celsius) = battery.temperature_c {
                    history.record("battery.temp", now, celsius);
                }
            }
            if let Some(sensors) = &sensors {
                let power = sensors.power();
                if let Ok(mut history) = shared.history.lock() {
                    history.record("power.load", now, power.system_watts as f32);
                    history.record("power.in", now, power.adapter_watts as f32);
                }
                let _ = updates.send(Update::Power(power));
            }
            if settings.top_up && battery.as_ref().and_then(|b| b.percent).is_some_and(|p| p >= 100) {
                let _ = updates.send(Update::TopUpFinished);
            }
            let _ = updates.send(Update::Battery(battery.map(Box::new)));
            if let Ok(history) = shared.history.lock() {
                let energy = history.energy_wh("power.load", local_midnight(now)).unwrap_or(0.0);
                let _ = updates.send(Update::EnergyToday(energy));
            }
        }
        // Checked often enough that slider changes apply quickly, and a limit changed in System
        // Settings shows up. Reading it is cheap, and nothing is written unless it differs.
        if tick.is_multiple_of(CHARGE_EVERY) {
            let actual = battery::read_charge_limit();
            let mut error = None;
            if let Some(actual) = actual
                && let Some(wanted) = charge_change(settings.charge_limit, settings.top_up, actual)
            {
                error = battery::write_charge_limit(wanted).err();
            }
            let _ = updates.send(Update::ChargeLimit(actual, error));
        }
        if tick.is_multiple_of(USB_DEVICE_EVERY) {
            let _ = updates.send(Update::UsbDevices(battery::read_usb_devices()));
        }
        if tick.is_multiple_of(DEVICE_BATTERY_EVERY) {
            let _ = updates.send(Update::DeviceBatteries(battery::read_devices()));
        }

        if open == drives_index && last_smart.is_none_or(|at| tick >= at + SMART_EVERY) {
            last_smart = Some(tick);
            let _ = updates.send(Update::Health(smart::read_all()));
        }

        if tick.is_multiple_of(FLUSH_EVERY) && let Ok(mut history) = shared.history.lock() {
            let today = local_day(now, 0);
            if let Err(error) = history.flush(now) {
                eprintln!("gauge: could not save history: {error}");
            }
            let totals = (
                history.totals_since(&today).unwrap_or_default(),
                history.totals_since(&month_start_day(now)).unwrap_or_default(),
            );
            let _ = updates.send(Update::Totals(totals.0, totals.1));
        }
        tick += 1;
    }
}

pub fn absorb(latest: &mut crate::view::Latest, update: Update) {
    match update {
        Update::Network(interfaces) => latest.interfaces = interfaces,
        Update::Totals(today, month) => {
            latest.today = today;
            latest.month = month;
        }
        Update::Traffic(traffic) => latest.traffic = Some(*traffic),
        Update::GeoStatus(status) => latest.geo_status = status,
        Update::Path(path) => latest.path = Some(path),
        Update::Wifi(wifi) => latest.wifi = wifi,
        Update::Ping(ms) => latest.ping_ms = ms,
        Update::Cpu(cpu) => latest.cpu = Some(cpu),
        Update::Processes(processes) => latest.processes = processes,
        Update::Gpu(gpu) => latest.gpu = gpu,
        Update::Memory(memory) => latest.memory = Some(memory),
        Update::Drives(volumes, disks) => {
            latest.volumes = volumes;
            latest.disks = disks;
        }
        Update::Health(health) => latest.health = health,
        Update::Fans(fans, hottest) => {
            latest.fans = fans;
            latest.hottest = hottest;
        }
        Update::FanState(state) => latest.fan_state = state,
        Update::Battery(battery) => latest.battery = battery.map(|b| *b),
        Update::Sensors(sensors) => latest.sensors = sensors,
        Update::Temperatures(areas) => latest.temperatures = areas,
        Update::DeviceBatteries(devices) => latest.device_batteries = devices,
        Update::UsbDevices(devices) => latest.usb_devices = devices,
        Update::EnergyToday(watt_hours) => latest.energy_today_wh = watt_hours,
        Update::Power(power) => latest.power = Some(power),
        Update::ChargeLimit(actual, error) => {
            latest.charge_limit = actual;
            latest.charge_error = error;
        }
        Update::TopUpFinished => {}
    }
}
