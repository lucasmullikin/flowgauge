//! One-off test: can a locally signed root process hold a manual fan speed on this Mac?
//!
//!   fan-spike read                                    no root needed, writes nothing
//!   fan-spike test FAN RPM SECONDS [CRASH_AFTER]      root; raises one fan, then restores Auto
//!   fan-spike auto FAN                                root; recovery, sets the fan to Auto
//!
//! The test re-executes itself as a watchdog in its own session, with a heartbeat pipe on stdin.
//! If the pipe closes (exit, panic, SIGKILL) or beats stop (stopped or hung process), the
//! watchdog restores Auto.

mod plan;

use plan::{ABORT_C, Observations, PREFLIGHT_MAX_C, hottest, validate_target};
use smc::{Error, Smc};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_TEST_SECONDS: f32 = 120.0;
const BASELINE_SECONDS: u32 = 5;
const SETTLE_SECONDS: u32 = 20;
const HEARTBEAT_TIMEOUT_MS: libc::c_int = 3000;
const AUTO: u8 = 0;
const MANUAL: u8 = 1;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

// stdout may be gone (closed terminal); that must never interrupt restoring the fans.
macro_rules! log {
    ($($arg:tt)*) => {{ let _ = writeln!(std::io::stdout(), $($arg)*); }};
}

#[derive(Clone, Copy, PartialEq)]
struct Sample {
    mode: u8,
    target: f32,
    actual: f32,
}

fn sample(smc: &Smc, fan: u8) -> Result<Sample, Error> {
    Ok(Sample {
        mode: smc.read_u8(&format!("F{fan}Md"))?,
        target: smc.read_f32(&format!("F{fan}Tg"))?,
        actual: smc.read_f32(&format!("F{fan}Ac"))?,
    })
}

fn temperature_keys(smc: &Smc) -> Result<Vec<String>, Error> {
    let mut keys = Vec::new();
    for index in 0..smc.key_count()? {
        let Ok(key) = smc.key_at(index) else { continue };
        if key.starts_with('T') && smc.read_f32(&key).is_ok() {
            keys.push(key);
        }
    }
    Ok(keys)
}

fn hottest_now(smc: &Smc, keys: &[String]) -> Option<(String, f32)> {
    hottest(
        keys.iter()
            .filter_map(|k| smc.read_f32(k).ok().map(|c| (k.clone(), c))),
    )
}

/// Writes Auto and confirms it by reading back. Retries because this is the safety path.
fn restore_auto(smc: &Smc, fan: u8) -> bool {
    let key = format!("F{fan}Md");
    for _ in 0..5 {
        let written = smc.write_u8(&key, AUTO);
        match (written, smc.read_u8(&key)) {
            (Ok(()), Ok(AUTO)) => return true,
            (written, read) => log!("restore retry: write {written:?}, read {read:?}"),
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// Parent's end of the watchdog: heartbeats go over the child's stdin.
struct Watchdog {
    child: Child,
    beat: ChildStdin,
}

impl Watchdog {
    /// Returns once the watchdog has its own working SMC connection.
    fn spawn(fan: u8) -> Result<Watchdog, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .args(["watchdog", &fan.to_string()])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("watchdog spawn failed: {e}"))?;
        let stderr = child.stderr.take().ok_or("watchdog has no stderr")?;
        let beat = child.stdin.take().ok_or("watchdog has no stdin")?;
        let mut line = String::new();
        BufReader::new(stderr)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.trim() != "ready" {
            return Err(format!("watchdog not ready: {line:?}"));
        }
        Ok(Watchdog { child, beat })
    }

    /// False once the watchdog is gone. Rust ignores SIGPIPE, so this returns EPIPE.
    fn beat(&mut self) -> bool {
        self.beat.write_all(b".").is_ok()
    }

    /// Closes the pipe and reports whether the watchdog confirmed Auto.
    fn finish(self) -> bool {
        let Watchdog { mut child, beat } = self;
        drop(beat);
        child.wait().is_ok_and(|status| status.success())
    }
}

/// Blocks until the parent exits or its heartbeats stop.
fn wait_for_parent() {
    let mut buffer = [0u8; 64];
    loop {
        let mut poll_fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll_fd and buffer are live and correctly sized for these calls.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, HEARTBEAT_TIMEOUT_MS) };
        let interrupted = |n: isize| {
            n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        };
        if interrupted(ready as isize) {
            continue;
        }
        if ready <= 0 {
            log!("[watchdog] heartbeat stopped");
            return;
        }
        // SAFETY: see above.
        let n = unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        if interrupted(n) {
            continue;
        }
        if n <= 0 {
            log!("[watchdog] parent gone");
            return;
        }
    }
}

