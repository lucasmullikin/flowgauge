//! Root LaunchDaemon that owns all fan writes. Runs `--allow-uid UID`; only that user can connect.
//!
//! Fans return to Auto when the client disconnects or stops sending for 5 seconds, when
//! temperatures stay too high or sensors disappear, and on startup and shutdown. If launchd
//! restarts this after a crash, startup restores Auto too.

use fan_proto::{
    CLIENT_TIMEOUT_SECONDS, Command, MAX_CONSECUTIVE_REVERTS, MAX_LINE_BYTES, SOCKET_PATH, Safety,
    ThermalGuard, clamp_rpm, parse_command, severity,
};
use smc::{Error, Smc};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const AUTO: u8 = 0;
const MANUAL: u8 = 1;
const TICK: Duration = Duration::from_millis(250);
const SAFETY_EVERY: Duration = Duration::from_secs(1);
const SENSOR_MIN_C: f32 = 15.0;
/// Only used to skip junk sensors at startup. See `Daemon::hottest` for later readings.
const SENSOR_MAX_C: f32 = 115.0;
/// Exit code for a Mac this helper can't support, so launchd doesn't keep restarting it.
const EXIT_UNSUPPORTED: i32 = 0;
const TARGET_TOLERANCE_RPM: f32 = 1.0;
/// Enough to serve a normal client, and short enough that a flood can't stall the safety checks.
/// The hardware takes a moment to report a new setting, so a just-changed fan isn't checked yet.
/// Without this, every speed change looked like macOS taking the fan back.
const SETTLE_TIME: Duration = Duration::from_secs(2);
const MAX_COMMANDS_PER_TICK: usize = 8;
const MAX_BUFFERED_BYTES: usize = 1024;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

struct Daemon {
    smc: Smc,
    fans: u8,
    /// Sensors that read sensibly at startup, so unconnected ones can't trip the guard.
    temperature_keys: Vec<(String, u32)>,
    /// Fans currently under manual control, with their requested speed.
    owned: BTreeMap<u8, f32>,
    /// Fans that didn't accept going back to Auto. Retried until they do.
    pending_release: BTreeSet<u8>,
    /// When each fan's requested speed last changed.
    changed_at: BTreeMap<u8, Instant>,
    guard: ThermalGuard,
    reverts: u64,
    consecutive_reverts: u32,
}

impl Daemon {
    fn open() -> Result<Daemon, Error> {
        let smc = Smc::open()?;
        let fans = smc.read_u8("FNum")?;
        let mut temperature_keys = Vec::new();
        for index in 0..smc.key_count()? {
            let Ok(key) = smc.key_at(index) else { continue };
            if key.starts_with('T')
                && smc.read_f32(&key).is_ok_and(|c| (SENSOR_MIN_C..=SENSOR_MAX_C).contains(&c))
            {
                let code = smc::fourcc(&key)?;
                temperature_keys.push((key, code));
            }
        }
        Ok(Daemon {
            smc,
            fans,
            temperature_keys,
            owned: BTreeMap::new(),
            pending_release: BTreeSet::new(),
            changed_at: BTreeMap::new(),
            guard: ThermalGuard::default(),
            reverts: 0,
            consecutive_reverts: 0,
        })
    }

    /// The worst sensor, scaled for its type. A sensor that worked at startup and now reads
    /// very high counts as hot rather than broken. Losing most sensors counts as unknown.
    fn hottest(&self) -> Option<f32> {
        let readings: Vec<f32> = self
            .temperature_keys
            .iter()
            .filter_map(|(name, code)| {
                let c = self.smc.read_known_f32(*code).ok()?;
                (c.is_finite() && c >= SENSOR_MIN_C).then(|| severity(name, c))
            })
            .collect();
        if readings.len() * 2 < self.temperature_keys.len() {
            return None;
        }
        readings.into_iter().max_by(f32::total_cmp)
    }

    fn mode_key(fan: u8) -> String {
        format!("F{fan}Md")
    }

