//! Client side of fan control: turns the active preset into helper commands and reports what
//! actually happened. The helper enforces the safety rules; this only asks.

use fan_proto::SOCKET_PATH;
use gauge_core::curve::{CurveFollower, rpm_for_percent};
use gauge_core::sensors::Fan;
use gauge_core::settings::FanMode;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FanState {
    #[default]
    Auto,
    Holding,
    Overridden,
    ThermalHold,
    HelperMissing,
    /// The helper didn't answer in time. Dropping the connection returns fans to Auto.
    HelperBusy,
    Error(String),
}

impl FanState {
    pub fn label(&self) -> String {
        match self {
            FanState::Auto => "macOS is in control".into(),
            FanState::Holding => "Holding your setting".into(),
            FanState::Overridden => "macOS keeps taking control back".into(),
            FanState::ThermalHold => "Too hot, handed back to macOS".into(),
            FanState::HelperMissing => "Fan helper not installed".into(),
            FanState::HelperBusy => "Fan helper didn't answer, trying again".into(),
            FanState::Error(message) => format!("Error: {message}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Default)]
pub struct HelperStatus {
    pub hold: bool,
    pub reverts: u64,
}

pub fn parse_status(line: &str) -> Option<HelperStatus> {
    let mut fields = line.split_ascii_whitespace();
    if fields.next() != Some("STATUS") {
        return None;
    }
    let mut status = HelperStatus::default();
    for field in fields {
        match field.split_once('=') {
            Some(("hold", value)) => status.hold = value == "1",
            Some(("reverts", value)) => status.reverts = value.parse().ok()?,
            _ => {}
        }
    }
    Some(status)
}

/// Compares against the previous revert count, since the helper's total never resets.
pub fn state_from_status(status: &HelperStatus, previous_reverts: u64) -> FanState {
    if status.hold {
        FanState::ThermalHold
    } else if status.reverts > previous_reverts {
        FanState::Overridden
    } else {
        FanState::Holding
    }
}

/// Plain words for how the helper installer ended; -128 is AppleScript's "user cancelled".
pub fn install_result(success: bool, stderr: &str) -> String {
    if success {
        "Fan helper installed".to_string()
    } else if stderr.contains("-128") {
        "Install cancelled".to_string()
    } else {
        format!("Install failed: {}", stderr.trim())
    }
}

fn is_timeout(kind: std::io::ErrorKind) -> bool {
    matches!(kind, std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
}

struct Connection {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Connection {
    fn open() -> std::io::Result<Connection> {
        let writer = UnixStream::connect(SOCKET_PATH)?;
        writer.set_read_timeout(Some(IO_TIMEOUT))?;
        writer.set_write_timeout(Some(IO_TIMEOUT))?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Connection { writer, reader })
    }

    fn ask(&mut self, command: &str) -> std::io::Result<String> {
        self.writer.write_all(format!("{command}\n").as_bytes())?;
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        Ok(line.trim().to_string())
    }
}

#[derive(Default)]
pub struct FanDriver {
    connection: Option<Connection>,
    followers: HashMap<u8, CurveFollower>,
    /// The helper's total never resets, so a new connection starts from whatever it says then.
    reverts: Option<u64>,
}

impl FanDriver {
    pub fn is_controlling(&self) -> bool {
        self.connection.is_some()
    }

    /// Called every couple of seconds. Each request also tells the helper the app is alive.
    pub fn step(&mut self, mode: &FanMode, fans: &[Fan], hottest_c: Option<f32>) -> FanState {
        if *mode == FanMode::Auto {
            // Closing the connection makes the helper return every fan to Auto.
            self.connection = None;
            self.followers.clear();
            return FanState::Auto;
        }
        match self.apply(mode, fans, hottest_c) {
            Ok(state) => state,
            Err(error) => {
                self.connection = None;
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) {
                    FanState::HelperMissing
                } else if is_timeout(error.kind()) {
                    FanState::HelperBusy
                } else {
                    FanState::Error(error.to_string())
                }
            }
        }
    }

    fn apply(&mut self, mode: &FanMode, fans: &[Fan], hottest_c: Option<f32>) -> std::io::Result<FanState> {
        if self.connection.is_none() {
            self.connection = Some(Connection::open()?);
            self.reverts = None;
        }
        let mut requests = Vec::new();
        for fan in fans {
            let percent = match mode {
                FanMode::Auto => continue,
                FanMode::Fixed { percent } => *percent,
                // With no temperature reading, run flat out rather than guess.
                FanMode::Curve { points } => match hottest_c {
                    Some(c) => self.followers.entry(fan.index).or_default().update(points, c),
                    None => 100.0,
                },
            };
            requests.push((fan.index, rpm_for_percent(percent, fan.min_rpm, fan.max_rpm)));
        }
        let connection = self.connection.as_mut().ok_or(std::io::ErrorKind::NotConnected)?;
        let mut refused = None;
        for (index, rpm) in requests {
            let reply = connection.ask(&format!("SET {index} {rpm:.0}"))?;
            if let Some(reason) = reply.strip_prefix("ERR ") {
                refused = Some(reason.to_string());
            }
        }
        let status = parse_status(&connection.ask("STATUS")?).unwrap_or_default();
        let state = state_from_status(&status, self.reverts.unwrap_or(status.reverts));
        self.reverts = Some(status.reverts);
        Ok(match (refused, state) {
            (_, FanState::ThermalHold) => FanState::ThermalHold,
            (Some(reason), _) => FanState::Error(reason),
            (None, state) => state,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_outcomes_read_plainly() {
        assert_eq!(install_result(true, ""), "Fan helper installed");
        assert_eq!(install_result(false, "execution error: User canceled. (-128)"), "Install cancelled");
        assert_eq!(install_result(false, "boom\n"), "Install failed: boom");
    }

    #[test]
    fn parses_helper_status() {
        let status = parse_status("STATUS hold=1 reverts=7 fan0=1 fan1=0").unwrap();
        assert_eq!(status, HelperStatus { hold: true, reverts: 7 });
        assert_eq!(parse_status("OK"), None);
        assert_eq!(parse_status("STATUS reverts=x"), None);
    }

    #[test]
    fn new_reverts_mean_macos_overrode_the_setting() {
        let status = HelperStatus { hold: false, reverts: 3 };
        assert_eq!(state_from_status(&status, 2), FanState::Overridden);
        assert_eq!(state_from_status(&status, 3), FanState::Holding);
    }

    #[test]
    fn thermal_hold_takes_priority() {
        let status = HelperStatus { hold: true, reverts: 9 };
        assert_eq!(state_from_status(&status, 1), FanState::ThermalHold);
    }

    #[test]
    fn a_slow_helper_reads_as_busy_not_a_raw_error() {
        assert!(is_timeout(std::io::ErrorKind::WouldBlock));
        assert!(is_timeout(std::io::ErrorKind::TimedOut));
        assert!(!is_timeout(std::io::ErrorKind::NotFound));
        assert_eq!(FanState::HelperBusy.label(), "Fan helper didn't answer, trying again");
    }

    #[test]
    fn auto_never_needs_the_helper() {
        let mut driver = FanDriver::default();
        assert_eq!(driver.step(&FanMode::Auto, &[], None), FanState::Auto);
        assert!(!driver.is_controlling());
    }
}