fn cmd_watchdog(fan: u8) -> Result<(), String> {
    // SAFETY: plain libc calls. A new session leaves the terminal's signals behind.
    unsafe {
        libc::setsid();
        for signal in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGTSTP,
        ] {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
    let smc = Smc::open().map_err(|e| e.to_string())?;
    smc.read_u8(&format!("F{fan}Md")).map_err(|e| e.to_string())?;
    eprintln!("ready");
    wait_for_parent();
    if restore_auto(&smc, fan) {
        log!("[watchdog] fan {fan} confirmed Auto");
        Ok(())
    } else {
        log!("[watchdog] FAN {fan} NOT RESTORED. Run: sudo fan-spike auto {fan}, or reboot");
        std::process::exit(3);
    }
}

fn cmd_read() -> Result<(), String> {
    let smc = Smc::open().map_err(|e| e.to_string())?;
    let fans = smc.read_u8("FNum").map_err(|e| e.to_string())?;
    for fan in 0..fans {
        let s = sample(&smc, fan).map_err(|e| e.to_string())?;
        let min = smc.read_f32(&format!("F{fan}Mn")).map_err(|e| e.to_string())?;
        let max = smc.read_f32(&format!("F{fan}Mx")).map_err(|e| e.to_string())?;
        log!(
            "fan {fan}: mode={} target={:.0} actual={:.0} range={min:.0}-{max:.0}",
            s.mode, s.target, s.actual
        );
    }
    let keys = temperature_keys(&smc).map_err(|e| e.to_string())?;
    log!("temperature keys: {}, hottest: {:?}", keys.len(), hottest_now(&smc, &keys));
    log!("Ftst present: {}", smc.info("Ftst").is_ok());
    Ok(())
}

fn cmd_auto(fan: u8) -> Result<(), String> {
    let smc = Smc::open().map_err(|e| e.to_string())?;
    if restore_auto(&smc, fan) {
        log!("fan {fan} confirmed Auto");
        Ok(())
    } else {
        Err(format!("fan {fan} could not be restored"))
    }
}

fn cmd_test(fan: u8, rpm: f32, seconds: f32, crash_after: Option<f32>) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        return Err("test needs root".into());
    }
    if !(1.0..=MAX_TEST_SECONDS).contains(&seconds) {
        return Err(format!("seconds must be 1-{MAX_TEST_SECONDS}"));
    }
    if crash_after.is_some_and(|after| !(0.0..seconds).contains(&after)) {
        return Err("CRASH_AFTER must fall inside the hold".into());
    }
    let smc = Smc::open().map_err(|e| e.to_string())?;
    if smc.info("Ftst").is_ok() {
        return Err("this Mac has Ftst; the unlock path is out of scope for this test".into());
    }
    if fan >= smc.read_u8("FNum").map_err(|e| e.to_string())? {
        return Err(format!("no fan {fan}"));
    }
    let start_sample = sample(&smc, fan).map_err(|e| e.to_string())?;
    if start_sample.mode != AUTO {
        return Err(format!("fan {fan} is in mode {}, expected Auto", start_sample.mode));
    }
    let min = smc.read_f32(&format!("F{fan}Mn")).map_err(|e| e.to_string())?;
    let max = smc.read_f32(&format!("F{fan}Mx")).map_err(|e| e.to_string())?;
    let rpm = validate_target(rpm, min, max, start_sample.actual)?;
    let keys = temperature_keys(&smc).map_err(|e| e.to_string())?;
    let (hot_key, hot_c) = hottest_now(&smc, &keys).ok_or("no usable temperature sensors")?;
    if hot_c > PREFLIGHT_MAX_C {
        return Err(format!("{hot_key} is {hot_c:.1}C, above the {PREFLIGHT_MAX_C}C start limit"));
    }
    log!("preflight ok: fan {fan}, {rpm} rpm for {seconds}s, hottest {hot_key} {hot_c:.1}C");

    let mut watchdog = Watchdog::spawn(fan)?;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }

    log!("-- baseline, Auto");
    for second in 0..BASELINE_SECONDS {
        if STOP.load(Ordering::SeqCst) || !watchdog.beat() {
            return Err("stopped before any write".into());
        }
        let s = sample(&smc, fan).map_err(|e| e.to_string())?;
        log!("t=-{} mode={} target={:.0} actual={:.0}", BASELINE_SECONDS - second, s.mode, s.target, s.actual);
        std::thread::sleep(Duration::from_secs(1));
    }
    if STOP.load(Ordering::SeqCst) || !watchdog.beat() {
        return Err("stopped before any write".into());
    }

    log!("-- writing manual mode");
    let mode_write = smc.write_u8(&format!("F{fan}Md"), MANUAL);
    log!("F{fan}Md=1 -> {mode_write:?}");
    if let Err(e) = mode_write {
        restore_auto(&smc, fan);
        return Err(format!("mode write refused: {e}"));
    }
    let target_write = smc.write_f32(&format!("F{fan}Tg"), rpm);
    log!("F{fan}Tg={rpm} -> {target_write:?}");
    if let Err(e) = target_write {
        restore_auto(&smc, fan);
        return Err(format!("target write refused: {e}"));
    }

    let hold = hold_and_observe(&smc, fan, rpm, seconds, crash_after, &keys, &mut watchdog);
    if let Err(e) = &hold {
        log!("hold ended early: {e}");
    }

    log!("-- restoring Auto");
    let restored = restore_auto(&smc, fan);
    log!("restored: {restored}");
    let mut failed_reads = 0;
    for second in 1..=SETTLE_SECONDS {
        watchdog.beat();
        std::thread::sleep(Duration::from_secs(1));
        match sample(&smc, fan) {
            Ok(s) => log!("t=+{second} mode={} target={:.0} actual={:.0}", s.mode, s.target, s.actual),
            Err(_) => failed_reads += 1,
        }
    }
    let final_mode = smc.read_u8(&format!("F{fan}Md"));
    let settled = restored && failed_reads == 0 && final_mode == Ok(AUTO);
    let watchdog_ok = watchdog.finish();

    let observed = hold?;
    log!(
        "RESULT held={} mode_reverts={} (first {:?}) target_reverts={} (first {:?}) reached_at={:?} settled_auto={settled} final_mode={final_mode:?} watchdog_ok={watchdog_ok}",
        observed.held(),
        observed.mode_reverts,
        observed.first_mode_revert_at,
        observed.target_reverts,
        observed.first_target_revert_at,
        observed.reached_at
    );
    if settled && watchdog_ok {
        Ok(())
    } else {
        Err("fan was not confirmed back in Auto".into())
    }
}

