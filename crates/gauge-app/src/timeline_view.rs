//! A row of cells per app, brighter when it used more of the network.

use crate::flow_view::color;
use crate::net_flow_view::DOWN_COLOR;
use crate::net_view::Timeline;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSStringDrawing, NSView};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;

const NAME_WIDTH: f64 = 74.0;
const ROW_HEIGHT: f64 = 16.0;
const ROW_GAP: f64 = 4.0;
const CELL_GAP: f64 = 1.0;
const AXIS_HEIGHT: f64 = 14.0;
const MIN_ALPHA: f64 = 0.06;

/// Cells are scaled against the busiest one on a square root, so quiet hours stay visible.
pub fn cell_alpha(value: f64, busiest: f64) -> f64 {
    if busiest <= 0.0 || value <= 0.0 {
        return MIN_ALPHA;
    }
    (MIN_ALPHA + (value / busiest).sqrt() * (1.0 - MIN_ALPHA)).min(1.0)
}

pub fn height_for(rows: usize) -> f64 {
    rows as f64 * (ROW_HEIGHT + ROW_GAP) + AXIS_HEIGHT
}

fn text(string: &str, point: NSPoint, align_right: bool) {
    let font = NSFont::systemFontOfSize(10.5);
    let foreground = NSColor::secondaryLabelColor();
    // SAFETY: the attribute statics are valid, and this only runs while the view is drawing.
    unsafe {
        let attributes = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, foreground.as_ref() as &AnyObject],
        );
        let mut shown = string.to_string();
        let mut chars: Vec<char> = string.chars().collect();
        let mut ns = NSString::from_str(&shown);
        while ns.sizeWithAttributes(Some(&attributes)).width > NAME_WIDTH - 6.0 && chars.len() > 1 {
            chars.pop();
            shown = format!("{}…", chars.iter().collect::<String>().trim_end());
            ns = NSString::from_str(&shown);
        }
        let width = ns.sizeWithAttributes(Some(&attributes)).width;
        let x = if align_right { point.x - width } else { point.x };
        ns.drawAtPoint_withAttributes(NSPoint::new(x, point.y), Some(&attributes));
    }
}

pub struct TimelineIvars {
    timeline: RefCell<Option<Timeline>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeTimelineView"]
    #[ivars = TimelineIvars]
    pub struct TimelineView;

    impl TimelineView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

impl TimelineView {
    pub fn new(mtm: MainThreadMarker, width: f64, rows: usize) -> Retained<Self> {
        let height = height_for(rows);
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(TimelineIvars { timeline: RefCell::new(None) });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view
    }

    pub fn set_timeline(&self, timeline: Timeline) {
        if self.ivars().timeline.borrow().as_ref() != Some(&timeline) {
            *self.ivars().timeline.borrow_mut() = Some(timeline);
            self.setNeedsDisplay(true);
        }
    }

    fn draw(&self) {
        let size = self.bounds().size;
        let timeline = self.ivars().timeline.borrow();
        let Some(timeline) = timeline.as_ref() else { return };
        if timeline.rows.is_empty() {
            text("No app traffic saved yet", NSPoint::new(0.0, size.height / 2.0), false);
            return;
        }
        let busiest = timeline.rows.iter().flat_map(|(_, cells)| cells.iter()).fold(0.0, |a: f64, b| a.max(*b));
        let cells = timeline.rows[0].1.len().max(1);
        let cell_width = (size.width - NAME_WIDTH) / cells as f64;
        for (row, (name, values)) in timeline.rows.iter().enumerate() {
            let y = size.height - (row as f64 + 1.0) * (ROW_HEIGHT + ROW_GAP) + ROW_GAP;
            text(name, NSPoint::new(0.0, y + 1.0), false);
            for (i, value) in values.iter().enumerate() {
                let rect = NSRect::new(
                    NSPoint::new(NAME_WIDTH + i as f64 * cell_width, y),
                    NSSize::new((cell_width - CELL_GAP).max(0.5), ROW_HEIGHT),
                );
                color(DOWN_COLOR, cell_alpha(*value, busiest)).setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 1.5, 1.5).fill();
            }
        }
        text(&format!("{} ago", timeline.range_label), NSPoint::new(NAME_WIDTH, 0.0), false);
        text("now", NSPoint::new(size.width, 0.0), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_cells_stay_faint_and_the_busiest_is_full() {
        assert_eq!(cell_alpha(0.0, 100.0), MIN_ALPHA);
        assert_eq!(cell_alpha(100.0, 100.0), 1.0);
        assert!(cell_alpha(1.0, 100.0) > MIN_ALPHA * 2.0);
        assert_eq!(cell_alpha(5.0, 0.0), MIN_ALPHA);
    }
}
