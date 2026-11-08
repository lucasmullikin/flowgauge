//! Calm motion for the Network views: soft points of light drifting along a path. Core
//! Animation runs it in the window server, so FlowGauge does no drawing per frame. Nothing moves
//! when Reduce Motion is on.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSBezierPath, NSColor, NSWorkspace};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSArray, NSNumber, NSObject, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CACurrentMediaTime, CAKeyframeAnimation, CALayer, CAMediaTiming, kCAAnimationPaced};

pub fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub fn cg_color((r, g, b): (f64, f64, f64), alpha: f64) -> Retained<CGColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha).CGColor()
}

/// The same S-curve the ribbons use, from one point to another.
pub fn curve(from: NSPoint, to: NSPoint) -> Retained<CGPath> {
    let middle = (from.x + to.x) / 2.0;
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(from);
    path.curveToPoint_controlPoint1_controlPoint2(to, NSPoint::new(middle, from.y), NSPoint::new(middle, to.y));
    path.CGPath()
}

/// A straight line, for the path check's probe.
pub fn line(from: NSPoint, to: NSPoint) -> Retained<CGPath> {
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(from);
    path.lineToPoint(to);
    path.CGPath()
}

fn dot(color: (f64, f64, f64), size: f64) -> Retained<CALayer> {
    let layer = CALayer::new();
    layer.setBounds(NSRect::new(NSPoint::ZERO, NSSize::new(size, size)));
    layer.setCornerRadius(size / 2.0);
    layer.setBackgroundColor(Some(&cg_color(color, 1.0)));
    layer.setShadowColor(Some(&cg_color(color, 1.0)));
    layer.setShadowOpacity(0.9);
    layer.setShadowRadius(size);
    layer.setShadowOffset(NSSize::ZERO);
    // Hidden unless an animation is showing it, so a dot never sits still at the origin.
    layer.setOpacity(0.0);
    layer
}

fn numbers(values: &[f64]) -> Retained<NSArray<NSNumber>> {
    let numbers: Vec<Retained<NSNumber>> = values.iter().map(|v| NSNumber::new_f64(*v)).collect();
    NSArray::from_retained_slice(&numbers)
}

/// Moves along `path` and fades in and out at the ends.
fn travel(layer: &CALayer, path: &CGPath, duration: f64, offset: f64, repeat: f32, peak: f64) {
    let position = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("position")));
    position.setPath(Some(path));
    // SAFETY: the constant is a valid calculation mode for the process's lifetime.
    position.setCalculationMode(unsafe { kCAAnimationPaced });
    let opacity = CAKeyframeAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
    let values: Vec<Retained<AnyObject>> = [0.0, peak, peak, 0.0]
        .iter()
        .map(|v| {
            let number: Retained<NSObject> = Retained::into_super(Retained::into_super(NSNumber::new_f64(*v)));
            Retained::into_super(number)
        })
        .collect();
    // SAFETY: opacity keyframes are numbers, which is what the key path expects.
    unsafe { opacity.setValues(Some(&NSArray::from_retained_slice(&values))) };
    opacity.setKeyTimes(Some(&numbers(&[0.0, 0.18, 0.82, 1.0])));
    for (animation, key) in [(&*position, "position"), (&*opacity, "opacity")] {
        animation.setDuration(duration);
        animation.setRepeatCount(repeat);
        animation.setTimeOffset(offset);
        layer.addAnimation_forKey(animation, Some(&NSString::from_str(key)));
    }
}

/// Adds `count` dots spread evenly along `path`, looping forever. Their place along the path
/// comes from the clock, so rebuilding with the same timing doesn't make them jump back.
pub fn drift(container: &CALayer, path: &CGPath, count: usize, duration: f64, color: (f64, f64, f64), size: f64) {
    let now = CACurrentMediaTime();
    for k in 0..count {
        let layer = dot(color, size);
        let phase = (now / duration + k as f64 / count as f64).fract();
        travel(&layer, path, duration, phase * duration, f32::INFINITY, 0.85);
        container.addSublayer(&layer);
    }
}

/// One dot along `path`, once.
pub fn pulse(container: &CALayer, path: &CGPath, duration: f64, color: (f64, f64, f64), size: f64) {
    let layer = dot(color, size);
    travel(&layer, path, duration, 0.0, 1.0, 0.95);
    container.addSublayer(&layer);
}

/// How many dots a stream of `bytes_per_second` gets, and how long each takes to cross. Both
/// grow slowly with the rate, so a fast download stays calm.
pub fn pace(bytes_per_second: f64) -> (usize, f64) {
    const QUIET: f64 = 2048.0;
    if bytes_per_second < QUIET {
        return (0, 0.0);
    }
    let level = (bytes_per_second / QUIET).log2();
    let count = (level / 2.2).round().clamp(1.0, 6.0) as usize;
    let duration = (5.0 / (1.0 + level * 0.07)).clamp(2.2, 5.0);
    (count, (duration * 4.0).round() / 4.0)
}

pub fn clear(container: &CALayer) {
    // SAFETY: an empty or missing array of sublayers is always valid.
    unsafe { container.setSublayers(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pace_grows_slowly_and_stops_when_quiet() {
        assert_eq!(pace(100.0), (0, 0.0));
        let slow = pace(20_000.0);
        let fast = pace(100_000_000.0);
        assert!(slow.0 >= 1 && fast.0 <= 6 && fast.0 > slow.0, "{slow:?} {fast:?}");
        assert!(fast.1 < slow.1 && fast.1 >= 2.2);
    }

    #[test]
    fn pace_changes_in_steps_so_motion_isnt_rebuilt_every_second() {
        assert_eq!(pace(1_000_000.0), pace(1_050_000.0));
    }
}