fn hold_and_observe(
    smc: &Smc,
    fan: u8,
    rpm: f32,
    seconds: f32,
    crash_after: Option<f32>,
    keys: &[String],
    watchdog: &mut Watchdog,
) -> Result<Observations, String> {
    log!("-- holding, no re-assert, sampling every 100ms");
    let mut observed = Observations::new(rpm);
    let started = Instant::now();
    let mut last: Option<Sample> = None;
    let mut next_second = 0.0;
    loop {
        let t = started.elapsed().as_secs_f32();
        if t >= seconds {
            return Ok(observed);
        }
        if STOP.load(Ordering::SeqCst) {
            return Err("stopped by signal".into());
        }
        if !watchdog.beat() {
            return Err("watchdog gone".into());
        }
        let s = sample(smc, fan).map_err(|e| format!("fan read failed: {e}"))?;
        observed.observe(t, s.mode, s.target, s.actual);
        let changed = last.is_some_and(|l| l.mode != s.mode || (l.target - s.target).abs() > 1.0);
        if changed || t >= next_second {
            log!("t={t:.1} mode={} target={:.0} actual={:.0}", s.mode, s.target, s.actual);
        }
        last = Some(s);
        if t >= next_second {
            next_second += 1.0;
            let (key, c) = hottest_now(smc, keys).ok_or("lost all temperature sensors")?;
            if c > ABORT_C {
                return Err(format!("{key} reached {c:.1}C, aborting"));
            }
        }
        if crash_after.is_some_and(|after| t >= after) {
            log!("t={t:.1} simulating a crash with SIGKILL");
            // SAFETY: deliberately kills this process so the watchdog path gets exercised.
            unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let usage = "usage: fan-spike read | test FAN RPM SECONDS [CRASH_AFTER] | auto FAN";
    let fan = |s: &str| s.parse::<u8>().map_err(|_| format!("bad fan {s}"));
    let num = |s: &str| s.parse::<f32>().map_err(|_| format!("bad number {s}"));
    match args.as_slice() {
        ["read"] => cmd_read(),
        ["auto", f] => cmd_auto(fan(f)?),
        ["watchdog", f] => cmd_watchdog(fan(f)?),
        ["test", f, r, s] => cmd_test(fan(f)?, num(r)?, num(s)?, None),
        ["test", f, r, s, c] => cmd_test(fan(f)?, num(r)?, num(s)?, Some(num(c)?)),
        _ => Err(usage.into()),
    }
}

fn main() {
    if let Err(message) = run() {
        log!("error: {message}");
        std::process::exit(1);
    }
}
