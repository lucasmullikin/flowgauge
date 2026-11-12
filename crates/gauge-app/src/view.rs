//! Turns readings into what the menu bar and panels show. Pure, so all of it is testable.

use crate::fan_control::FanState;
use crate::graph_math::Unit;
use gauge_core::battery::Battery;
use gauge_core::drives::{DiskActivity, Volume};
use gauge_core::gpu::Gpu;
use gauge_core::memory::{Memory, Pressure};
use gauge_core::network::InterfaceRate;
use gauge_core::processor::{Cpu, TopProcess, top_by};
use gauge_core::sensors::Fan;
use gauge_core::settings::{Settings, TemperatureUnit};
use gauge_core::smart::DriveHealth;
use gauge_core::wifi::{Wifi, quality_label, snr};

const HIDDEN_INTERFACE_PREFIXES: [&str; 8] = ["lo", "awdl", "llw", "anpi", "anri", "ap", "gif", "stf"];
const MAX_OTHER_CONNECTIONS: usize = 3;
const TOP_ROWS: usize = 5;
const MANUAL_MODE: u8 = 1;

pub struct DiskRate {
    pub disk: DiskActivity,
    pub read_per_sec: f64,
    pub write_per_sec: f64,
}

#[derive(Default)]
pub struct Latest {
    pub interfaces: Vec<InterfaceRate>,
    pub traffic: Option<crate::net_view::TrafficNow>,
    pub path: Option<crate::net_view::PathNow>,
    pub geo_status: Option<String>,
    pub wifi: Option<Wifi>,
    pub ping_ms: Option<f32>,
    pub today: (u64, u64),
    pub month: (u64, u64),
    pub cpu: Option<Cpu>,
    pub processes: Vec<TopProcess>,
    pub gpu: Option<Gpu>,
    pub memory: Option<Memory>,
    pub volumes: Vec<Volume>,
    pub disks: Vec<DiskRate>,
    pub health: Vec<DriveHealth>,
    pub fans: Vec<Fan>,
    pub hottest: Option<(String, f32)>,
    pub battery: Option<Battery>,
    pub fan_state: FanState,
    pub install_message: Option<String>,
    pub sensors: Vec<gauge_core::sensors::Reading>,
    pub temperatures: Vec<gauge_core::sensors::AreaTemperature>,
    pub device_batteries: Vec<gauge_core::battery::DeviceBattery>,
    /// Devices drawing power from this Mac's USB ports, like a charging iPhone.
    pub usb_devices: Vec<gauge_core::battery::UsbDevice>,
    pub energy_today_wh: f64,
    pub power: Option<gauge_core::battery::PowerSample>,
    /// What macOS is enforcing. Outer `None` means not read yet or not understood.
    pub charge_limit: Option<Option<u8>>,
    pub charge_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub adapter: f64,
    pub battery: f64,
    /// Label and watts, largest last so "Other" sits at the bottom. Zero outputs are left out.
    pub outputs: Vec<(String, f64)>,
}

impl Flow {
    pub fn total(&self) -> f64 {
        self.adapter + self.battery
    }
}

const FLOW_MIN_WATTS: f64 = 0.05;

/// Splits what's coming in by where it goes. Only readings the hardware reports get their own path.
/// Everything else, including the SoC, is the remainder, so the two sides always add up.
pub fn power_flow(latest: &Latest) -> Option<Flow> {
    let power = latest.power?;
    let battery = latest.battery.as_ref();
    let unplugged = battery.is_some_and(|b| b.external_power != Some(true));
    let draining = unplugged || battery.and_then(|b| b.amperage_ma).is_some_and(|a| a < 0);
    let (from_battery, into_battery) = if draining { (power.battery_watts, 0.0) } else { (0.0, power.battery_watts) };
    let adapter = if unplugged { 0.0 } else { power.adapter_watts };
    let total = adapter + from_battery;
    let mut outputs: Vec<(String, f64)> = Vec::new();
    let mut remaining = total;
    let usb = battery.and_then(|b| b.usb_out_watts).unwrap_or(0.0);
    let usb_label = usb_label(&latest.usb_devices);
    for (label, watts) in [("Charging", into_battery), ("Display", power.display_watts), (usb_label.as_str(), usb)] {
        let watts = watts.min(remaining);
        if watts >= FLOW_MIN_WATTS {
            outputs.push((label.to_string(), watts));
            remaining -= watts;
        }
    }
    if remaining >= FLOW_MIN_WATTS {
        outputs.push(("Other".to_string(), remaining));
    }
    Some(Flow { adapter, battery: from_battery, outputs })
}

/// What the Mac is powering over USB: the device's name when there's one, like "iPhone".
pub fn usb_label(devices: &[gauge_core::battery::UsbDevice]) -> String {
    match devices {
        [] => "USB".to_string(),
        [one] => one.name.clone(),
        [first, rest @ ..] => format!("{} + {}", first.name, rest.len()),
    }
}

/// A row saying which devices charge from the Mac, and how much they're drawing.
fn usb_row(latest: &Latest) -> Option<Row> {
    let watts = latest.battery.as_ref().and_then(|b| b.usb_out_watts).filter(|w| *w >= FLOW_MIN_WATTS);
    if latest.usb_devices.is_empty() && watts.is_none() {
        return None;
    }
    let allowed: f64 = latest.usb_devices.iter().map(|d| d.allocated_watts).sum();
    let value = match (watts, allowed > 0.0) {
        (Some(w), true) => format!("{} of up to {}", format_watts(w), format_watts(allowed)),
        (Some(w), false) => format_watts(w),
        (None, _) => "Connected, not drawing".to_string(),
    };
    let label = if latest.usb_devices.is_empty() { "Powering USB devices".to_string() } else { format!("Powering {}", usb_label(&latest.usb_devices)) };
    Some(row(label, value))
}

/// The label beside the charge limit slider.
pub fn charge_limit_label(desired: Option<u8>, actual: Option<Option<u8>>, top_up: bool) -> String {
    if top_up {
        return "Topping up to 100%".to_string();
    }
    match (desired, actual) {
        (Some(100), _) => "No charge limit".to_string(),
        (Some(limit), _) => format!("Limit: {limit}%"),
        (None, Some(Some(limit))) => format!("Limit: {limit}%"),
        (None, Some(None)) => "No charge limit".to_string(),
        (None, None) => "Limit unknown".to_string(),
    }
}

pub fn slider_position(desired: Option<u8>, actual: Option<Option<u8>>) -> f64 {
    f64::from(desired.or(actual.flatten()).unwrap_or(100))
}

/// A short list for the panel; the Power window has everything.
pub fn power_summary(latest: &Latest, settings: &Settings) -> Vec<Row> {
    let mut rows = Vec::new();
    let Some(b) = &latest.battery else { return rows };
    let system = latest.power.map(|p| p.system_watts).filter(|w| *w > 0.0).or(b.load_watts);
    rows.push(row("Using", system.map_or_else(|| MISSING.to_string(), format_watts)));
    rows.push(row("Used today", format_energy(latest.energy_today_wh)));
    if let Some(adapter) = &b.adapter {
        let name = adapter.name.clone().unwrap_or_else(|| "Power adapter".to_string());
        let input = latest.power.map(|p| p.adapter_watts).or(b.input_watts);
        let load = gauge_core::battery::adapter_load_percent(input, adapter.rated_watts);
        rows.push(row(name, load.map_or_else(|| MISSING.to_string(), |l| format!("{l:.0}% of its limit"))));
    }
    if let Some(percent) = b.percent {
        let health = b.health_percent.map_or_else(String::new, |h| format!(", {h:.0}% health"));
        rows.push(row("Battery", format!("{percent}%{health}")));
    }
    if let Some(minutes) = b.minutes_to_full.filter(|m| *m > 0 && b.charging == Some(true)) {
        rows.push(row("Time to full", format_duration(minutes)));
    }
    if let Some(minutes) = b.minutes_remaining.filter(|_| b.external_power != Some(true)) {
        rows.push(row("Time left", format_duration(minutes)));
    }
    if let Some(error) = &latest.charge_error {
        rows.push(row("Charge limit", error.clone()));
    }
    rows.extend(usb_row(latest));
    rows.extend(device_rows(latest));
    let _ = settings;
    rows
}

