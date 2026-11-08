//! Line protocol between the app and the root fan helper, plus the helper's safety rules.
//! Kept free of I/O and dependencies so the root process stays small and this stays testable.

pub const SOCKET_PATH: &str = "/var/run/gauge-fand.sock";
pub const MAX_LINE_BYTES: usize = 64;
pub const CLIENT_TIMEOUT_SECONDS: f32 = 5.0;
/// macOS itself reached 95.5C on this Mac at full fans under load, so normal heavy use stays
/// below this. Tripping hands the fans back to macOS.
pub const THERMAL_LIMIT_C: f32 = 100.0;
pub const THERMAL_RESUME_C: f32 = 90.0;
pub const THERMAL_TRIP_TICKS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    Beat,
    Status,
    Auto(u8),
    Set(u8, f32),
}

pub fn parse_command(line: &str) -> Result<Command, String> {
    if line.len() > MAX_LINE_BYTES {
        return Err("line too long".into());
    }
    let fan = |s: &str| {
        s.parse::<u8>()
            .ok()
            .filter(|f| *f < 16)
            .ok_or_else(|| "bad fan".to_string())
    };
    let parts: Vec<&str> = line.split_ascii_whitespace().collect();
    match parts.as_slice() {
        ["BEAT"] => Ok(Command::Beat),
        ["STATUS"] => Ok(Command::Status),
        ["AUTO", f] => Ok(Command::Auto(fan(f)?)),
        ["SET", f, rpm] => {
            let rpm: f32 = rpm.parse().map_err(|_| "bad rpm".to_string())?;
            if !rpm.is_finite() {
                return Err("bad rpm".into());
            }
            Ok(Command::Set(fan(f)?, rpm))
        }
        _ => Err("unknown command".into()),
    }
}

/// Requests are clamped to the fan's own limits, so a fan can never be asked to stop. Limits
/// that don't make sense are refused, since acting on them could stop a fan or panic here.
pub fn clamp_rpm(rpm: f32, min: f32, max: f32) -> Option<f32> {
    (min.is_finite() && max.is_finite() && min > 0.0 && min <= max).then(|| rpm.clamp(min, max))
}

const BATTERY_LIMIT_C: f32 = 60.0;
const SSD_LIMIT_C: f32 = 85.0;

/// Puts a reading on the scale of `THERMAL_LIMIT_C`. Batteries and SSDs are hurt by heat a CPU
/// shrugs off, so they trip the guard at their own, lower temperatures.
pub fn severity(key: &str, celsius: f32) -> f32 {
    let limit = match key.get(..2) {
        Some("TB") => BATTERY_LIMIT_C,
        Some("TH") => SSD_LIMIT_C,
        _ => THERMAL_LIMIT_C,
    };
    celsius * THERMAL_LIMIT_C / limit
}

/// After this many one-second checks in a row with macOS taking a fan back, stop fighting it.
pub const MAX_CONSECUTIVE_REVERTS: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Safety {
    Normal,
    /// Fans are back with macOS; requests are refused until things cool down.
    ThermalHold,
}

/// A brief spike on one sensor shouldn't trip this, so it takes several hot ticks in a row.
/// Losing every sensor counts as hot.
#[derive(Debug)]
pub struct ThermalGuard {
    pub state: Safety,
    hot_ticks: u32,
}

impl Default for ThermalGuard {
    fn default() -> Self {
        ThermalGuard { state: Safety::Normal, hot_ticks: 0 }
    }
}

impl ThermalGuard {
    pub fn update(&mut self, hottest_c: Option<f32>) -> Safety {
        match hottest_c {
            Some(c) if c < THERMAL_LIMIT_C => {
                self.hot_ticks = 0;
                if self.state == Safety::ThermalHold && c < THERMAL_RESUME_C {
                    self.state = Safety::Normal;
                }
            }
            _ => {
                self.hot_ticks += 1;
                if self.hot_ticks >= THERMAL_TRIP_TICKS {
                    self.state = Safety::ThermalHold;
                }
            }
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_commands() {
        assert_eq!(parse_command("BEAT"), Ok(Command::Beat));
        assert_eq!(parse_command("STATUS\n"), Ok(Command::Status));
        assert_eq!(parse_command("AUTO 1"), Ok(Command::Auto(1)));
        assert_eq!(parse_command("SET 0 2500"), Ok(Command::Set(0, 2500.0)));
    }

    #[test]
    fn rejects_everything_else() {
        for line in ["", "SET 0", "SET 0 NaN", "SET 0 inf", "SET 99 2000", "SET x 2", "AUTO", "rm -rf", "SET 0 1 2", "LIMIT 80", "LIMIT OFF"] {
            assert!(parse_command(line).is_err(), "{line}");
        }
        assert!(parse_command(&"B".repeat(MAX_LINE_BYTES + 1)).is_err());
    }

    #[test]
    fn clamps_to_the_fans_range() {
        assert_eq!(clamp_rpm(0.0, 1499.0, 5348.0), Some(1499.0));
        assert_eq!(clamp_rpm(9000.0, 1499.0, 5348.0), Some(5348.0));
        assert_eq!(clamp_rpm(2500.0, 1499.0, 5348.0), Some(2500.0));
    }

    #[test]
    fn nonsense_limits_are_refused_rather_than_used() {
        assert_eq!(clamp_rpm(2000.0, 0.0, 5348.0), None, "a zero minimum could stop the fan");
        assert_eq!(clamp_rpm(2000.0, 5348.0, 1499.0), None, "this would panic in clamp");
        assert_eq!(clamp_rpm(2000.0, f32::NAN, 5348.0), None);
        assert_eq!(clamp_rpm(2000.0, 1499.0, f32::INFINITY), None);
    }

    #[test]
    fn batteries_and_ssds_trip_at_lower_temperatures() {
        assert_eq!(severity("Tp0D", 100.0), 100.0);
        assert_eq!(severity("TB1T", 60.0), 100.0);
        assert_eq!(severity("TH0x", 85.0), 100.0);
        assert!(severity("TB1T", 35.0) < THERMAL_RESUME_C);
    }

    #[test]
    fn one_hot_reading_does_not_trip() {
        let mut guard = ThermalGuard::default();
        assert_eq!(guard.update(Some(105.0)), Safety::Normal);
        assert_eq!(guard.update(Some(70.0)), Safety::Normal);
        assert_eq!(guard.update(Some(105.0)), Safety::Normal);
    }

    #[test]
    fn sustained_heat_trips_and_only_cooling_clears_it() {
        let mut guard = ThermalGuard::default();
        for _ in 0..THERMAL_TRIP_TICKS {
            guard.update(Some(101.0));
        }
        assert_eq!(guard.state, Safety::ThermalHold);
        assert_eq!(guard.update(Some(95.0)), Safety::ThermalHold);
        assert_eq!(guard.update(Some(89.0)), Safety::Normal);
    }

    #[test]
    fn losing_all_sensors_counts_as_hot() {
        let mut guard = ThermalGuard::default();
        for _ in 0..THERMAL_TRIP_TICKS {
            guard.update(None);
        }
        assert_eq!(guard.state, Safety::ThermalHold);
    }
}
