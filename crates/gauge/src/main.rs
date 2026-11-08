//! `gauge`: prints the monitor's readings as JSON, for scripts and for checking collectors.

use gauge_core::{battery, drives, gpu, memory, network, ping, processor, sensors, smart, wifi};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: flowgauge [all|net|apps|traffic|drives|sensors|fans|cpu|gpu|memory|battery|cost]";
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const APP_SAMPLE_SECONDS: u32 = 1;
const TOP_PROCESSES: usize = 5;

fn to_json<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn net() -> Result<Value, String> {
    let before = network::read_counters().map_err(|e| e.to_string())?;
    let started = Instant::now();
    std::thread::sleep(SAMPLE_INTERVAL);
    let after = network::read_counters().map_err(|e| e.to_string())?;
    let default = network::default_interface();
    to_json(network::rates(
        &before,
        &after,
        started.elapsed().as_secs_f64(),
        &network::read_addresses(),
        default.as_deref(),
    ))
}

fn apps() -> Result<Value, String> {
    to_json(network::app_traffic(APP_SAMPLE_SECONDS).map_err(|e| e.to_string())?)
}

/// Traffic per app and destination over two seconds, from `nettop` snapshots.
fn traffic() -> Result<Value, String> {
    let mut tracker = gauge_core::traffic::Tracker::default();
    tracker.step(&gauge_core::traffic::read_snapshot().map_err(|e| e.to_string())?, 0.0);
    std::thread::sleep(Duration::from_secs(2));
    let interval = tracker.step(&gauge_core::traffic::read_snapshot().map_err(|e| e.to_string())?, 2.0);
    let apps: Vec<Value> = interval
        .apps
        .iter()
        .map(|(app, usage)| {
            let destinations: Vec<Value> = interval
                .destinations
                .get(app)
                .into_iter()
                .flatten()
                .map(|(address, u)| serde_json::json!({
                    "address": address.to_string(),
                    "local": gauge_core::traffic::is_local(address),
                    "bytes_in": u.bytes_in,
                    "bytes_out": u.bytes_out,
                    "rtt_ms": interval.rtt_ms.get(address),
                }))
                .collect();
            serde_json::json!({ "app": app, "bytes_in": usage.bytes_in, "bytes_out": usage.bytes_out, "destinations": destinations })
        })
        .collect();
    Ok(Value::Array(apps))
}

fn drives_json() -> Result<Value, String> {
    let before = drives::read_activity();
    let started = Instant::now();
    std::thread::sleep(SAMPLE_INTERVAL);
    let after = drives::read_activity();
    let seconds = started.elapsed().as_secs_f64();
    let activity: Vec<Value> = after
        .iter()
        .map(|now| {
            let previous = before.iter().find(|b| b.bsd_name == now.bsd_name);
            json!({
                "disk": now,
                "read_per_sec": previous.map_or(0.0, |p| network::rate(p.bytes_read, now.bytes_read, seconds)),
                "write_per_sec": previous.map_or(0.0, |p| network::rate(p.bytes_written, now.bytes_written, seconds)),
            })
        })
        .collect();
    Ok(json!({ "volumes": drives::read_volumes(), "activity": activity, "health": smart::read_all() }))
}

fn sensors_json() -> Result<Value, String> {
    let sensors = sensors::Sensors::open().map_err(|e| e.to_string())?;
    to_json(sensors.read())
}

fn fans() -> Result<Value, String> {
    let sensors = sensors::Sensors::open().map_err(|e| e.to_string())?;
    to_json(sensors.fans().map_err(|e| e.to_string())?)
}

fn cpu() -> Result<Value, String> {
    let (cpu, processes) = processor::Sampler::new().sample();
    Ok(json!({
        "cpu": cpu,
        "top_by_cpu": processor::top_by(processes.clone(), TOP_PROCESSES, |p| (p.cpu_percent * 100.0) as u64),
        "top_by_memory": processor::top_by(processes, TOP_PROCESSES, |p| p.memory_bytes),
    }))
}

fn cpu_milliseconds() -> f64 {
    // SAFETY: all-zero is valid for rusage, and getrusage only writes through the pointer.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let seconds = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64;
    let micros = f64::from(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec);
    seconds * 1e3 + micros / 1e3
}

/// CPU per collector at the app's polling rate, to keep the always-on cost visible.
fn cost() -> Result<Value, String> {
    const CALLS: u32 = 20;
    let mut rows = Vec::new();
    let mut measure = |name: &str, every_seconds: f64, f: &mut dyn FnMut()| {
        let before = cpu_milliseconds();
        for _ in 0..CALLS {
            f();
        }
        let per_call = (cpu_milliseconds() - before) / f64::from(CALLS);
        rows.push(json!({
            "collector": name,
            "ms_per_call": (per_call * 100.0).round() / 100.0,
            "every_seconds": every_seconds,
            "percent_cpu": (per_call / every_seconds / 10.0 * 100.0).round() / 100.0,
        }));
    };
    let sensors = sensors::Sensors::open().map_err(|e| e.to_string())?;
    let mut sampler = processor::Sampler::new();
    measure("counters", 1.0, &mut || { let _ = network::read_counters(); });
    measure("addresses", 10.0, &mut || { network::read_addresses(); });
    measure("default_interface", 10.0, &mut || { network::default_interface(); });
    measure("gateway_and_ping", 5.0, &mut || {
        if let Some(gateway) = ping::default_gateway() {
            ping::ping(gateway, 1, Duration::from_secs(1));
        }
    });
    measure("cpu", 2.0, &mut || { sampler.poll(false); });
    measure("memory", 2.0, &mut || { memory::read(); });
    measure("gpu", 2.0, &mut || { gpu::read(); });
    measure("fans", 3.0, &mut || { let _ = sensors.fans(); });
    measure("hottest", 3.0, &mut || { sensors.hottest(); });
    measure("drive_activity", 5.0, &mut || { drives::read_activity(); });
    measure("volumes", 5.0, &mut || { drives::read_volumes(); });
    measure("wifi", 10.0, &mut || { wifi::read(); });
    measure("battery", 30.0, &mut || { battery::read(); });
    Ok(Value::Array(rows))
}

fn section(name: &str) -> Result<Value, String> {
    match name {
        "cost" => cost(),
        "net" => net(),
        "apps" => apps(),
        "traffic" => traffic(),
        "drives" => drives_json(),
        "sensors" => sensors_json(),
        "fans" => fans(),
        "cpu" => cpu(),
        "gpu" => to_json(gpu::read()),
        "memory" => to_json(memory::read()),
        "battery" => to_json(battery::read()),
        "all" => {
            let mut all = serde_json::Map::new();
            for name in ["net", "drives", "fans", "cpu", "gpu", "memory", "battery"] {
                all.insert(name.to_string(), section(name)?);
            }
            Ok(Value::Object(all))
        }
        _ => Err(USAGE.to_string()),
    }
}

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    match section(&name).and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string())) {
        Ok(text) => println!("{text}"),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }
}