    fn release(&mut self, fan: u8) -> bool {
        self.owned.remove(&fan);
        // Hardware takes a moment to report a change, so a quick read-back can look like a failure.
        for _ in 0..5 {
            let written = self.smc.write_u8(&Self::mode_key(fan), AUTO);
            // macOS reports its own control as 0 or, once it has settled, as 3.
            let released = self.smc.read_u8(&Self::mode_key(fan)).is_ok_and(|mode| mode != MANUAL);
            if written.is_ok() && released {
                self.pending_release.remove(&fan);
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!("gauge-fand: fan {fan} didn't return to Auto, will keep trying");
        self.pending_release.insert(fan);
        false
    }

    fn release_all(&mut self) {
        for fan in 0..self.fans {
            self.release(fan);
        }
    }

    fn apply(&mut self, fan: u8, rpm: f32) -> Result<f32, String> {
        if fan >= self.fans {
            return Err("no such fan".into());
        }
        // The guard only runs while fans are owned, so take a fresh reading for the first one.
        if self.owned.is_empty() {
            self.guard.update(self.hottest());
        }
        if self.guard.state == Safety::ThermalHold {
            return Err("thermal hold".into());
        }
        let read = |suffix: &str| {
            self.smc
                .read_f32(&format!("F{fan}{suffix}"))
                .map_err(|e| e.to_string())
        };
        let rpm = clamp_rpm(rpm, read("Mn")?, read("Mx")?).ok_or("fan limits don't make sense")?;
        if let Err(error) = self.write_manual(fan, rpm) {
            // Manual mode may have taken before the speed failed, so don't leave it that way.
            self.release(fan);
            return Err(error.to_string());
        }
        if self.owned.insert(fan, rpm) != Some(rpm) {
            self.changed_at.insert(fan, Instant::now());
        }
        Ok(rpm)
    }

    fn write_manual(&self, fan: u8, rpm: f32) -> Result<(), Error> {
        self.smc.write_u8(&Self::mode_key(fan), MANUAL)?;
        self.smc.write_f32(&format!("F{fan}Tg"), rpm)
    }

    /// Runs once a second: enforces the thermal limit, retries failed releases, and restores
    /// anything macOS took back. With nothing under manual control, it costs nothing.
    fn safety_tick(&mut self) {
        for fan in self.pending_release.clone() {
            self.release(fan);
        }
        if self.owned.is_empty() {
            return;
        }
        if self.guard.update(self.hottest()) == Safety::ThermalHold {
            eprintln!("gauge-fand: thermal hold, returning fans to Auto");
            self.release_all();
            return;
        }
        let mut reverted = false;
        for (fan, rpm) in self.owned.clone() {
            if self.changed_at.get(&fan).is_some_and(|at| at.elapsed() < SETTLE_TIME) {
                continue;
            }
            let mode = self.smc.read_u8(&Self::mode_key(fan));
            let target = self.smc.read_f32(&format!("F{fan}Tg"));
            let held = mode == Ok(MANUAL)
                && target.is_ok_and(|t| (t - rpm).abs() <= TARGET_TOLERANCE_RPM);
            if !held {
                reverted = true;
                self.reverts += 1;
                if self.write_manual(fan, rpm).is_err() {
                    eprintln!("gauge-fand: fan {fan} refused a re-apply, returning it to Auto");
                    self.release(fan);
                }
            }
        }
        self.consecutive_reverts = if reverted { self.consecutive_reverts + 1 } else { 0 };
        if self.consecutive_reverts >= MAX_CONSECUTIVE_REVERTS {
            eprintln!("gauge-fand: macOS keeps taking the fans back, leaving them with it");
            self.consecutive_reverts = 0;
            self.release_all();
        }
    }

    fn status(&self) -> String {
        let hold = u8::from(self.guard.state == Safety::ThermalHold);
        let mut line = format!("STATUS hold={hold} reverts={}", self.reverts);
        for fan in 0..self.fans {
            let mode = self.smc.read_u8(&Self::mode_key(fan)).unwrap_or(255);
            line.push_str(&format!(" fan{fan}={mode}"));
        }
        line
    }

    fn handle(&mut self, line: &str) -> String {
        match parse_command(line) {
            Ok(Command::Beat) => "OK".into(),
            Ok(Command::Status) => self.status(),
            Ok(Command::Auto(fan)) if fan < self.fans => {
                if self.release(fan) { "OK".into() } else { "ERR restore failed".into() }
            }
            Ok(Command::Auto(_)) => "ERR no such fan".into(),
            Ok(Command::Set(fan, rpm)) => match self.apply(fan, rpm) {
                Ok(applied) => format!("OK {applied:.0}"),
                Err(e) => format!("ERR {e}"),
            },
            Err(e) => format!("ERR {e}"),
        }
    }
}

fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: the descriptor is open, and both out pointers are valid.
    let code = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    (code == 0).then_some(uid)
}

fn bind(allow_uid: libc::uid_t) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(SOCKET_PATH) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(SOCKET_PATH)?;
    let path = CString::new(SOCKET_PATH).map_err(std::io::Error::other)?;
    // SAFETY: path is a valid C string for both calls.
    unsafe {
        if libc::chown(path.as_ptr(), allow_uid, 0) != 0 || libc::chmod(path.as_ptr(), 0o600) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    listener.set_nonblocking(true)?;
    Ok(listener)
}

struct Client {
    stream: UnixStream,
    buffer: Vec<u8>,
    last_message: Instant,
}

/// Returns false when the client should be dropped.
fn service_client(client: &mut Client, daemon: &mut Daemon) -> bool {
    let mut chunk = [0u8; 256];
    match client.stream.read(&mut chunk) {
        Ok(0) => return false,
        Ok(n) => client.buffer.extend_from_slice(&chunk[..n]),
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {}
        Err(_) => return false,
    }
    for _ in 0..MAX_COMMANDS_PER_TICK {
        let Some(end) = client.buffer.iter().position(|b| *b == b'\n') else { break };
        let line: Vec<u8> = client.buffer.drain(..=end).collect();
        client.last_message = Instant::now();
        let reply = daemon.handle(String::from_utf8_lossy(&line).trim());
        if client.stream.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return false;
        }
    }
    let has_full_line = client.buffer.contains(&b'\n');
    client.buffer.len() <= MAX_BUFFERED_BYTES && (has_full_line || client.buffer.len() <= MAX_LINE_BYTES)
}

