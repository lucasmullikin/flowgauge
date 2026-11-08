//! Fan curves: temperature to percent of a fan's range, with hysteresis and limited ramping so
//! fans don't hunt up and down as temperatures wobble.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub temp_c: f32,
    pub percent: f32,
}

const HYSTERESIS_C: f32 = 3.0;
const MAX_DROP_PERCENT_PER_UPDATE: f32 = 10.0;

/// Below the first point uses its percent, above the last uses its percent. Unsorted or empty
/// curves fail safe to 100%.
pub fn percent_for(points: &[CurvePoint], temp_c: f32) -> f32 {
    let sorted = points.windows(2).all(|p| p[0].temp_c < p[1].temp_c);
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return 100.0;
    };
    if !sorted {
        return 100.0;
    }
    if temp_c <= first.temp_c {
        return first.percent;
    }
    if temp_c >= last.temp_c {
        return last.percent;
    }
    let pair = points
        .windows(2)
        .find(|p| temp_c <= p[1].temp_c)
        .expect("temp is inside the curve");
    let share = (temp_c - pair[0].temp_c) / (pair[1].temp_c - pair[0].temp_c);
    pair[0].percent + share * (pair[1].percent - pair[0].percent)
}

pub fn rpm_for_percent(percent: f32, min_rpm: f32, max_rpm: f32) -> f32 {
    min_rpm + percent.clamp(0.0, 100.0) / 100.0 * (max_rpm - min_rpm)
}

/// Remembers recent state. Speeds up immediately on heat, because cooling must never lag behind
/// a sudden load, but only slows down gradually once past the hysteresis band.
#[derive(Debug, Default)]
pub struct CurveFollower {
    percent: Option<f32>,
    temp_at_last_rise: f32,
}

impl CurveFollower {
    pub fn update(&mut self, points: &[CurvePoint], temp_c: f32) -> f32 {
        let wanted = percent_for(points, temp_c);
        let Some(current) = self.percent else {
            self.percent = Some(wanted);
            self.temp_at_last_rise = temp_c;
            return wanted;
        };
        let next = if wanted >= current {
            self.temp_at_last_rise = temp_c;
            wanted
        } else if temp_c <= self.temp_at_last_rise - HYSTERESIS_C {
            wanted.max(current - MAX_DROP_PERCENT_PER_UPDATE)
        } else {
            current
        };
        self.percent = Some(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> Vec<CurvePoint> {
        vec![
            CurvePoint { temp_c: 40.0, percent: 0.0 },
            CurvePoint { temp_c: 80.0, percent: 100.0 },
        ]
    }

    #[test]
    fn interpolates_between_points_and_holds_at_the_ends() {
        assert_eq!(percent_for(&curve(), 20.0), 0.0);
        assert_eq!(percent_for(&curve(), 60.0), 50.0);
        assert_eq!(percent_for(&curve(), 95.0), 100.0);
    }

    #[test]
    fn broken_curves_fail_to_full_speed() {
        assert_eq!(percent_for(&[], 50.0), 100.0);
        let unsorted = [
            CurvePoint { temp_c: 80.0, percent: 0.0 },
            CurvePoint { temp_c: 40.0, percent: 10.0 },
        ];
        assert_eq!(percent_for(&unsorted, 50.0), 100.0);
    }

    #[test]
    fn percent_maps_onto_the_fans_range() {
        assert_eq!(rpm_for_percent(0.0, 1500.0, 5500.0), 1500.0);
        assert_eq!(rpm_for_percent(50.0, 1500.0, 5500.0), 3500.0);
        assert_eq!(rpm_for_percent(150.0, 1500.0, 5500.0), 5500.0);
    }

    #[test]
    fn sudden_heat_gets_full_speed_immediately() {
        let mut follower = CurveFollower::default();
        assert_eq!(follower.update(&curve(), 40.0), 0.0);
        assert_eq!(follower.update(&curve(), 80.0), 100.0);
    }

    #[test]
    fn cooling_slows_down_in_steps() {
        let mut follower = CurveFollower::default();
        assert_eq!(follower.update(&curve(), 80.0), 100.0);
        assert_eq!(follower.update(&curve(), 40.0), 90.0);
        assert_eq!(follower.update(&curve(), 40.0), 80.0);
    }

    #[test]
    fn small_drops_hold_speed_until_past_the_hysteresis_band() {
        let mut follower = CurveFollower::default();
        assert_eq!(follower.update(&curve(), 60.0), 50.0);
        assert_eq!(follower.update(&curve(), 58.0), 50.0);
        assert_eq!(follower.update(&curve(), 56.0), 40.0);
    }
}
