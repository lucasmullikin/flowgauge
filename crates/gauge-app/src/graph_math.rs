//! The arithmetic behind graphs, separate from drawing so it can be tested.

use gauge_core::settings::GraphScale;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Rate,
    Percent,
    Rpm,
    Celsius,
    Fahrenheit,
    Watts,
}

impl Unit {
    pub fn fixed_max(self) -> Option<f32> {
        (self == Unit::Percent).then_some(100.0)
    }
}

/// Rounds up to 1, 2 or 5 times a power of ten, so axis labels stay readable.
pub fn nice_max(value: f32) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f32.powf(value.log10().floor());
    let share = value / magnitude;
    let step = [1.0, 2.0, 5.0].into_iter().find(|s| share <= *s).unwrap_or(10.0);
    step * magnitude
}

const RATE_LOG_FLOOR: f32 = 1000.0;

impl Unit {
    fn lowest_log_floor(self) -> f32 {
        if self == Unit::Rate { RATE_LOG_FLOOR } else { 1.0 }
    }
}

/// The bottom of a log axis: the decade below the quietest reading. Starting at the data rather
/// than at 1 stops the graph being a solid wall with all its movement squeezed into the top.
pub fn log_floor(lowest_positive: Option<f32>, max: f32, unit: Unit) -> f32 {
    let floor = lowest_positive
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or(unit.lowest_log_floor(), |v| 10f32.powf(v.log10().floor()))
        .max(unit.lowest_log_floor());
    if floor >= max { max / 10.0 } else { floor }
}

/// Height share from 0 to 1. Log scale keeps small traffic visible beside large spikes.
pub fn fraction(value: f32, max: f32, floor: f32, scale: GraphScale) -> f32 {
    if max <= 0.0 {
        return 0.0;
    }
    let share = match scale {
        GraphScale::Linear => value / max,
        GraphScale::Log => {
            if value <= floor || max <= floor {
                0.0
            } else {
                (value / floor).log10() / (max / floor).log10()
            }
        }
    };
    share.clamp(0.0, 1.0)
}

/// Grid lines as values, ending at the top of the axis.
pub fn ticks(max: f32, floor: f32, scale: GraphScale) -> Vec<f32> {
    match scale {
        GraphScale::Linear => vec![max / 2.0, max],
        GraphScale::Log => {
            // A round number for the middle line, rather than the exact midpoint.
            let middle = 10f32.powf((floor * max).sqrt().log10().round());
            if middle > floor && middle < max { vec![floor, middle, max] } else { vec![floor, max] }
        }
    }
}

/// Index ranges of unbroken data, so gaps in time stay gaps on screen.
pub fn runs<T>(values: &[Option<T>]) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut start = None;
    for (index, value) in values.iter().enumerate() {
        match (value.is_some(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                found.push(from..index);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        found.push(from..values.len());
    }
    found
}

pub fn format_ago(seconds: i64) -> String {
    match seconds {
        ..=59 => format!("{}s ago", seconds.max(0)),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86399 => format!("{:.1}h ago", seconds as f64 / 3600.0),
        _ => format!("{:.1}d ago", seconds as f64 / 86400.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_max_rounds_up_to_readable_numbers() {
        assert_eq!(nice_max(0.0), 1.0);
        assert_eq!(nice_max(f32::NAN), 1.0);
        assert_eq!(nice_max(7.0), 10.0);
        assert_eq!(nice_max(1200.0), 2000.0);
        assert_eq!(nice_max(4100.0), 5000.0);
        assert_eq!(nice_max(5348.0), 10000.0);
    }

    #[test]
    fn linear_fraction_is_proportional_and_clamped() {
        assert_eq!(fraction(50.0, 100.0, 1.0, GraphScale::Linear), 0.5);
        assert_eq!(fraction(150.0, 100.0, 1.0, GraphScale::Linear), 1.0);
        assert_eq!(fraction(5.0, 0.0, 1.0, GraphScale::Linear), 0.0);
    }

    #[test]
    fn log_fraction_spans_the_floor_to_the_top() {
        assert_eq!(fraction(1000.0, 1e6, 1000.0, GraphScale::Log), 0.0);
        assert_eq!(fraction(0.0, 1e6, 1000.0, GraphScale::Log), 0.0);
        assert_eq!(fraction(1e6, 1e6, 1000.0, GraphScale::Log), 1.0);
        let middle = fraction(31_623.0, 1e6, 1000.0, GraphScale::Log);
        assert!((middle - 0.5).abs() < 0.01, "{middle}");
    }

    #[test]
    fn log_floor_starts_at_the_quietest_reading() {
        assert_eq!(log_floor(Some(500_000.0), 5e9, Unit::Rate), 100_000.0);
        assert_eq!(log_floor(Some(3.0), 5e9, Unit::Rate), 1000.0);
        assert_eq!(log_floor(None, 5e9, Unit::Rate), 1000.0);
        assert_eq!(log_floor(Some(1000.0), 1000.0, Unit::Rate), 100.0);
    }

    #[test]
    fn ticks_end_at_the_top() {
        assert_eq!(ticks(100.0, 1.0, GraphScale::Linear), [50.0, 100.0]);
        let log = ticks(5e6, 1000.0, GraphScale::Log);
        assert_eq!(log.len(), 3);
        assert!((log[1] - 100_000.0).abs() < 1.0, "{}", log[1]);
        assert_eq!(ticks(1000.0, 500.0, GraphScale::Log).len(), 2);
    }

    #[test]
    fn runs_split_at_gaps() {
        let values = [Some(1), Some(2), None, None, Some(3)];
        assert_eq!(runs(&values), [0..2, 4..5]);
        assert!(runs::<i32>(&[None]).is_empty());
    }

    #[test]
    fn ago_uses_a_sensible_unit() {
        assert_eq!(format_ago(5), "5s ago");
        assert_eq!(format_ago(600), "10m ago");
        assert_eq!(format_ago(5400), "1.5h ago");
        assert_eq!(format_ago(172800), "2.0d ago");
    }
}
