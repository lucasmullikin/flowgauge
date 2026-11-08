//! `--render`: draws panels or settings pages to PNGs, for checking layouts without screen
//! recording permission and for the README. Images use made-up readings and history unless
//! `real` is passed, so nothing personal can end up in a public image by accident.

use crate::panel::Panel;
use crate::settings_window::SettingsWindow;
use crate::view::Latest;
use crate::worker::{self, NO_MODULE, Shared};
use gauge_core::history::History;
use gauge_core::settings::{MODULES, Settings};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBitmapImageFileType, NSBitmapImageRep, NSBox,
    NSBoxType, NSCalibratedRGBColorSpace, NSColor, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSSize, NSString};
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const COLLECT_SECONDS: u64 = 9;

fn seed(history: &mut History, now: i64, range: i64) {
    let series: [(&str, f32); 17] = [
        ("net.down", 6e6), ("net.up", 8e5), ("mem.used", 80.0), ("mem.compressed", 9.0),
        ("cpu.total", 35.0), ("gpu.device", 60.0), ("gpu.memory", 38.0), ("disk.read", 2e7),
        ("disk.write", 8e6), ("fan.0", 2600.0), ("fan.1", 2800.0), ("temp.hot", 70.0),
        ("battery.percent", 76.0), ("ping", 4.0), ("power.load", 28.0), ("power.in", 47.0), ("battery.temp", 31.0),
    ];
    let step = (range / 600).max(1);
    let mut t = now - range;
    let mut noise = 12345u32;
    while t < now {
        noise = noise.wrapping_mul(1103515245).wrapping_add(12345);
        let random = (noise >> 16) as f32 / 65535.0;
        let wave = ((t as f32 / range as f32) * 40.0).sin() * 0.3 + 0.7;
        let asleep = (t - (now - range)) * 10 / range == 6;
        if !asleep {
            for (name, level) in series {
                history.record(name, t, level * wave * (0.8 + random * 0.4));
            }
        }
        t += step;
    }
    let _ = history.flush(now + 120);
}

/// Made-up app traffic per hour for the timeline: steady browsing by day, a backup at night.
fn seed_apps(history: &mut History, now: i64) {
    let apps: [(&str, f64); 9] = [
        ("Safari", 9e8), ("Mail", 2e8), ("Music", 4e8), ("Photos", 1e8), ("Notes", 3e7), ("Messages", 6e7),
        ("cloudd", 1.5e8), ("nsurlsessiond", 8e7), ("Podcasts", 5e7),
    ];
    let mut noise = 777u32;
    for hours_ago in 0..(8 * 24) {
        let t = now - hours_ago * 3600;
        let hour_of_day = (t / 3600 + 20) % 24;
        let daytime = if (8..23).contains(&hour_of_day) { 1.0 } else { 0.08 };
        for (index, (name, level)) in apps.iter().enumerate() {
            noise = noise.wrapping_mul(1103515245).wrapping_add(12345);
            let random = f64::from((noise >> 16) % 1000) / 1000.0;
            let mut bytes = level * daytime * (0.3 + random) * (1.0 + (index as f64 + t as f64 / 20000.0).sin() * 0.4);
            if *name == "Photos" && hour_of_day == 3 {
                bytes = 3.1e9;
            }
            history.add_app_usage(t, name, (bytes * 0.9) as u64, (bytes * 0.1) as u64);
        }
    }
    let _ = history.flush_all(now);
}

const IMAGE_SCALE: f64 = 2.0;

/// Writes at double resolution, so images stay sharp on Retina screens.
fn write_png(view: &NSView, path: &str) -> Result<(), String> {
    let bounds = view.bounds();
    // SAFETY: null planes ask AppKit to allocate the buffer, and zero row and pixel sizes ask
    // it to work those out. The color space static is valid for the process's lifetime.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            (bounds.size.width * IMAGE_SCALE) as isize,
            (bounds.size.height * IMAGE_SCALE) as isize,
            8,
            4,
            true,
            false,
            NSCalibratedRGBColorSpace,
            0,
            0,
        )
    }
    .ok_or("no bitmap")?;
    rep.setSize(bounds.size);
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
    // SAFETY: an empty properties dictionary is valid for PNG output.
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
        .ok_or("no png")?;
    if data.writeToFile_atomically(&NSString::from_str(path), true) {
        Ok(())
    } else {
        Err(format!("couldn't write {path}"))
    }
}

