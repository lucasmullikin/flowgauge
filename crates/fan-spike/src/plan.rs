//! Pure decisions for the fan test, kept apart from hardware so they can be tested.

pub const PREFLIGHT_MAX_C: f32 = 85.0;
pub const ABORT_C: f32 = 90.0;
const SENSOR_MIN_C: f32 = 15.0;
const SENSOR_MAX_C: f32 = 115.0;
const REACHED_TOLERANCE: f32 = 0.05;
const TARGET_TOLERANCE_RPM: f32 = 1.0;
const MANUAL: u8 = 1;
/// Must exceed the reached band, so a fan that never responds can't look like success.
const MIN_RAISE_RPM: f32 = 500.0;

/// The test may only make a fan clearly faster than it already is, within its rated range.
pub fn validate_target(rpm: f32, min: f32, max: f32, current: f32) -> Result<f32, String> {
    if !rpm.is_finite() {
        return Err("target is not a number".into());
    }
    if rpm < min || rpm > max {
        return Err(format!("{rpm} rpm is outside this fan's {min}-{max} range"));
    }
    if rpm < current + MIN_RAISE_RPM {
        return Err(format!(
            "{rpm} rpm is not {MIN_RAISE_RPM} above the current {current} rpm"
        ));
    }
    Ok(rpm)
}

/// Hottest plausible reading. `None` means no usable sensor, which callers treat as unsafe.
pub fn hottest(readings: impl IntoIterator<Item = (String, f32)>) -> Option<(String, f32)> {
    readings
        .into_iter()
        .filter(|(_, c)| c.is_finite() && (SENSOR_MIN_C..=SENSOR_MAX_C).contains(c))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Records whether the manual setting stuck during the hold.
#[derive(Debug, Default)]
pub struct Observations {
    requested: f32,
    mode_was_manual: bool,
    target_was_held: bool,
    pub mode_reverts: u32,
    pub target_reverts: u32,
    pub first_mode_revert_at: Option<f32>,
    pub first_target_revert_at: Option<f32>,
    pub reached_at: Option<f32>,
    in_band_at_end: bool,
}

impl Observations {
    pub fn new(requested: f32) -> Self {
        Observations {
            requested,
            mode_was_manual: true,
            target_was_held: true,
            ..Default::default()
        }
    }

    pub fn observe(&mut self, t: f32, mode: u8, target: f32, actual: f32) {
        let manual = mode == MANUAL;
        if self.mode_was_manual && !manual {
            self.mode_reverts += 1;
            self.first_mode_revert_at.get_or_insert(t);
        }
        self.mode_was_manual = manual;

        let held = (target - self.requested).abs() <= TARGET_TOLERANCE_RPM;
        if self.target_was_held && !held {
            self.target_reverts += 1;
            self.first_target_revert_at.get_or_insert(t);
        }
        self.target_was_held = held;

        self.in_band_at_end = (actual - self.requested).abs() <= self.requested * REACHED_TOLERANCE;
        if self.in_band_at_end {
            self.reached_at.get_or_insert(t);
        }
    }

    pub fn held(&self) -> bool {
        self.mode_reverts == 0 && self.target_reverts == 0 && self.in_band_at_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_must_be_in_range_and_not_slow_the_fan() {
        assert_eq!(validate_target(2500.0, 1499.0, 5348.0, 1523.0), Ok(2500.0));
        assert!(validate_target(1000.0, 1499.0, 5348.0, 900.0).is_err());
        assert!(validate_target(6000.0, 1499.0, 5348.0, 1523.0).is_err());
        assert!(validate_target(2000.0, 1499.0, 5348.0, 2600.0).is_err());
        assert!(validate_target(1550.0, 1499.0, 5348.0, 1523.0).is_err());
        assert!(validate_target(f32::NAN, 1499.0, 5348.0, 1523.0).is_err());
    }

    #[test]
    fn hottest_ignores_implausible_readings() {
        let got = hottest([
            ("Ta".to_string(), 40.0),
            ("Tb".to_string(), 129.0),
            ("Tc".to_string(), f32::NAN),
            ("Td".to_string(), 0.0),
            ("Te".to_string(), 62.5),
        ]);
        assert_eq!(got, Some(("Te".to_string(), 62.5)));
    }

    #[test]
    fn hottest_is_none_without_usable_sensors() {
        assert_eq!(hottest([("Tb".to_string(), 129.0)]), None);
    }

    #[test]
    fn steady_manual_setting_counts_as_held() {
        let mut o = Observations::new(2500.0);
        o.observe(0.1, 1, 2500.0, 1600.0);
        o.observe(4.0, 1, 2500.0, 2450.0);
        o.observe(8.0, 1, 2500.0, 2500.0);
        assert!(o.held());
        assert_eq!(o.reached_at, Some(4.0));
    }

    #[test]
    fn mode_revert_is_recorded_with_its_time() {
        let mut o = Observations::new(2500.0);
        o.observe(0.1, 1, 2500.0, 2500.0);
        o.observe(1.4, 3, 2500.0, 2500.0);
        o.observe(1.5, 3, 2500.0, 2500.0);
        assert_eq!(o.mode_reverts, 1);
        assert_eq!(o.first_mode_revert_at, Some(1.4));
        assert!(!o.held());
    }

    #[test]
    fn target_clobbering_counts_each_revert() {
        let mut o = Observations::new(2500.0);
        o.observe(0.1, 1, 2500.0, 2500.0);
        o.observe(1.4, 1, 1600.0, 2500.0);
        o.observe(1.5, 1, 2500.0, 2500.0);
        o.observe(2.8, 1, 1600.0, 2500.0);
        assert_eq!(o.target_reverts, 2);
        assert_eq!(o.first_target_revert_at, Some(1.4));
        assert!(!o.held());
    }

    #[test]
    fn falling_away_after_reaching_target_is_not_held() {
        let mut o = Observations::new(2500.0);
        o.observe(4.0, 1, 2500.0, 2500.0);
        o.observe(8.0, 1, 2500.0, 1700.0);
        assert_eq!(o.reached_at, Some(4.0));
        assert!(!o.held());
    }

    #[test]
    fn never_reaching_target_is_not_held() {
        let mut o = Observations::new(2500.0);
        o.observe(0.1, 1, 2500.0, 1600.0);
        assert!(!o.held());
    }
}