/// Newer chips use different fan keys, and these write paths are only tested on the older ones.
fn is_supported(smc: &Smc) -> bool {
    smc.info("Ftst").is_err() && smc.read_u8("F0Md").is_ok()
}

fn run(allow_uid: libc::uid_t) -> Result<(), String> {
    let mut daemon = Daemon::open().map_err(|e| e.to_string())?;
    daemon.release_all();
    if !is_supported(&daemon.smc) {
        eprintln!("gauge-fand: fan control isn't supported on this Mac");
        std::process::exit(EXIT_UNSUPPORTED);
    }
    let listener = bind(allow_uid).map_err(|e| format!("bind: {e}"))?;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
    }
    let mut client: Option<Client> = None;
    let mut last_safety = Instant::now();
    while !STOP.load(Ordering::SeqCst) {
        if let Ok((stream, _)) = listener.accept()
            && peer_uid(&stream) == Some(allow_uid)
            && stream.set_nonblocking(true).is_ok()
        {
            if client.is_some() {
                daemon.release_all();
            }
            client = Some(Client { stream, buffer: Vec::new(), last_message: Instant::now() });
        }
        let keep = client.as_mut().is_none_or(|c| {
            service_client(c, &mut daemon)
                && c.last_message.elapsed().as_secs_f32() < CLIENT_TIMEOUT_SECONDS
        });
        if !keep {
            client = None;
            daemon.release_all();
        }
        if last_safety.elapsed() >= SAFETY_EVERY {
            last_safety = Instant::now();
            daemon.safety_tick();
        }
        std::thread::sleep(TICK);
    }
    daemon.release_all();
    let _ = std::fs::remove_file(SOCKET_PATH);
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let allow_uid = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--check"] => {
            let supported = Smc::open().is_ok_and(|smc| is_supported(&smc));
            std::process::exit(if supported { 0 } else { 3 });
        }
        ["--allow-uid", uid] => uid.parse::<libc::uid_t>().ok(),
        _ => None,
    };
    let Some(allow_uid) = allow_uid else {
        eprintln!("usage: gauge-fand --allow-uid UID | --check");
        std::process::exit(2);
    };
    if let Err(message) = run(allow_uid) {
        eprintln!("gauge-fand: {message}");
        std::process::exit(1);
    }
}
