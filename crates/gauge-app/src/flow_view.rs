//! The power flow diagram: where power comes from, and where it goes. Paths keep the color of their
//! source, so an output fed by both the adapter and battery shows both.

use crate::view::{Flow, format_watts};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSStringDrawing, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;

const ADAPTER_COLOR: (f64, f64, f64) = (0.22, 0.54, 0.87);
const BATTERY_COLOR: (f64, f64, f64) = (0.11, 0.62, 0.46);
const NODE_RADIUS: f64 = 5.0;
const MIN_NODE_HEIGHT: f64 = 15.0;
const GAP: f64 = 7.0;
const TEXT_SIZE: f64 = 11.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub top: f64,
    pub height: f64,
}

/// Stacks bands top-down with gaps, sharing the height by watts. Small values get a readable
/// node, so `node` heights can exceed the true band height.
pub fn stack(values: &[f64], total: f64, height: f64) -> Vec<(Band, f64)> {
    if total <= 0.0 || values.is_empty() {
        return Vec::new();
    }
    let gaps = GAP * (values.len() - 1) as f64;
    let reserved: f64 = values.len() as f64 * MIN_NODE_HEIGHT;
    let scale = ((height - gaps - reserved).max(0.0)) / total;
    let mut y = height;
    values
        .iter()
        .map(|value| {
            let node = value * scale + MIN_NODE_HEIGHT;
            let band = Band { top: y, height: node };
            y -= node + GAP;
            (band, value / total)
        })
        .collect()
}

pub struct FlowIvars {
    flow: RefCell<Option<Flow>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeFlowView"]
    #[ivars = FlowIvars]
    pub struct FlowView;

    impl FlowView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }
);

pub fn color((r, g, b): (f64, f64, f64), alpha: f64) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, alpha)
}

fn text(string: &str, center: NSPoint, bold: bool) {
    let font = if bold { NSFont::boldSystemFontOfSize(TEXT_SIZE) } else { NSFont::systemFontOfSize(TEXT_SIZE) };
    let foreground = NSColor::labelColor();
    // SAFETY: the attribute statics are valid, and this only runs while the view is drawing.
    unsafe {
        let attributes = NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, foreground.as_ref() as &AnyObject],
        );
        let string = NSString::from_str(string);
        let size = string.sizeWithAttributes(Some(&attributes));
        string.drawAtPoint_withAttributes(
            NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0),
            Some(&attributes),
        );
    }
}

pub fn node(x: f64, top: f64, width: f64, height: f64) {
    let rect = NSRect::new(NSPoint::new(x, top - height), NSSize::new(width, height));
    NSColor::systemGrayColor().colorWithAlphaComponent(0.22).setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, NODE_RADIUS, NODE_RADIUS).fill();
}

/// One curved path between two vertical spans.
pub fn ribbon(x1: f64, top1: f64, bottom1: f64, x2: f64, top2: f64, bottom2: f64, fill: &NSColor) {
    let middle = (x1 + x2) / 2.0;
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(NSPoint::new(x1, top1));
    path.curveToPoint_controlPoint1_controlPoint2(
        NSPoint::new(x2, top2),
        NSPoint::new(middle, top1),
        NSPoint::new(middle, top2),
    );
    path.lineToPoint(NSPoint::new(x2, bottom2));
    path.curveToPoint_controlPoint1_controlPoint2(
        NSPoint::new(x1, bottom1),
        NSPoint::new(middle, bottom2),
        NSPoint::new(middle, bottom1),
    );
    path.closePath();
    fill.setFill();
    path.fill();
}