fn device_rows(latest: &Latest) -> Vec<Row> {
    let mut rows = Vec::new();
    if !latest.device_batteries.is_empty() {
        rows.push(header("Devices"));
        for device in &latest.device_batteries {
            let state = if device.charging == Some(true) { ", charging" } else { "" };
            rows.push(row(device.name.clone(), format!("{}%{state}", device.percent)));
        }
    }
    rows
}

/// The three cards in the Power window: battery now, battery health, power adapter.
pub fn power_cards(latest: &Latest, settings: &Settings) -> [(&'static str, Vec<Row>); 3] {
    let mut now = Vec::new();
    let mut health = Vec::new();
    let mut adapter_rows = Vec::new();
    if let Some(b) = &latest.battery {
        if let Some(percent) = b.percent {
            now.push(row("Charge", format!("{percent}%")));
        }
        if let (Some(volts), Some(amps)) = (b.voltage_mv, b.amperage_ma) {
            now.push(row("Voltage", format!("{:.2} V", volts as f64 / 1000.0)));
            now.push(row("Current", format!("{:.2} A", amps as f64 / 1000.0)));
        }
        if let Some(t) = b.temperature_c {
            now.push(row("Temperature", format_temperature(t, settings.temperature_unit)));
        }
        if let Some(mah) = b.remaining_mah {
            now.push(row("Stored", format!("{mah} mAh")));
        }
        if let Some(h) = b.health_percent {
            health.push(row("Maximum capacity", format!("{h:.0}%")));
        }
        if let (Some(nominal), Some(design)) = (b.nominal_mah, b.design_mah) {
            health.push(row("Now", format!("{nominal} mAh")));
            health.push(row("When new", format!("{design} mAh")));
        }
        if let Some(cycles) = b.cycle_count {
            let of = b.design_cycles.map_or_else(String::new, |d| format!(" of {d}"));
            health.push(row("Cycles", format!("{cycles}{of}")));
        }
        match &b.adapter {
            Some(adapter) => {
                if let Some(name) = &adapter.name {
                    adapter_rows.push(row("Model", name.trim_end_matches(" Power Adapter").to_string()));
                }
                if let Some(rated) = adapter.rated_watts {
                    adapter_rows.push(row("Rated", format!("{rated} W")));
                }
                let input = latest.power.map(|p| p.adapter_watts).or(b.input_watts);
                if let Some(input) = input {
                    adapter_rows.push(row("Incoming", format_watts(input)));
                }
                if let (Some(volts), Some(amps)) = (b.input_volts, b.input_amps) {
                    adapter_rows.push(row("Supply", format!("{volts:.1} V, {amps:.2} A")));
                }
                if let Some(load) = gauge_core::battery::adapter_load_percent(input, adapter.rated_watts) {
                    adapter_rows.push(row("In use", format!("{load:.0}%")));
                }
            }
            None => adapter_rows.push(row("Status", "Not connected")),
        }
    }
    adapter_rows.extend(usb_row(latest));
    [("Battery", now), ("Health", health), ("Power adapter", adapter_rows)]
}

/// Every sensor by kind, hottest first. Most voltage, current and power channels read zero, so
/// those are left out to keep the list useful.
pub fn sensor_rows(readings: &[gauge_core::sensors::Reading], is_m1: bool, settings: &Settings) -> Vec<Row> {
    use gauge_core::sensors::{Kind, sensor_name};
    let sections = [
        (Kind::Temperature, "Temperature"),
        (Kind::Power, "Power"),
        (Kind::Voltage, "Voltage"),
        (Kind::Current, "Current"),
    ];
    let mut rows = Vec::new();
    for (kind, title) in sections {
        let mut group: Vec<_> = readings
            .iter()
            .filter(|r| r.kind == kind && (kind == Kind::Temperature || r.value.abs() >= 0.01))
            .collect();
        group.sort_by(|a, b| b.value.total_cmp(&a.value));
        if group.is_empty() {
            continue;
        }
        rows.push(header(title));
        for reading in group {
            let value = match kind {
                Kind::Temperature => format_temperature(reading.value, settings.temperature_unit),
                Kind::Power => format!("{:.2} W", reading.value),
                Kind::Voltage => format!("{:.2} V", reading.value),
                Kind::Current => format!("{:.2} A", reading.value),
            };
            rows.push(row(sensor_name(&reading.key, is_m1), value));
        }
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Primary,
    Secondary,
}

pub struct GraphSpec {
    /// Says what a graph shows when a panel has more than one.
    pub caption: Option<&'static str>,
    pub series: Vec<(&'static str, Slot)>,
    pub unit: Unit,
    pub height: f64,
}

pub fn graphs(module: &str, settings: &Settings) -> Vec<GraphSpec> {
    let graph = |series, unit| GraphSpec { caption: None, series, unit, height: 110.0 };
    match module {
        "network" => vec![graph(vec![("net.down", Slot::Primary), ("net.up", Slot::Secondary)], Unit::Rate)],
        "memory" => vec![graph(vec![("mem.used", Slot::Primary), ("mem.compressed", Slot::Secondary)], Unit::Percent)],
        "cpu" => vec![graph(vec![("cpu.total", Slot::Primary)], Unit::Percent)],
        "gpu" => vec![graph(vec![("gpu.device", Slot::Primary), ("gpu.memory", Slot::Secondary)], Unit::Percent)],
        "drives" => vec![graph(vec![("disk.read", Slot::Primary), ("disk.write", Slot::Secondary)], Unit::Rate)],
        "fans" => {
            let temperature = match settings.temperature_unit {
                TemperatureUnit::Celsius => Unit::Celsius,
                TemperatureUnit::Fahrenheit => Unit::Fahrenheit,
            };
            vec![
                graph(vec![("fan.0", Slot::Primary), ("fan.1", Slot::Secondary)], Unit::Rpm),
                GraphSpec { caption: Some("Hottest sensor"), series: vec![("temp.hot", Slot::Secondary)], unit: temperature, height: 72.0 },
            ]
        }
        "battery" => vec![graph(vec![("power.load", Slot::Primary), ("power.in", Slot::Secondary)], Unit::Watts)],
        _ => Vec::new(),
    }
}

pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut value = bytes.max(0.0);
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{value:.0} {}", UNITS[unit]),
        _ if value < 10.0 => format!("{value:.1} {}", UNITS[unit]),
        _ => format!("{value:.0} {}", UNITS[unit]),
    }
}

/// RAM is sold and shown in binary units, so a 64 GB Mac should read as 64 GB, not 69.
pub fn format_memory(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        let gigabytes = value / GIB;
        if gigabytes < 10.0 {
            format!("{gigabytes:.1} GB")
        } else {
            format!("{gigabytes:.0} GB")
        }
    } else {
        format!("{:.0} MB", value / MIB)
    }
}

