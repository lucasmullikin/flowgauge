//! Small graph for the menu bar, drawn as a template image so macOS tints it to match the bar.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{NSBezierPath, NSColor, NSImage};
use objc2_foundation::{NSPoint, NSRect, NSSize};

const WIDTH: f64 = 36.0;
const HEIGHT: f64 = 16.0;
const FAINT_SERIES_ALPHA: f64 = 0.45;

fn draw_bars(shares: &[f32]) {
    let step = WIDTH / shares.len().max(1) as f64;
    for (index, share) in shares.iter().enumerate() {
        let height = (f64::from(*share) * HEIGHT).max(0.5);
        NSBezierPath::bezierPathWithRect(NSRect::new(
            NSPoint::new(index as f64 * step, 0.0),
            NSSize::new(step, height),
        ))
        .fill();
    }
}

/// Values are 0 to 1 shares, oldest first. With two series, like download and upload, the first
/// is drawn fainter behind the second, since a template image can only vary by opacity.
// lockFocus is deprecated, but the handler-based replacement needs blocks for no real gain here.
#[allow(deprecated)]
pub fn image(series: &[Vec<f32>]) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(WIDTH, HEIGHT));
    image.lockFocus();
    NSColor::blackColor().colorWithAlphaComponent(0.25).setStroke();
    NSBezierPath::bezierPathWithRect(NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT))).stroke();
    for (index, shares) in series.iter().enumerate() {
        let faint = index == 0 && series.len() > 1;
        NSColor::blackColor().colorWithAlphaComponent(if faint { FAINT_SERIES_ALPHA } else { 1.0 }).setFill();
        draw_bars(shares);
    }
    image.unlockFocus();
    image.setTemplate(true);
    image
}