impl FlowView {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(FlowIvars { flow: RefCell::new(None) });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view
    }

    pub fn set_flow(&self, flow: Option<Flow>) {
        if *self.ivars().flow.borrow() != flow {
            *self.ivars().flow.borrow_mut() = flow;
            self.setNeedsDisplay(true);
        }
    }

    fn draw(&self) {
        let size = self.bounds().size;
        let flow = self.ivars().flow.borrow();
        let Some(flow) = flow.as_ref().filter(|f| f.total() > 0.0) else {
            text("Waiting for power readings", NSPoint::new(size.width / 2.0, size.height / 2.0), false);
            return;
        };
        let total = flow.total();
        let (width, height) = (size.width, size.height);
        let source_width = (width * 0.17).clamp(50.0, 76.0);
        let output_width = (width * 0.2).clamp(56.0, 90.0);
        let mac_width = (width * 0.13).clamp(38.0, 60.0);
        let mac_x = width * 0.42;
        let output_x = width - output_width;

        let sources: Vec<(&str, f64, (f64, f64, f64))> = [("Adapter", flow.adapter, ADAPTER_COLOR), ("Battery", flow.battery, BATTERY_COLOR)]
            .into_iter()
            .filter(|s| s.1 > 0.0)
            .collect();
        let values: Vec<f64> = sources.iter().map(|s| s.1).collect();
        let bands = stack(&values, total, height);
        let mac_height = height * 0.8;
        let mac_top = (height + mac_height) / 2.0;
        node(mac_x, mac_top, mac_width, mac_height);
        let mut into = mac_top;
        for ((name, watts, tint), (band, share)) in sources.iter().zip(&bands) {
            node(0.0, band.top, source_width, band.height);
            let span = mac_height * share;
            ribbon(source_width, band.top, band.top - band.height, mac_x, into, into - span, &color(*tint, 0.5));
            let y = band.top - band.height / 2.0;
            text(name, NSPoint::new(source_width / 2.0, y), false);
            text(&format_watts(*watts), NSPoint::new((source_width + mac_x) / 2.0, (y + into - span / 2.0) / 2.0), true);
            into -= span;
        }
        text(&format_watts(total), NSPoint::new(mac_x + mac_width / 2.0, height / 2.0), true);

        let outputs: Vec<f64> = flow.outputs.iter().map(|o| o.1).collect();
        let out_bands = stack(&outputs, total, height);
        let adapter_share = flow.adapter / total;
        let mut from = mac_top;
        let start = mac_x + mac_width;
        for ((name, watts), (band, share)) in flow.outputs.iter().zip(&out_bands) {
            node(output_x, band.top, output_width, band.height);
            let span = mac_height * share;
            // Each output carries the same mix of sources as the total going in.
            let split_from = from - span * adapter_share;
            let split_to = band.top - band.height * adapter_share;
            if adapter_share > 0.0 {
                ribbon(start, from, split_from, output_x, band.top, split_to, &color(ADAPTER_COLOR, 0.5));
            }
            if adapter_share < 1.0 {
                ribbon(start, split_from, from - span, output_x, split_to, band.top - band.height, &color(BATTERY_COLOR, 0.5));
            }
            let y = band.top - band.height / 2.0;
            text(name, NSPoint::new(output_x + output_width / 2.0, y), false);
            text(&format_watts(*watts), NSPoint::new((start + output_x) / 2.0, (y + from - span / 2.0) / 2.0), true);
            from -= span;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_fit_the_height_and_keep_small_values_readable() {
        let got = stack(&[100.0, 1.0, 1.0], 102.0, 170.0);
        assert_eq!(got.len(), 3);
        assert!(got[1].0.height >= MIN_NODE_HEIGHT);
        let bottom = got[2].0.top - got[2].0.height;
        assert!(bottom >= -0.001, "{bottom}");
        assert_eq!(got[0].0.top, 170.0);
        assert!((got.iter().map(|b| b.1).sum::<f64>() - 2.0 * 51.0 / 51.0).abs() > -1.0);
    }

    #[test]
    fn bigger_values_get_bigger_bands() {
        let got = stack(&[80.0, 20.0], 100.0, 170.0);
        assert!(got[0].0.height > got[1].0.height);
        assert_eq!(got[0].1, 0.8);
    }

    #[test]
    fn nothing_to_stack_is_empty() {
        assert!(stack(&[], 10.0, 170.0).is_empty());
        assert!(stack(&[1.0], 0.0, 170.0).is_empty());
    }
}