pub fn format_watts(watts: f64) -> String {
    if watts.abs() < 10.0 { format!("{watts:.1} W") } else { format!("{watts:.0} W") }
}

pub fn format_energy(watt_hours: f64) -> String {
    if watt_hours >= 1000.0 { format!("{:.2} kWh", watt_hours / 1000.0) } else { format!("{watt_hours:.0} Wh") }
}

fn format_duration(minutes: i64) -> String {
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

/// The same text with every number at its widest (`888`) and every byte unit as `MB`, so a
/// width measured from it fits any value.
pub fn widest_title(title: &str) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut out = String::with_capacity(title.len() + 8);
    let chars: Vec<char> = title.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            out.push_str("888");
        } else if c.is_ascii_uppercase() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            out.push_str(if UNITS.contains(&word.as_str()) { "MB" } else { &word });
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

pub fn format_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

pub fn format_temperature(celsius: f32, unit: TemperatureUnit) -> String {
    match unit {
        TemperatureUnit::Celsius => format!("{celsius:.0}°C"),
        TemperatureUnit::Fahrenheit => format!("{:.0}°F", celsius * 9.0 / 5.0 + 32.0),
    }
}

pub fn format_value(value: f32, unit: Unit) -> String {
    match unit {
        Unit::Rate => format_rate(f64::from(value)),
        Unit::Percent => format!("{value:.0}%"),
        Unit::Rpm => format!("{value:.0} rpm"),
        Unit::Celsius => format!("{value:.0}°C"),
        Unit::Fahrenheit => format!("{value:.0}°F"),
        Unit::Watts => format!("{value:.0} W"),
    }
}

pub fn pressure_label(pressure: Pressure) -> &'static str {
    match pressure {
        Pressure::Normal => "Normal",
        Pressure::Warning => "Warning",
        Pressure::Critical => "Critical",
        Pressure::Unknown => "Unknown",
    }
}

pub fn memory_percent(memory: &Memory) -> f64 {
    if memory.total_bytes == 0 {
        return 0.0;
    }
    memory.used_bytes as f64 / memory.total_bytes as f64 * 100.0
}

pub struct Token {
    pub label: &'static str,
    pub name: &'static str,
}

const fn token(label: &'static str, name: &'static str) -> Token {
    Token { label, name }
}

const NETWORK_TOKENS: &[Token] = &[
    token("Upload speed", "up"),
    token("Download speed", "down"),
    token("Download today", "today_down"),
    token("Upload today", "today_up"),
    token("Gateway ping", "ping"),
    token("Wi-Fi signal", "signal"),
];
const MEMORY_TOKENS: &[Token] = &[
    token("Percent used", "percent"),
    token("Amount used", "used"),
    token("Free", "free"),
    token("Swap used", "swap"),
    token("Pressure", "pressure"),
];
const CPU_TOKENS: &[Token] = &[
    token("Percent used", "percent"),
    token("Load average", "load"),
];
const GPU_TOKENS: &[Token] = &[
    token("Percent used", "percent"),
    token("Memory", "memory"),
];
const DRIVE_TOKENS: &[Token] = &[
    token("Free space", "free"),
    token("Percent used", "percent"),
    token("Read speed", "read"),
    token("Write speed", "write"),
];
const FAN_TOKENS: &[Token] = &[
    token("Fastest fan, rpm", "rpm"),
    token("Hottest sensor", "temp"),
    token("Fan 1, rpm", "fan1"),
    token("Fan 2, rpm", "fan2"),
    token("Preset", "preset"),
];
const BATTERY_TOKENS: &[Token] = &[
    token("Power use", "load"),
    token("Incoming power", "input"),
    token("Battery charge", "percent"),
    token("Battery power", "power"),
    token("Battery health", "health"),
    token("Energy used today", "today"),
];

pub fn tokens(module: &str) -> &'static [Token] {
    match module {
        "network" => NETWORK_TOKENS,
        "memory" => MEMORY_TOKENS,
        "cpu" => CPU_TOKENS,
        "gpu" => GPU_TOKENS,
        "drives" => DRIVE_TOKENS,
        "fans" => FAN_TOKENS,
        "battery" => BATTERY_TOKENS,
        _ => &[],
    }
}

pub const ICONS: [&str; 20] = [
    "↑", "↓", "⇡", "⇣", "▲", "▼", "●", "◆", "■", "▪︎", "⚡︎", "♨︎", "☀︎", "⌘", "∿", "≋", "🌡", "💾", "🌐", "🌀",
];

pub fn default_template(module: &str) -> &'static str {
    match module {
        "network" => "↑{up} ↓{down}",
        "memory" => "RAM {percent}",
        "cpu" => "CPU {percent}",
        "gpu" => "GPU {percent}",
        "drives" => "{free} free",
        "fans" => "{rpm} rpm {temp}",
        "battery" => "⚡︎{load}",
        _ => "",
    }
}

pub fn template_for<'a>(module: &str, settings: &'a Settings) -> &'a str {
    settings
        .module(module)
        .and_then(|m| m.bar_template.as_deref())
        .unwrap_or_else(|| default_template(module))
}