/// Puts the view on a solid background, since a popover's material doesn't exist off screen.
fn on_background(mtm: MainThreadMarker, view: &NSView, dark: bool) -> Retained<NSBox> {
    view.layoutSubtreeIfNeeded();
    let size = view.fittingSize();
    let plate = NSBox::new(mtm);
    plate.setBoxType(NSBoxType::Custom);
    plate.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
    plate.setBorderWidth(0.0);
    plate.setContentViewMargins(NSSize::ZERO);
    let (r, g, b) = if dark { (0.13, 0.13, 0.14) } else { (0.96, 0.96, 0.96) };
    plate.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0));
    plate.setContentView(Some(view));
    // SAFETY: the appearance name statics are valid for the process's lifetime.
    let appearance = unsafe {
        NSAppearance::appearanceNamed(if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua })
    };
    plate.setAppearance(appearance.as_deref());
    plate.setFrameSize(size);
    plate.layoutSubtreeIfNeeded();
    plate
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let [kind, name, path, rest @ ..] = args else {
        return Err("usage: --render panel MODULE FILE [dark] [real] | power x FILE [dark] | network x FILE [dark] | settings PAGE FILE [dark] | gallery a,b,c FILE [dark]".into());
    };
    let dark = rest.iter().any(|a| a == "dark");
    let dummy: Retained<NSObject> = NSObject::new();
    let target: &AnyObject = dummy.as_ref();
    let mut settings = Settings::default();
    // Shows fan control in use in the images.
    "Cool".clone_into(&mut settings.active_preset);
    let settings = settings;
    match kind.as_str() {
        "gallery" => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            let modules: Vec<&str> = name.split(',').collect();
            gallery(mtm, &modules, &settings, target, now, dark, path)
        }
        "panel" => {
            let index = MODULES.iter().position(|m| m == name).ok_or("unknown module")?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            // "real" reads the saved history, so point HOME at a copy of it to leave the original alone.
            let real = rest.iter().any(|a| a == "real");
            let mut history = if real {
                History::open(&gauge_core::settings::support_dir().join("history.sqlite"))
            } else {
                History::open_in_memory()
            }
            .map_err(|e| e.to_string())?;
            let module = settings.module(name).ok_or("unknown module")?;
            // "All" has no fixed length, so show it with three days of sample history.
            let seeded = match Panel::range_seconds(module) {
                gauge_core::settings::ALL_HISTORY => 3 * 86400,
                fixed => fixed,
            };
            if !real {
                seed(&mut history, now, seeded);
            }
            let shared = Arc::new(Shared {
                history: Arc::new(Mutex::new(history)),
                settings: Arc::new(Mutex::new(settings.clone())),
                open_module: AtomicUsize::new(NO_MODULE),
                install_message: Mutex::new(None),
                sensors_open: std::sync::atomic::AtomicBool::new(false),
                network_window_open: std::sync::atomic::AtomicBool::new(false),
                names: gauge_core::names::Names::start(),
                geo: Mutex::new(None),
                geo_refresh: std::sync::atomic::AtomicBool::new(false),
            });
            let latest = if real {
                shared.open_module.store(index, std::sync::atomic::Ordering::Relaxed);
                let (sender, receiver) = mpsc::channel();
                let worker_shared = shared.clone();
                std::thread::spawn(move || worker::run(sender, worker_shared));
                std::thread::sleep(Duration::from_secs(COLLECT_SECONDS));
                let mut latest = Latest::default();
                while let Ok(update) = receiver.try_recv() {
                    worker::absorb(&mut latest, update);
                }
                latest
            } else {
                demo_latest()
            };
            let panel = Panel::new(mtm, MODULES[index], index, &settings, target);
            panel.refresh_text(&latest, &settings);
            let history = shared.history.lock().map_err(|e| e.to_string())?;
            let drawn_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(now, |d| d.as_secs() as i64);
            panel.refresh_graphs(&history, &settings, drawn_at, true);
            write_png(&on_background(mtm, panel.root_view(), dark), path)
        }
        "power" => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            let mut history = History::open_in_memory().map_err(|e| e.to_string())?;
            seed(&mut history, now, 3600);
            let window = crate::power_window::PowerWindow::new(mtm, target, 6);
            let latest = demo_latest();
            window.refresh_text(&latest, &settings);
            window.refresh_graphs(&history, &settings, now, true);
            let content = window.content().ok_or("no content")?;
            write_png(&on_background(mtm, &content, dark), path)
        }
        "network" => {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
            let mut history = History::open_in_memory().map_err(|e| e.to_string())?;
            seed(&mut history, now, 3600);
            seed_apps(&mut history, now);
            let index = MODULES.iter().position(|m| *m == "network").unwrap_or(0) as isize;
            let window = crate::network_window::NetworkWindow::new(mtm, target, index);
            let latest = demo_latest();
            let mut settings = settings;
            settings.check_host = "example.com".into();
            let names = gauge_core::names::Names::start();
            window.refresh_graphs(&history, &settings, &names, None, &latest, now, true);
            window.refresh_text(&latest, &settings);
            let content = window.content().ok_or("no content")?;
            write_png(&on_background(mtm, &content, dark), path)
        }
        "sensors" => {
            // Real readings, so this is for checking only and isn't used for public images.
            let readings = gauge_core::sensors::Sensors::open().map_err(|e| e.to_string())?.read();
            let window = crate::sensors_window::SensorsWindow::new(mtm);
            window.set_rows(&crate::view::sensor_rows(&readings, gauge_core::sensors::is_m1_mac(), &settings));
            let content = window.window.contentView().ok_or("no content")?;
            content.layoutSubtreeIfNeeded();
            write_png(&content, path)
        }
        "settings" => {
            let page: i64 = name.parse().map_err(|_| "bad page")?;
            let mut settings = settings;
            if rest.iter().any(|a| a == "many") {
                while settings.add_preset().is_some() {}
            }
            let callbacks = crate::order_view::PreviewCallbacks {
                on_order: Box::new(|_| {}),
                on_select: Box::new(|_| {}),
            };
            let window = SettingsWindow::new(mtm, target, callbacks);
            window.show_page(mtm, page, &settings, target, std::rc::Rc::new(|_, _| {}));
            window.set_preview(crate::view::preview_items(&demo_latest(), &settings), settings.item_spacing);
            let content = window.window.contentView().ok_or("no content")?;
            let plate = on_background(mtm, &content, dark);
            write_png(&plate, path)
        }
        _ => Err("unknown render kind".into()),
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

