//! Decides when an alert should fire. A condition must hold for a while, fires once, and can
//! fire again only after clearing.

use std::collections::HashMap;

#[derive(Default)]
pub struct AlertTracker {
    since: HashMap<&'static str, i64>,
    fired: HashMap<&'static str, bool>,
}

impl AlertTracker {
    pub fn check(&mut self, name: &'static str, active: bool, hold_seconds: i64, now: i64) -> bool {
        if !active {
            self.since.remove(name);
            self.fired.remove(name);
            return false;
        }
        let since = *self.since.entry(name).or_insert(now);
        let already = self.fired.get(name).copied().unwrap_or(false);
        if !already && now - since >= hold_seconds {
            self.fired.insert(name, true);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_the_hold_time_then_fires_once() {
        let mut tracker = AlertTracker::default();
        assert!(!tracker.check("x", true, 10, 100));
        assert!(!tracker.check("x", true, 10, 109));
        assert!(tracker.check("x", true, 10, 110));
        assert!(!tracker.check("x", true, 10, 111));
    }

    #[test]
    fn clearing_rearms_and_restarts_the_hold() {
        let mut tracker = AlertTracker::default();
        tracker.check("x", true, 0, 100);
        assert!(!tracker.check("x", false, 0, 101));
        assert!(!tracker.check("x", true, 5, 102));
        assert!(tracker.check("x", true, 5, 107));
    }

    #[test]
    fn a_brief_condition_never_fires() {
        let mut tracker = AlertTracker::default();
        assert!(!tracker.check("x", true, 10, 100));
        assert!(!tracker.check("x", false, 10, 105));
        assert!(!tracker.check("x", true, 10, 106));
    }
}