/// Swaps each `{name}` for its value. Anything unrecognized stays as typed, so a typo is visible.
pub fn render_template(template: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                match value(&after[..close]) {
                    Some(text) => out.push_str(&text),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

const MISSING: &str = "–";

fn token_value(module: &str, name: &str, latest: &Latest, settings: &Settings) -> Option<String> {
    let or_missing = |value: Option<String>| Some(value.unwrap_or_else(|| MISSING.to_string()));
    let default = latest.interfaces.iter().find(|i| i.is_default);
    let root = latest.volumes.iter().find(|v| v.mount_point == "/");
    let total_rate = |pick: fn(&DiskRate) -> f64| format_rate(latest.disks.iter().map(pick).sum());
    let fan_rpm = |index: usize| latest.fans.get(index).map(|f| format!("{:.0}", (f.actual_rpm / 10.0).round() * 10.0));
    match (module, name) {
        ("network", "up") => or_missing(default.map(|i| format_rate(i.out_per_sec))),
        ("network", "down") => or_missing(default.map(|i| format_rate(i.in_per_sec))),
        ("network", "today_down") => Some(format_bytes(latest.today.0 as f64)),
        ("network", "today_up") => Some(format_bytes(latest.today.1 as f64)),
        ("network", "ping") => or_missing(latest.ping_ms.map(|ms| format!("{ms:.0} ms"))),
        ("network", "signal") => or_missing(latest.wifi.as_ref().map(|w| format!("{} dBm", w.rssi_dbm))),
        ("memory", "percent") => or_missing(latest.memory.as_ref().map(|m| format!("{:.0}%", memory_percent(m)))),
        ("memory", "used") => or_missing(latest.memory.as_ref().map(|m| format_memory(m.used_bytes))),
        ("memory", "free") => or_missing(latest.memory.as_ref().map(|m| format_memory(m.free_bytes))),
        ("memory", "swap") => or_missing(latest.memory.as_ref().map(|m| format_memory(m.swap_used_bytes))),
        ("memory", "pressure") => or_missing(latest.memory.as_ref().map(|m| pressure_label(m.pressure).to_string())),
        ("cpu", "percent") => or_missing(latest.cpu.as_ref().map(|c| format!("{:.0}%", c.total_percent))),
        ("cpu", "load") => or_missing(latest.cpu.as_ref().map(|c| format!("{:.2}", c.load_average[0]))),
        ("gpu", "percent") => or_missing(latest.gpu.as_ref().and_then(|g| g.device_percent).map(|p| format!("{p}%"))),
        ("gpu", "memory") => or_missing(latest.gpu.as_ref().and_then(|g| g.memory_in_use_bytes).map(|b| format_memory(b as u64))),
        ("drives", "free") => or_missing(root.map(|v| format_bytes(v.available_bytes as f64))),
        ("drives", "percent") => or_missing(root.map(|v| {
            let used = v.total_bytes.saturating_sub(v.available_bytes) as f64;
            format!("{:.0}%", used / v.total_bytes.max(1) as f64 * 100.0)
        })),
        ("drives", "read") => Some(total_rate(|d| d.read_per_sec)),
        ("drives", "write") => Some(total_rate(|d| d.write_per_sec)),
        ("fans", "rpm") => {
            // Rounded to tens: fans jitter by a few rpm, and every visible change costs a redraw.
            let fastest = latest.fans.iter().map(|f| f.actual_rpm).fold(0.0, f32::max);
            Some(format!("{:.0}", (fastest / 10.0).round() * 10.0))
        }
        ("fans", "temp") => or_missing(latest.hottest.as_ref().map(|(_, c)| format_temperature(*c, settings.temperature_unit))),
        ("fans", "fan1") => or_missing(fan_rpm(0)),
        ("fans", "fan2") => or_missing(fan_rpm(1)),
        ("fans", "preset") => Some(settings.active_preset.clone()),
        ("battery", "load") => or_missing(
            latest
                .power
                .map(|p| p.system_watts)
                .filter(|w| *w > 0.0)
                .or(latest.battery.as_ref().and_then(|b| b.load_watts))
                .map(format_watts),
        ),
        ("battery", "input") => or_missing(
            latest
                .power
                .map(|p| p.adapter_watts)
                .or(latest.battery.as_ref().and_then(|b| b.input_watts))
                .map(format_watts),
        ),
        ("battery", "today") => Some(format_energy(latest.energy_today_wh)),
        ("battery", "percent") => or_missing(latest.battery.as_ref().and_then(|b| b.percent).map(|p| format!("{p}%"))),
        ("battery", "power") => or_missing(latest.battery.as_ref().and_then(|b| b.power_watts).map(|w| format!("{:.1} W", w.abs()))),
        ("battery", "health") => or_missing(latest.battery.as_ref().and_then(|b| b.health_percent).map(|h| format!("{h:.0}%"))),
        _ => None,
    }
}

pub fn bar_title(module: &str, latest: &Latest, settings: &Settings) -> String {
    render_template(template_for(module, settings), |name| token_value(module, name, latest, settings))
}

/// The menu bar as it stands, leftmost first, with hidden items included so they can be moved.
pub fn preview_items(latest: &Latest, settings: &Settings) -> Vec<crate::order_view::BarItem> {
    settings
        .order
        .iter()
        .map(|name| crate::order_view::BarItem {
            name: name.clone(),
            title: bar_title(name, latest, settings),
            shown: settings.module(name).is_some_and(|m| m.shown),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub value: String,
    pub header: bool,
}

fn row(key: impl Into<String>, value: impl Into<String>) -> Row {
    Row { key: key.into(), value: value.into(), header: false }
}

fn header(key: impl Into<String>) -> Row {
    Row { key: key.into(), value: String::new(), header: true }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Content {
    pub title: String,
    pub subtitle: String,
    /// Each legend entry's text, matched in order to the first graph's series.
    pub legend: Vec<String>,
    pub rows: Vec<Row>,
}

fn is_shown(interface: &InterfaceRate) -> bool {
    !interface.is_default
        && interface.up
        && !interface.addresses.is_empty()
        && interface.in_per_sec + interface.out_per_sec > 0.0
        && !HIDDEN_INTERFACE_PREFIXES.iter().any(|p| interface.name.starts_with(p))
}

fn first_routable(interface: &InterfaceRate) -> Option<&String> {
    let ipv4 = interface.addresses.iter().find(|a| a.contains('.'));
    ipv4.or_else(|| interface.addresses.iter().find(|a| !a.starts_with("fe80:")))
}

const PANEL_CONNECTIONS: usize = 6;
const CONNECTION_CHARS: usize = 34;

pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end())
}

fn network_content(latest: &Latest, settings: &Settings) -> Content {
    let default = latest.interfaces.iter().find(|i| i.is_default);
    let mut content = Content {
        title: "Network".into(),
        subtitle: default.map_or_else(|| "Not connected".to_string(), |i| i.name.clone()),
        legend: match default {
            Some(i) => vec![format!("Down {}", format_rate(i.in_per_sec)), format!("Up {}", format_rate(i.out_per_sec))],
            None => vec!["Down".into(), "Up".into()],
        },
        rows: Vec::new(),
    };
    let rows = &mut content.rows;
    let totals = |(down, up): (u64, u64)| format!("↓{}  ↑{}", format_bytes(down as f64), format_bytes(up as f64));
    rows.push(row("Today", totals(latest.today)));
    rows.push(row("This month", totals(latest.month)));
    if let Some(i) = default {
        rows.push(row("Since boot", totals((i.bytes_in_total, i.bytes_out_total))));
        if let Some(address) = first_routable(i) {
            rows.push(row("Address", address.clone()));
        }
    }
    rows.push(row(
        "Gateway ping",
        latest.ping_ms.map_or_else(|| "No reply".to_string(), |ms| format!("{ms:.0} ms")),
    ));
    if let Some(path) = &latest.path {
        if let Some(gateway) = path.gateway {
            rows.push(row("Router", gateway.to_string()));
        }
        if let Some(first) = path.dns_servers.first() {
            rows.push(row("DNS server", format!("{}, {}", first.provider, first.address)));
            let via = first.interface.as_deref().filter(|i| i.starts_with("utun") || i.starts_with("ipsec"));
            if let Some(next) = path.dns_servers.iter().find(|s| s.provider != first.provider) {
                let note = if via.is_some() { "Overrides" } else { "Then" };
                rows.push(row(note, format!("{}, {}", next.provider, next.address)));
            }
        }
    }
    if settings.show_wifi && let Some(wifi) = &latest.wifi {
        if settings.show_wifi_name {
            rows.push(row("Wi-Fi network", wifi.ssid.clone().unwrap_or_else(|| "Needs Location permission".to_string())));
        }
        let quality = quality_label(snr(wifi.rssi_dbm, wifi.noise_dbm));
        rows.push(row("Wi-Fi signal", format!("{} dBm, {quality}", wifi.rssi_dbm)));
        rows.push(row("Wi-Fi speed", format!("{:.0} Mb/s", wifi.transmit_mbps)));
        if let Some(channel) = wifi.channel {
            rows.push(row("Wi-Fi channel", channel.to_string()));
        }
    }
    if let Some(status) = &latest.geo_status {
        rows.push(row("Locations", status.clone()));
    }
    if let Some(traffic) = &latest.traffic {
        let recent = crate::net_view::recent_connections(traffic, PANEL_CONNECTIONS);
        if !recent.is_empty() {
            rows.push(header("Recent connections"));
            for (app, place, value) in recent {
                rows.push(row(shorten(&format!("{app}: {place}"), CONNECTION_CHARS), value));
            }
        }
    }
    let mut others: Vec<&InterfaceRate> = latest.interfaces.iter().filter(|i| is_shown(i)).collect();
    others.sort_by(|a, b| (b.in_per_sec + b.out_per_sec).total_cmp(&(a.in_per_sec + a.out_per_sec)));
    others.truncate(MAX_OTHER_CONNECTIONS);
    if !others.is_empty() {
        rows.push(header("Other connections"));
        for i in others {
            rows.push(row(i.name.clone(), format!("↓{}  ↑{}", format_rate(i.in_per_sec), format_rate(i.out_per_sec))));
        }
    }
    content
}

fn memory_content(latest: &Latest) -> Content {
    let Some(m) = &latest.memory else {
        return Content { title: "Memory".into(), ..Content::default() };
    };
    let gigabytes = format_memory;
    let mut rows = vec![
        row("Pressure", pressure_label(m.pressure)),
        row("App memory", gigabytes(m.app_bytes)),
        row("Wired", gigabytes(m.wired_bytes)),
        row("Compressed", gigabytes(m.compressed_bytes)),
        row("Cached files", gigabytes(m.cached_files_bytes)),
        row("Free", gigabytes(m.free_bytes)),
        row("Swap used", format!("{} of {}", gigabytes(m.swap_used_bytes), gigabytes(m.swap_total_bytes))),
    ];
    if !latest.processes.is_empty() {
        rows.push(header("Top apps"));
        for p in top_by(latest.processes.clone(), TOP_ROWS, |p| p.memory_bytes) {
            rows.push(row(p.name, format_memory(p.memory_bytes)));
        }
    }
    Content {
        title: "Memory".into(),
        subtitle: format!("{} of {}", gigabytes(m.used_bytes), gigabytes(m.total_bytes)),
        legend: vec![
            format!("Used {:.0}%", memory_percent(m)),
            format!("Compressed {:.0}%", m.compressed_bytes as f64 / m.total_bytes.max(1) as f64 * 100.0),
        ],
        rows,
    }
}

fn cpu_content(latest: &Latest) -> Content {
    let Some(c) = &latest.cpu else {
        return Content { title: "CPU".into(), ..Content::default() };
    };
    let mut rows = vec![
        row("Load average", format!("{:.2}  {:.2}  {:.2}", c.load_average[0], c.load_average[1], c.load_average[2])),
        row("Uptime", format!("{}d {}h", c.uptime_seconds / 86400, c.uptime_seconds % 86400 / 3600)),
    ];
    if let (Some(p), Some(e)) = (c.performance_cores, c.efficiency_cores) {
        rows.push(row("Cores", format!("{p} performance, {e} efficiency")));
    }
    let cores: Vec<String> = c.per_core_percent.iter().map(|p| format!("{p:.0}")).collect();
    rows.push(row("Per core, %", cores.join(" ")));
    if !latest.processes.is_empty() {
        rows.push(header("Top apps"));
        for p in top_by(latest.processes.clone(), TOP_ROWS, |p| (p.cpu_percent * 100.0) as u64) {
            rows.push(row(p.name, format!("{:.0}%", p.cpu_percent)));
        }
    }
    Content {
        title: "CPU".into(),
        subtitle: String::new(),
        legend: vec![format!("Total {:.0}%", c.total_percent)],
        rows,
    }
}

fn gpu_content(latest: &Latest) -> Content {
    let Some(g) = &latest.gpu else {
        return Content { title: "GPU".into(), ..Content::default() };
    };
    let percent = |p: Option<i64>| p.map_or_else(|| "Unknown".to_string(), |v| format!("{v}%"));
    let memory = g.memory_in_use_bytes.map_or_else(|| "Unknown".to_string(), |b| format_bytes(b as f64));
    Content {
        title: "GPU".into(),
        subtitle: g.model.clone().unwrap_or_default(),
        legend: vec![format!("Use {}", percent(g.device_percent)), format!("Memory {memory}")],
        rows: vec![
            row("Renderer", percent(g.renderer_percent)),
            row("Tiler", percent(g.tiler_percent)),
            row("Cores", g.core_count.map_or_else(|| "Unknown".to_string(), |c| c.to_string())),
        ],
    }
}

fn drives_content(latest: &Latest, settings: &Settings) -> Content {
    let mut rows = vec![header("Volumes")];
    for v in &latest.volumes {
        let name = v.mount_point.rsplit('/').find(|p| !p.is_empty()).unwrap_or("Macintosh HD");
        let kind = if v.is_network { " (network)" } else { "" };
        rows.push(row(
            format!("{name}{kind}"),
            format!("{} free of {}", format_bytes(v.available_bytes as f64), format_bytes(v.total_bytes as f64)),
        ));
    }
    for d in &latest.disks {
        rows.push(header(d.disk.model.clone().unwrap_or_else(|| d.disk.bsd_name.clone())));
        match latest.health.iter().find(|h| h.bsd_name.as_deref() == Some(&d.disk.bsd_name)) {
            Some(h) => {
                let h = &h.health;
                let healthy = h.critical_warning == 0 && h.media_errors == 0;
                rows.push(row("Health", if healthy { "Healthy" } else { "WARNING" }));
                rows.push(row("Temperature", format_temperature(h.temperature_c as f32, settings.temperature_unit)));
                rows.push(row("Wear", format!("{}% used, {}% spare", h.percent_used, h.available_spare_percent)));
                rows.push(row("Written", format_bytes(h.bytes_written as f64)));
                rows.push(row("Powered on", format!("{} hours", h.power_on_hours)));
                rows.push(row("Media errors", h.media_errors.to_string()));
            }
            None => rows.push(row("Health", "Unavailable")),
        }
    }
    let (read, write) = latest
        .disks
        .iter()
        .fold((0.0, 0.0), |(r, w), d| (r + d.read_per_sec, w + d.write_per_sec));
    Content {
        title: "Drives".into(),
        subtitle: String::new(),
        legend: vec![format!("Read {}", format_rate(read)), format!("Write {}", format_rate(write))],
        rows,
    }
}

fn fans_content(latest: &Latest, settings: &Settings) -> Content {
    let mut rows: Vec<Row> = latest
        .fans
        .iter()
        .map(|f| {
            // Anything but manual is macOS in control: it reports 0, then 3 once settled.
            let mode = if f.mode == MANUAL_MODE { "Manual" } else { "Auto" };
            let speed = if f.actual_rpm < 1.0 { "Stopped".to_string() } else { format!("{:.0} rpm", f.actual_rpm) };
            row(format!("Fan {}", f.index + 1), format!("{speed}, {mode}"))
        })
        .collect();
    rows.push(row("Status", latest.fan_state.label()));
    if let Some(message) = &latest.install_message {
        rows.push(row("Installer", message.clone()));
    }
    if !latest.temperatures.is_empty() {
        rows.push(header("Temperatures"));
        for area in &latest.temperatures {
            rows.push(row(
                area.area,
                format!(
                    "{} avg, {} peak",
                    format_temperature(area.average_c, settings.temperature_unit),
                    format_temperature(area.peak_c, settings.temperature_unit)
                ),
            ));
        }
    }
    if let Some((key, c)) = &latest.hottest {
        let name = gauge_core::sensors::sensor_name(key, gauge_core::sensors::is_m1_mac());
        rows.push(row("Hottest", format!("{name}, {}", format_temperature(*c, settings.temperature_unit))));
    }
    Content {
        title: "Fans".into(),
        subtitle: settings.active_preset.clone(),
        legend: latest.fans.iter().map(|f| format!("Fan {} {:.0}", f.index + 1, f.actual_rpm)).collect(),
        rows,
    }
}

fn power_state(b: &Battery) -> &'static str {
    match (b.fully_charged, b.charging, b.external_power) {
        (Some(true), _, Some(true)) => "Plugged in, fully charged",
        (_, Some(true), _) => "Charging",
        (_, _, Some(true)) => "Plugged in, not charging",
        _ => "On battery",
    }
}

fn power_content(latest: &Latest, settings: &Settings) -> Content {
    let Some(b) = &latest.battery else {
        return Content { title: "Power".into(), subtitle: "No battery".into(), rows: device_rows(latest), ..Content::default() };
    };
    Content {
        title: "Power".into(),
        subtitle: power_state(b).to_string(),
        legend: match latest.power {
            Some(p) => vec![format!("Using {}", format_watts(p.system_watts)), format!("In {}", format_watts(p.adapter_watts))],
            None => vec!["Using".into(), "In".into()],
        },
        rows: power_summary(latest, settings),
    }
}

pub fn content(module: &str, latest: &Latest, settings: &Settings) -> Content {
    match module {
        "network" => network_content(latest, settings),
        "memory" => memory_content(latest),
        "cpu" => cpu_content(latest),
        "gpu" => gpu_content(latest),
        "drives" => drives_content(latest, settings),
        "fans" => fans_content(latest, settings),
        "battery" => power_content(latest, settings),
        _ => Content::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, is_default: bool, addresses: &[&str]) -> InterfaceRate {
        InterfaceRate {
            name: name.into(),
            up: true,
            is_default,
            addresses: addresses.iter().map(|a| a.to_string()).collect(),
            bytes_in_total: 2_000_000,
            bytes_out_total: 1_000_000,
            in_per_sec: 14_000_000.0,
            out_per_sec: 2_100.0,
        }
    }

    fn value<'a>(content: &'a Content, key: &str) -> &'a str {
        &content.rows.iter().find(|r| r.key == key).unwrap_or_else(|| panic!("no {key}")).value
    }

    #[test]
    fn formats_bytes_at_a_readable_scale() {
        assert_eq!(format_bytes(0.0), "0 B");
        assert_eq!(format_bytes(2_100.0), "2.1 KB");
        assert_eq!(format_bytes(14_000_000.0), "14 MB");
        assert_eq!(format_bytes(-5.0), "0 B");
    }

    #[test]
    fn temperatures_follow_the_setting() {
        assert_eq!(format_temperature(100.0, TemperatureUnit::Celsius), "100°C");
        assert_eq!(format_temperature(100.0, TemperatureUnit::Fahrenheit), "212°F");
    }

    #[test]
    fn network_title_uses_the_default_route_so_vpns_do_not_double_count() {
        let latest = Latest {
            interfaces: vec![interface("utun3", false, &["10.0.0.1"]), interface("en0", true, &[])],
            ..Latest::default()
        };
        assert_eq!(bar_title("network", &latest, &Settings::default()), "↑2.1 KB/s ↓14 MB/s");
        assert_eq!(bar_title("network", &Latest::default(), &Settings::default()), "↑– ↓–");
    }

    #[test]
    fn network_panel_shows_totals_ping_and_hides_system_interfaces() {
        let latest = Latest {
            interfaces: vec![
                interface("lo0", false, &["127.0.0.1"]),
                interface("awdl0", false, &["fe80::1"]),
                interface("utun2", false, &["100.1.2.3"]),
                InterfaceRate { in_per_sec: 0.0, out_per_sec: 0.0, ..interface("utun9", false, &["10.9.9.9"]) },
                interface("en0", true, &["fe80::2", "192.168.1.5"]),
            ],
            today: (18_000_000_000, 3_200_000_000),
            ping_ms: Some(3.4),
            ..Latest::default()
        };
        let got = content("network", &latest, &Settings::default());
        assert_eq!(got.subtitle, "en0");
        assert_eq!(value(&got, "Today"), "↓18 GB  ↑3.2 GB");
        assert_eq!(value(&got, "Address"), "192.168.1.5");
        assert_eq!(value(&got, "Gateway ping"), "3 ms");
        assert!(got.rows.iter().any(|r| r.key == "utun2"));
        assert!(!got.rows.iter().any(|r| r.key == "utun9"), "idle tunnels stay hidden");
        assert!(!got.rows.iter().any(|r| r.key == "lo0" || r.key == "awdl0"));
    }

    #[test]
    fn widest_title_fits_any_rate() {
        assert_eq!(widest_title("↑2.1 MB/s ↓14 KB/s"), "↑888 MB/s ↓888 MB/s");
        assert_eq!(widest_title("↑0 B/s ↓999 B/s"), "↑888 MB/s ↓888 MB/s");
        assert_eq!(widest_title("Net 5 GB"), "Net 888 MB");
    }

    #[test]
    fn a_failed_ping_says_so() {
        let got = content("network", &Latest::default(), &Settings::default());
        assert_eq!(value(&got, "Gateway ping"), "No reply");
    }

    #[test]
    fn wifi_rows_follow_the_setting() {
        let latest = Latest {
            wifi: Some(Wifi {
                interface: None,
                ssid: None,
                rssi_dbm: -43,
                noise_dbm: -90,
                transmit_mbps: 1200.0,
                channel: Some(161),
            }),
            ..Latest::default()
        };
        let got = content("network", &latest, &Settings::default());
        assert_eq!(value(&got, "Wi-Fi signal"), "-43 dBm, Excellent");
        let off = Settings { show_wifi: false, ..Settings::default() };
        assert!(!content("network", &latest, &off).rows.iter().any(|r| r.key == "Wi-Fi signal"));
    }

    fn memory() -> Memory {
        Memory {
            total_bytes: 64 * 1024 * 1024 * 1024,
            used_bytes: 32 * 1024 * 1024 * 1024,
            app_bytes: 20 * 1024 * 1024 * 1024,
            wired_bytes: 10 * 1024 * 1024 * 1024,
            compressed_bytes: 2 * 1024 * 1024 * 1024,
            cached_files_bytes: 5 * 1024 * 1024 * 1024,
            free_bytes: 1024 * 1024 * 1024,
            swap_total_bytes: 1024 * 1024 * 1024,
            swap_used_bytes: 200 * 1024 * 1024,
            pressure: Pressure::Warning,
        }
    }

    #[test]
    fn memory_shows_percent_breakdown_and_pressure() {
        let latest = Latest { memory: Some(memory()), ..Latest::default() };
        assert_eq!(bar_title("memory", &latest, &Settings::default()), "RAM 50%");
        let got = content("memory", &latest, &Settings::default());
        assert_eq!(got.subtitle, "32 GB of 64 GB");
        assert_eq!(value(&got, "Pressure"), "Warning");
        assert_eq!(value(&got, "Swap used"), "200 MB of 1.0 GB");
        assert_eq!(format_memory(68_719_476_736), "64 GB");
        assert_eq!(got.legend[0], "Used 50%");
    }

    fn disk(name: &str) -> DiskRate {
        DiskRate {
            disk: DiskActivity {
                bsd_name: name.into(),
                model: Some("SSD".into()),
                location: None,
                bytes_read: 0,
                bytes_written: 0,
            },
            read_per_sec: 0.0,
            write_per_sec: 1_000_000.0,
        }
    }

    fn health(errors: u128) -> DriveHealth {
        DriveHealth {
            bsd_name: Some("disk0".into()),
            model: None,
            health: gauge_core::smart::Health {
                critical_warning: 0,
                temperature_c: 36,
                available_spare_percent: 100,
                percent_used: 3,
                bytes_read: 0,
                bytes_written: 182_000_000_000_000,
                power_on_hours: 1,
                power_cycles: 1,
                unsafe_shutdowns: 0,
                media_errors: errors,
            },
        }
    }

    #[test]
    fn a_disk_without_smart_says_so_instead_of_looking_fine() {
        let latest = Latest { disks: vec![disk("disk3")], ..Latest::default() };
        assert_eq!(value(&content("drives", &latest, &Settings::default()), "Health"), "Unavailable");
    }

    #[test]
    fn media_errors_show_a_warning() {
        let latest = |h| Latest { disks: vec![disk("disk0")], health: vec![h], ..Latest::default() };
        let settings = Settings::default();
        assert_eq!(value(&content("drives", &latest(health(0)), &settings), "Health"), "Healthy");
        assert_eq!(value(&content("drives", &latest(health(2)), &settings), "Health"), "WARNING");
    }

    fn fan() -> Fan {
        Fan { index: 0, mode: 1, target_rpm: 2500.0, actual_rpm: 2480.0, min_rpm: 1499.0, max_rpm: 5348.0 }
    }

    #[test]
    fn fan_panel_reports_real_mode_and_helper_state() {
        let latest = Latest {
            fans: vec![fan()],
            hottest: Some(("Tp0c".to_string(), 62.4)),
            fan_state: FanState::HelperMissing,
            ..Latest::default()
        };
        let settings = Settings::default();
        assert_eq!(bar_title("fans", &latest, &settings), "2480 rpm 62°C");
        let jitter = Latest { fans: vec![Fan { actual_rpm: 2483.0, ..fan() }], ..Latest::default() };
        assert_eq!(bar_title("fans", &jitter, &settings), "2480 rpm –");
        let got = content("fans", &latest, &settings);
        assert_eq!(value(&got, "Fan 1"), "2480 rpm, Manual");
        assert_eq!(value(&got, "Status"), "Fan helper not installed");
        assert_eq!(value(&got, "Hottest"), "CPU sensor Tp0c, 62°C");
    }

    #[test]
    fn templates_fill_in_values_and_keep_everything_else() {
        let value = |name: &str| (name == "up").then(|| "5 KB/s".to_string());
        assert_eq!(render_template("▲{up} net", value), "▲5 KB/s net");
        assert_eq!(render_template("{typo}", value), "{typo}");
        assert_eq!(render_template("open { brace", value), "open { brace");
        assert_eq!(render_template("", value), "");
    }

    #[test]
    fn a_custom_template_replaces_the_default() {
        let mut settings = Settings::default();
        settings.memory.bar_template = Some("🌀 {used} ({pressure})".into());
        let latest = Latest { memory: Some(memory()), ..Latest::default() };
        assert_eq!(bar_title("memory", &latest, &settings), "🌀 32 GB (Warning)");
        settings.memory.bar_template = Some(String::new());
        assert_eq!(bar_title("memory", &latest, &settings), "");
    }

    #[test]
    fn every_token_produces_a_value() {
        let settings = Settings::default();
        for module in gauge_core::settings::MODULES {
            for token in tokens(module) {
                let got = token_value(module, token.name, &Latest::default(), &settings);
                assert!(got.is_some(), "{module} {}", token.name);
            }
        }
    }

    #[test]
    fn the_preview_follows_order_and_marks_hidden_items() {
        let settings = Settings { order: vec!["fans".to_string(), "gpu".to_string()], ..Settings::default() };
        let items = preview_items(&Latest::default(), &settings);
        assert_eq!(items[0].name, "fans");
        assert!(items[0].shown);
        assert!(!items[1].shown, "GPU is hidden by default");
    }

    #[test]
    fn sensor_list_groups_sorts_and_hides_idle_channels() {
        use gauge_core::sensors::{Kind, Reading};
        let reading = |key: &str, kind, value| Reading { key: key.into(), kind, value };
        let got = sensor_rows(
            &[
                reading("Tp01", Kind::Temperature, 50.0),
                reading("Tp0D", Kind::Temperature, 70.0),
                reading("VP0R", Kind::Voltage, 0.0),
                reading("PSTR", Kind::Power, 12.5),
            ],
            true,
            &Settings::default(),
        );
        let keys: Vec<&str> = got.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["Temperature", "CPU performance core 3", "CPU performance core 1", "Power", "Sensor PSTR"]);
        assert_eq!(got[4].value, "12.50 W");
    }

    #[test]
    fn wifi_name_shows_only_when_asked_and_says_why_it_is_missing() {
        let wifi = Wifi {
            interface: None,
            ssid: None,
            rssi_dbm: -43,
            noise_dbm: -90,
            transmit_mbps: 1200.0,
            channel: None,
        };
        let latest = Latest { wifi: Some(wifi), ..Latest::default() };
        assert!(!content("network", &latest, &Settings::default()).rows.iter().any(|r| r.key == "Wi-Fi network"));
        let on = Settings { show_wifi_name: true, ..Settings::default() };
        assert_eq!(value(&content("network", &latest, &on), "Wi-Fi network"), "Needs Location permission");
    }

    #[test]
    fn usb_power_names_the_device_it_charges() {
        use gauge_core::battery::UsbDevice;
        let phone = UsbDevice { name: "iPhone".into(), allocated_watts: 12.0 };
        let pad = UsbDevice { name: "iPad".into(), allocated_watts: 15.0 };
        assert_eq!(usb_label(&[]), "USB");
        assert_eq!(usb_label(&[phone.clone()]), "iPhone");
        assert_eq!(usb_label(&[phone.clone(), pad]), "iPhone + 1");
        let latest = Latest {
            battery: Some(Battery { usb_out_watts: Some(1.9), ..plugged_in() }),
            usb_devices: vec![phone],
            ..Latest::default()
        };
        let row = usb_row(&latest).unwrap();
        assert_eq!((row.key.as_str(), row.value.as_str()), ("Powering iPhone", "1.9 W of up to 12 W"));
    }

    #[test]
    fn device_batteries_appear_with_or_without_a_mac_battery() {
        let latest = Latest {
            device_batteries: vec![gauge_core::battery::DeviceBattery { name: "Magic Mouse".into(), percent: 42, charging: None }],
            ..Latest::default()
        };
        assert_eq!(value(&content("battery", &latest, &Settings::default()), "Magic Mouse"), "42%");
    }

    fn plugged_in() -> Battery {
        Battery {
            percent: Some(80),
            health_percent: Some(84.0),
            cycle_count: Some(613),
            voltage_mv: Some(12_650),
            amperage_ma: Some(1500),
            power_watts: Some(18.975),
            temperature_c: Some(31.0),
            charging: Some(true),
            external_power: Some(true),
            fully_charged: Some(false),
            minutes_remaining: None,
            minutes_to_full: Some(75),
            design_cycles: Some(1000),
            nominal_mah: Some(7299),
            design_mah: Some(8694),
            remaining_mah: Some(5800),
            adapter: Some(gauge_core::battery::Adapter {
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
        }
    }

    fn sample(adapter: f64, battery: f64, system: f64, display: f64) -> gauge_core::battery::PowerSample {
        gauge_core::battery::PowerSample { adapter_watts: adapter, battery_watts: battery, system_watts: system, display_watts: display }
    }

    #[test]
    fn plugged_in_flow_splits_incoming_power_and_adds_up() {
        let latest = Latest { battery: Some(plugged_in()), power: Some(sample(47.0, 19.0, 28.0, 3.0)), ..Latest::default() };
        let flow = power_flow(&latest).unwrap();
        assert_eq!(flow.adapter, 47.0);
        assert_eq!(flow.battery, 0.0);
        let labels: Vec<&str> = flow.outputs.iter().map(|o| o.0.as_str()).collect();
        assert_eq!(labels, ["Charging", "Display", "USB", "Other"]);
        let out: f64 = flow.outputs.iter().map(|o| o.1).sum();
        assert!((out - flow.total()).abs() < 0.001);
    }

    #[test]
    fn unplugged_flow_comes_only_from_the_battery() {
        let battery = Battery { external_power: Some(false), charging: Some(false), adapter: None, amperage_ma: Some(-1000), usb_out_watts: None, ..plugged_in() };
        let latest = Latest { battery: Some(battery), power: Some(sample(0.0, 12.0, 14.0, 3.0)), ..Latest::default() };
        let flow = power_flow(&latest).unwrap();
        assert_eq!((flow.adapter, flow.battery), (0.0, 12.0));
        assert!(!flow.outputs.iter().any(|o| o.0 == "Charging"));
    }

    #[test]
    fn a_small_adapter_and_battery_can_both_feed_the_mac() {
        let battery = Battery { amperage_ma: Some(-2000), charging: Some(false), usb_out_watts: None, ..plugged_in() };
        let latest = Latest { battery: Some(battery), power: Some(sample(30.0, 20.0, 50.0, 0.0)), ..Latest::default() };
        let flow = power_flow(&latest).unwrap();
        assert_eq!((flow.adapter, flow.battery), (30.0, 20.0));
        assert_eq!(flow.outputs, [("Other".to_string(), 50.0)]);
    }

    #[test]
    fn a_sensor_reading_high_never_makes_the_paths_exceed_the_total() {
        let battery = Battery { usb_out_watts: Some(90.0), ..plugged_in() };
        let latest = Latest { battery: Some(battery), power: Some(sample(10.0, 0.0, 10.0, 3.0)), ..Latest::default() };
        let flow = power_flow(&latest).unwrap();
        let out: f64 = flow.outputs.iter().map(|o| o.1).sum();
        assert!(out <= 10.0 + 0.001, "{out}");
    }

    #[test]
    fn no_power_reading_means_no_diagram() {
        assert_eq!(power_flow(&Latest::default()), None);
    }

    #[test]
    fn panel_summary_uses_live_power_and_lists_adapter_and_battery() {
        let latest = Latest { battery: Some(plugged_in()), power: Some(sample(47.0, 19.0, 30.0, 3.0)), energy_today_wh: 1234.0, ..Latest::default() };
        let got = content("battery", &latest, &Settings::default());
        assert_eq!(got.title, "Power");
        assert_eq!(got.subtitle, "Charging");
        assert_eq!(value(&got, "Using"), "30 W");
        assert_eq!(value(&got, "Used today"), "1.23 kWh");
        assert_eq!(value(&got, "96W USB-C Power Adapter"), "50% of its limit");
        assert_eq!(value(&got, "Battery"), "80%, 84% health");
        assert_eq!(value(&got, "Time to full"), "1h 15m");
    }

    #[test]
    fn power_legend_names_the_graph_lines() {
        let latest = Latest { battery: Some(plugged_in()), power: Some(sample(47.0, 19.0, 30.0, 3.0)), ..Latest::default() };
        assert_eq!(content("battery", &latest, &Settings::default()).legend, vec!["Using 30 W", "In 47 W"]);
        let waiting = Latest { battery: Some(plugged_in()), ..Latest::default() };
        assert_eq!(content("battery", &waiting, &Settings::default()).legend, vec!["Using", "In"]);
    }

    #[test]
    fn unplugged_summary_shows_time_left_and_no_adapter() {
        let battery = Battery { external_power: Some(false), charging: Some(false), adapter: None, minutes_remaining: Some(312), ..plugged_in() };
        let latest = Latest { battery: Some(battery), ..Latest::default() };
        let got = content("battery", &latest, &Settings::default());
        assert_eq!(got.subtitle, "On battery");
        assert_eq!(value(&got, "Time left"), "5h 12m");
        assert!(!got.rows.iter().any(|r| r.key.contains("Adapter")));
    }

    #[test]
    fn window_cards_group_battery_health_and_adapter() {
        let latest = Latest { battery: Some(plugged_in()), power: Some(sample(47.0, 19.0, 30.0, 3.0)), ..Latest::default() };
        let [now, health, adapter] = power_cards(&latest, &Settings::default());
        assert!(now.1.iter().any(|r| r.key == "Voltage" && r.value == "12.65 V"));
        assert!(health.1.iter().any(|r| r.key == "Cycles" && r.value == "613 of 1000"));
        assert!(adapter.1.iter().any(|r| r.key == "Model" && r.value == "96W USB-C"));
        assert!(adapter.1.iter().any(|r| r.key == "In use" && r.value == "50%"));
    }

    #[test]
    fn charge_labels_say_what_is_happening() {
        assert_eq!(charge_limit_label(Some(80), Some(None), false), "Limit: 80%");
        assert_eq!(charge_limit_label(Some(100), None, false), "No charge limit");
        assert_eq!(charge_limit_label(None, Some(Some(70)), false), "Limit: 70%");
        assert_eq!(charge_limit_label(None, Some(None), false), "No charge limit");
        assert_eq!(charge_limit_label(None, None, false), "Limit unknown");
        assert_eq!(charge_limit_label(Some(80), None, true), "Topping up to 100%");
        assert_eq!(slider_position(None, Some(Some(70))), 70.0);
        assert_eq!(slider_position(None, Some(None)), 100.0);
    }

    #[test]
    fn the_power_item_shows_what_the_mac_is_using() {
        let latest = Latest { battery: Some(plugged_in()), ..Latest::default() };
        assert_eq!(bar_title("battery", &latest, &Settings::default()), "⚡︎28 W");
    }

    #[test]
    fn energy_reads_at_a_sensible_scale() {
        assert_eq!(format_energy(450.4), "450 Wh");
        assert_eq!(format_energy(1234.0), "1.23 kWh");
        assert_eq!(format_watts(1.86), "1.9 W");
        assert_eq!(format_watts(56.4), "56 W");
    }

    #[test]
    fn macos_idle_mode_reads_as_auto_and_a_stopped_fan_says_so() {
        let idle = Fan { mode: 3, actual_rpm: 0.0, ..fan() };
        let latest = Latest { fans: vec![idle], ..Latest::default() };
        assert_eq!(value(&content("fans", &latest, &Settings::default()), "Fan 1"), "Stopped, Auto");
    }

    #[test]
    fn fan_panel_lists_temperatures_by_area() {
        let latest = Latest {
            temperatures: vec![gauge_core::sensors::AreaTemperature { area: "CPU", average_c: 62.0, peak_c: 78.0 }],
            ..Latest::default()
        };
        let got = content("fans", &latest, &Settings::default());
        assert!(got.rows.iter().any(|r| r.header && r.key == "Temperatures"));
        assert_eq!(value(&got, "CPU"), "62°C avg, 78°C peak");
        let plain = content("fans", &Latest::default(), &Settings::default());
        assert!(!plain.rows.iter().any(|r| r.key == "Temperatures"));
    }

    #[test]
    fn every_module_has_a_graph() {
        for module in gauge_core::settings::MODULES {
            assert!(!graphs(module, &Settings::default()).is_empty(), "{module}");
        }
    }

    #[test]
    fn empty_readings_never_panic() {
        for module in gauge_core::settings::MODULES {
            let _ = content(module, &Latest::default(), &Settings::default());
            let _ = bar_title(module, &Latest::default(), &Settings::default());
        }
    }
}