/// Made-up readings for public images, so no real addresses, apps or volume names appear.
fn demo_latest() -> Latest {
    use gauge_core::battery::{Adapter, Battery};
    use gauge_core::drives::{DiskActivity, Volume};
    use gauge_core::gpu::Gpu;
    use gauge_core::memory::{Memory, Pressure};
    use crate::net_view::{AppNow, DestinationNow, DnsServer, PathNow, TrafficNow};
    use gauge_core::network::InterfaceRate;
    use gauge_core::pathcheck::{Health as PathHealth, Step};
    use gauge_core::processor::{Cpu, TopProcess};
    use gauge_core::sensors::{AreaTemperature, Fan};
    use gauge_core::smart::{DriveHealth, Health};
    use gauge_core::wifi::Wifi;
    let process = |name: &str, cpu_percent: f32, memory_gb: u64| TopProcess {
        pid: 100,
        name: name.to_string(),
        cpu_percent,
        memory_bytes: memory_gb * GIB,
    };
    // Documentation addresses and example names only.
    let place = |address: &str, name: Option<&str>, down: f64, up: f64, rtt: f32| DestinationNow {
        address: address.parse().unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
        name: name.map(str::to_string),
        country: None,
        down,
        up,
        rtt_ms: Some(rtt),
        idle_seconds: if down + up < 1.0 { 12.0 } else { 0.0 },
        peak: (down + up).max(150_000.0),
    };
    let app = |name: &str, destinations: Vec<DestinationNow>, extra: f64| AppNow {
        name: name.to_string(),
        down: destinations.iter().map(|d| d.down).sum::<f64>() + extra,
        up: destinations.iter().map(|d| d.up).sum(),
        destinations,
    };
    let step = |ms: f32, usual: f32, health: PathHealth| Step { latest_ms: Some(ms), usual_ms: Some(usual), loss_percent: 0.0, health };
    Latest {
        interfaces: vec![InterfaceRate {
            name: "en0".into(),
            up: true,
            is_default: true,
            addresses: vec!["192.168.1.20".into()],
            bytes_in_total: 114_000_000_000,
            bytes_out_total: 25_000_000_000,
            in_per_sec: 14_000_000.0,
            out_per_sec: 2_100_000.0,
        }],
        traffic: Some(TrafficNow {
            apps: vec![
                app("Safari", vec![
                    place("203.0.113.24", Some("edge-24.cdn.example.net"), 6_200_000.0, 160_000.0, 21.0),
                    place("198.51.100.7", Some("video.example.org"), 2_600_000.0, 120_000.0, 96.0),
                ], 300_000.0),
                app("Mail", vec![place("198.51.100.30", Some("mail.example.com"), 2_400_000.0, 180_000.0, 34.0)], 0.0),
                app("Music", vec![place("203.0.113.80", Some("edge-80.cdn.example.net"), 1_200_000.0, 40_000.0, 25.0)], 0.0),
                app("Photos", vec![place("192.168.1.40", None, 640_000.0, 1_500_000.0, 3.0)], 0.0),
                app("Notes", vec![place("192.0.2.81", None, 180_000.0, 30_000.0, 42.0)], 0.0),
                app("Messages", vec![place("198.51.100.77", Some("chat.example.com"), 0.0, 0.0, 51.0)], 0.0),
                app("cloudd", vec![place("203.0.113.60", Some("sync.example.net"), 90_000.0, 420_000.0, 30.0)], 0.0),
                app("nsurlsessiond", vec![place("203.0.113.61", Some("files.example.net"), 260_000.0, 8_000.0, 28.0)], 0.0),
                app("Podcasts", vec![place("203.0.113.62", Some("media.example.org"), 120_000.0, 4_000.0, 40.0)], 0.0),
                app("apsd", vec![place("203.0.113.63", Some("push.example.com"), 2_000.0, 1_000.0, 90.0)], 0.0),
            ],
        }),
        path: Some(PathNow {
            router: step(3.2, 3.0, PathHealth::Good),
            dns: step(86.0, 18.0, PathHealth::Slow),
            internet: step(24.0, 22.0, PathHealth::Good),
            checks: 40,
            gateway: Some(std::net::Ipv4Addr::new(192, 168, 1, 1)),
            dns_servers: vec![
                DnsServer { address: "1.1.1.1".into(), provider: "Cloudflare".into(), interface: Some("en0".into()) },
                DnsServer { address: "192.168.1.1".into(), provider: "Your router".into(), interface: Some("en0".into()) },
            ],
        }),
        geo_status: None,
        wifi: Some(Wifi {
            interface: Some("en0".into()),
            ssid: None,
            rssi_dbm: -43,
            noise_dbm: -90,
            transmit_mbps: 1200.0,
            channel: Some(161),
        }),
        ping_ms: Some(3.0),
        today: (18_000_000_000, 3_200_000_000),
        month: (214_000_000_000, 41_000_000_000),
        cpu: Some(Cpu {
            total_percent: 24.0,
            per_core_percent: vec![31.0, 28.0, 44.0, 36.0, 22.0, 18.0, 15.0, 12.0, 9.0, 6.0],
            performance_cores: Some(8),
            efficiency_cores: Some(2),
            load_average: [3.2, 2.8, 2.4],
            uptime_seconds: 3 * 86400 + 5 * 3600,
        }),
        processes: vec![
            process("Safari", 18.0, 4),
            process("Xcode", 12.0, 6),
            process("Photos", 7.0, 2),
            process("Music", 3.0, 1),
        ],
        gpu: Some(Gpu {
            model: Some("Apple M1 Max".into()),
            core_count: Some(32),
            device_percent: Some(38),
            renderer_percent: Some(35),
            tiler_percent: Some(30),
            memory_in_use_bytes: Some(9 * GIB as i64),
        }),
        memory: Some(Memory {
            total_bytes: 64 * GIB,
            used_bytes: 42 * GIB,
            app_bytes: 26 * GIB,
            wired_bytes: 12 * GIB,
            compressed_bytes: 4 * GIB,
            cached_files_bytes: 14 * GIB,
            free_bytes: 8 * GIB,
            swap_total_bytes: GIB,
            swap_used_bytes: 200 * 1024 * 1024,
            pressure: Pressure::Normal,
        }),
        volumes: vec![
            Volume {
                mount_point: "/".into(),
                filesystem: "apfs".into(),
                is_network: false,
                total_bytes: 8_000_000_000_000,
                available_bytes: 7_800_000_000_000,
            },
            Volume {
                mount_point: "/Volumes/Backup".into(),
                filesystem: "apfs".into(),
                is_network: false,
                total_bytes: 4_000_000_000_000,
                available_bytes: 2_600_000_000_000,
            },
            Volume {
                mount_point: "/Volumes/Media".into(),
                filesystem: "smbfs".into(),
                is_network: true,
                total_bytes: 12_000_000_000_000,
                available_bytes: 5_100_000_000_000,
            },
        ],
        disks: vec![crate::view::DiskRate {
            disk: DiskActivity {
                bsd_name: "disk0".into(),
                model: Some("APPLE SSD".into()),
                location: Some("Internal".into()),
                bytes_read: 0,
                bytes_written: 0,
            },
            read_per_sec: 42_000_000.0,
            write_per_sec: 18_000_000.0,
        }],
        health: vec![DriveHealth {
            bsd_name: Some("disk0".into()),
            model: Some("APPLE SSD".into()),
            health: Health {
                critical_warning: 0,
                temperature_c: 36,
                available_spare_percent: 100,
                percent_used: 3,
                bytes_read: 200_000_000_000_000,
                bytes_written: 120_000_000_000_000,
                power_on_hours: 6000,
                power_cycles: 400,
                unsafe_shutdowns: 10,
                media_errors: 0,
            },
        }],
        fans: vec![
            Fan { index: 0, mode: 1, target_rpm: 3100.0, actual_rpm: 3120.0, min_rpm: 1500.0, max_rpm: 5350.0 },
            Fan { index: 1, mode: 1, target_rpm: 3350.0, actual_rpm: 3340.0, min_rpm: 1500.0, max_rpm: 5780.0 },
        ],
        hottest: Some(("Tp0c".into(), 71.0)),
        battery: Some(Battery {
            percent: Some(76),
            health_percent: Some(84.0),
            cycle_count: Some(240),
            voltage_mv: Some(12_400),
            amperage_ma: Some(1500),
            power_watts: Some(18.6),
            temperature_c: Some(31.0),
            charging: Some(true),
            external_power: Some(true),
            fully_charged: Some(false),
            minutes_remaining: None,
            minutes_to_full: Some(75),
            design_cycles: Some(1000),
            nominal_mah: Some(7300),
            design_mah: Some(8700),
            remaining_mah: Some(5800),
            adapter: Some(Adapter {
                name: Some("96W USB-C Power Adapter".into()),
                manufacturer: Some("Apple Inc.".into()),
                rated_watts: Some(94),
                volts: Some(20.0),
                amps: Some(4.69),
            }),
            input_watts: Some(47.0),
            input_volts: Some(19.96),
            input_amps: Some(2.35),
            load_watts: Some(28.0),
            adapter_loss_watts: Some(1.6),
            usb_out_watts: Some(1.9),
        }),
        fan_state: crate::fan_control::FanState::Holding,
        install_message: None,
        sensors: Vec::new(),
        device_batteries: vec![
            gauge_core::battery::DeviceBattery { name: "AirPods Pro".into(), percent: 80, charging: Some(true) },
            gauge_core::battery::DeviceBattery { name: "AirPods Pro Case".into(), percent: 62, charging: Some(false) },
        ],
        usb_devices: vec![gauge_core::battery::UsbDevice { name: "iPhone".into(), allocated_watts: 12.0 }],
        energy_today_wh: 214.0,
        power: Some(gauge_core::battery::PowerSample {
            adapter_watts: 47.0,
            battery_watts: 19.0,
            system_watts: 28.0,
            display_watts: 8.0,
        }),
        charge_limit: Some(Some(80)),
        charge_error: None,
        temperatures: vec![
            AreaTemperature { area: "CPU", average_c: 58.0, peak_c: 71.0 },
            AreaTemperature { area: "GPU", average_c: 52.0, peak_c: 60.0 },
            AreaTemperature { area: "Memory", average_c: 47.0, peak_c: 50.0 },
            AreaTemperature { area: "SSD", average_c: 36.0, peak_c: 38.0 },
            AreaTemperature { area: "Battery", average_c: 31.0, peak_c: 32.0 },
        ],
    }
}

/// Sample panels side by side, for the top of the README.
fn gallery(mtm: MainThreadMarker, modules: &[&str], settings: &Settings, target: &AnyObject, now: i64, dark: bool, path: &str) -> Result<(), String> {
    let mut history = History::open_in_memory().map_err(|e| e.to_string())?;
    seed(&mut history, now, 3600);
    let latest = demo_latest();
    let row = crate::ui::horizontal_stack(mtm, 18.0);
    row.setAlignment(objc2_app_kit::NSLayoutAttribute::Top);
    let mut panels = Vec::new();
    for name in modules {
        let index = MODULES.iter().position(|m| m == name).ok_or("unknown module")?;
        let panel = Panel::new(mtm, MODULES[index], index, settings, target);
        panel.refresh_text(&latest, settings);
        panel.refresh_graphs(&history, settings, now, true);
        row.addArrangedSubview(panel.root_view());
        panels.push(panel);
    }
    write_png(&on_background(mtm, &row, dark), path)
}
