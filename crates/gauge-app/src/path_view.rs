//! The steps from this Mac to the internet, colored by how each one is answering. A small
//! probe travels along the steps after every check.

use crate::flow_view::color;
use crate::motion;
use crate::net_view::{PathStepView, PathView};
use gauge_core::pathcheck::Health;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSStringDrawing, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;
use std::cell::{Cell, RefCell};

const GOOD: (f64, f64, f64) = (0.11, 0.62, 0.46);
const SLOW: (f64, f64, f64) = (0.94, 0.66, 0.24);
const DOWN: (f64, f64, f64) = (0.90, 0.30, 0.25);
const PROBE: (f64, f64, f64) = (0.95, 0.97, 1.0);
const PROBE_SECONDS: f64 = 1.4;

fn tint(health: Health) -> Option<(f64, f64, f64)> {
    match health {
        Health::Good => Some(GOOD),
        Health::Slow => Some(SLOW),
        Health::Down => Some(DOWN),
        Health::Off => None,
    }
}

fn fill(health: Health, alpha: f64) -> Retained<NSColor> {
    match tint(health) {
        Some(rgb) => color(rgb, alpha),
        None => NSColor::systemGrayColor().colorWithAlphaComponent(alpha * 0.6),
    }
}

fn text(string: &str, center: NSPoint, size: f64, bold: bool, secondary: bool) {
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    let foreground = if secondary { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
    // SAFETY: the attribute statics are valid, and this only runs while the view is drawing.
    unsafe {
        let attributes = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, foreground.as_ref() as &AnyObject],
        );
        let string = NSString::from_str(string);
        let measured = string.sizeWithAttributes(Some(&attributes));
        string.drawAtPoint_withAttributes(
            NSPoint::new(center.x - measured.width / 2.0, center.y - measured.height / 2.0),
            Some(&attributes),
        );
    }
}

pub struct PathIvars {
    path: RefCell<Option<PathView>>,
    /// Boxes with values for the window, or a strip of dots for the panel.
    full: bool,
    probes: RefCell<Option<Retained<CALayer>>>,
    last_checks: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugePathView"]
    #[ivars = PathIvars]
    pub struct PathStrip;

    impl PathStrip {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);


impl PathStrip {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64, full: bool) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(PathIvars {
            path: RefCell::new(None),
            full,
            probes: RefCell::new(None),
            last_checks: Cell::new(0),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view.setWantsLayer(true);
        view
    }

    pub fn set_path(&self, path: PathView) {
        let checks = path.checks;
        if self.ivars().path.borrow().as_ref() != Some(&path) {
            *self.ivars().path.borrow_mut() = Some(path);
            self.setNeedsDisplay(true);
        }
        if checks > self.ivars().last_checks.get() {
            let first = self.ivars().last_checks.get() == 0;
            self.ivars().last_checks.set(checks);
            if !first {
                self.probe();
            }
        }
    }

    /// Where each step's center sits.
    fn centers(&self, count: usize) -> Vec<NSPoint> {
        let size = self.bounds().size;
        let y = if self.ivars().full { size.height - 22.0 } else { size.height - 9.0 };
        let (box_width, gap) = self.box_size(count);
        let start = if self.ivars().full { box_width / 2.0 } else { 14.0 };
        let step = if self.ivars().full { box_width + gap } else { (size.width - 28.0) / (count.max(2) - 1) as f64 };
        (0..count).map(|i| NSPoint::new(start + step * i as f64, y)).collect()
    }

    fn box_size(&self, count: usize) -> (f64, f64) {
        let width = self.bounds().size.width;
        let gap = 14.0;
        ((width - gap * (count.max(1) - 1) as f64) / count.max(1) as f64, gap)
    }

    fn probe(&self) {
        if motion::reduce_motion() {
            return;
        }
        let path = self.ivars().path.borrow();
        let Some(path) = path.as_ref() else { return };
        let reach = path.steps.iter().rposition(|s| s.health != Health::Off).unwrap_or(0);
        if reach == 0 {
            return;
        }
        let centers = self.centers(path.steps.len());
        let Some(layer) = self.layer() else { return };
        let container = self.ivars().probes.borrow_mut().get_or_insert_with(|| {
            let container = CALayer::new();
            container.setFrame(self.bounds());
            layer.addSublayer(&container);
            container
        }).clone();
        motion::clear(&container);
        let seconds = PROBE_SECONDS * reach as f64 / (path.steps.len() - 1) as f64;
        motion::pulse(&container, &motion::line(centers[0], centers[reach]), seconds, PROBE, 2.6);
    }

    fn draw(&self) {
        let path = self.ivars().path.borrow();
        let Some(path) = path.as_ref() else { return };
        let centers = self.centers(path.steps.len());
        // In the window, lines join the boxes' edges rather than crossing their labels.
        let inset = if self.ivars().full { self.box_size(centers.len()).0 / 2.0 } else { 0.0 };
        for i in 1..centers.len() {
            let line = NSBezierPath::bezierPath();
            line.moveToPoint(NSPoint::new(centers[i - 1].x + inset, centers[i - 1].y));
            line.lineToPoint(NSPoint::new(centers[i].x - inset, centers[i].y));
            line.setLineWidth(if self.ivars().full { 3.0 } else { 2.0 });
            fill(path.steps[i].health, 0.65).setStroke();
            if path.steps[i].health == Health::Off {
                let dash = [3.0, 3.0];
                // SAFETY: the pattern has the two entries the count says, and outlives the call.
                unsafe { line.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            }
            line.stroke();
        }
        if self.ivars().full {
            self.draw_boxes(&path.steps, &centers);
        } else {
            for (step, center) in path.steps.iter().zip(&centers) {
                let rect = NSRect::new(NSPoint::new(center.x - 5.0, center.y - 5.0), NSSize::new(10.0, 10.0));
                fill(step.health, 1.0).setFill();
                NSBezierPath::bezierPathWithOvalInRect(rect).fill();
                text(&step.label, NSPoint::new(center.x, center.y - 17.0), 10.5, false, true);
                if !step.value.is_empty() {
                    text(&step.value, NSPoint::new(center.x, center.y - 31.0), 10.5, true, step.health == Health::Off);
                }
            }
        }
    }

    fn draw_boxes(&self, steps: &[PathStepView], centers: &[NSPoint]) {
        let (width, _) = self.box_size(steps.len());
        for (step, center) in steps.iter().zip(centers) {
            let rect = NSRect::new(NSPoint::new(center.x - width / 2.0, center.y - 17.0), NSSize::new(width, 34.0));
            let alpha = if step.health == Health::Good { 0.16 } else { 0.3 };
            fill(step.health, alpha).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 6.0, 6.0).fill();
            text(&step.label, *center, 11.5, false, false);
            if !step.value.is_empty() {
                text(&step.value, NSPoint::new(center.x, center.y - 31.0), 11.5, true, step.health == Health::Off);
            }
        }
    }
}
