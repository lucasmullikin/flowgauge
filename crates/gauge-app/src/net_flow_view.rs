//! The network flow: your connection on the left, apps in the middle, destinations on the
//! right. Downloads drift right to left, toward your Mac, and uploads drift outward.

use crate::flow_view::{Band, color, node, ribbon, stack};
use crate::motion;
use crate::net_view::{NetFlow, OTHER};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSStringDrawing, NSView};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::CALayer;
use std::cell::RefCell;

pub const DOWN_COLOR: (f64, f64, f64) = (0.22, 0.54, 0.87);
pub const OUT_COLOR: (f64, f64, f64) = (0.45, 0.42, 0.92);
const DOWN_DOT: (f64, f64, f64) = (0.62, 0.80, 1.0);
const UP_DOT: (f64, f64, f64) = (0.36, 0.84, 0.68);
const TEXT_SIZE: f64 = 11.0;
const TWO_LINE_HEIGHT: f64 = 28.0;
const SOURCE_SHARE: f64 = 0.8;
const ROUND_TO: f64 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Ribbon {
    pub left: (f64, f64, f64),
    pub right: (f64, f64, f64),
    pub outer: bool,
    pub down: f64,
    pub up: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub source_width: f64,
    pub source_top: f64,
    pub source_height: f64,
    pub app_x: f64,
    pub app_width: f64,
    pub destination_x: f64,
    pub destination_width: f64,
    pub apps: Vec<Band>,
    pub destinations: Vec<Band>,
    pub ribbons: Vec<Ribbon>,
}

/// Where every node and ribbon goes. Each ribbon is (x, top, bottom) at both ends.
pub fn layout(flow: &NetFlow, width: f64, height: f64) -> Layout {
    let total = flow.total();
    let source_width = (width * 0.14).clamp(40.0, 72.0);
    let app_width = (width * 0.22).clamp(60.0, 120.0);
    let destination_width = (width * 0.24).clamp(64.0, 150.0);
    let app_x = width * 0.36;
    let destination_x = width - destination_width;
    let source_height = height * SOURCE_SHARE;
    let source_top = (height + source_height) / 2.0;
    let apps: Vec<Band> = stack(&flow.apps.iter().map(|a| a.total()).collect::<Vec<_>>(), total, height)
        .into_iter()
        .map(|(band, _)| band)
        .collect();
    let destinations: Vec<Band> = stack(&flow.destinations.iter().map(|d| d.total()).collect::<Vec<_>>(), total, height)
        .into_iter()
        .map(|(band, _)| band)
        .collect();
    let mut ribbons = Vec::new();
    let mut into = source_top;
    for (app, band) in flow.apps.iter().zip(&apps) {
        let span = source_height * app.total() / total;
        ribbons.push(Ribbon {
            left: (source_width, into, into - span),
            right: (app_x, band.top, band.top - band.height),
            outer: false,
            down: app.down,
            up: app.up,
        });
        into -= span;
    }
    let mut app_cursor: Vec<f64> = apps.iter().map(|b| b.top).collect();
    let mut destination_cursor: Vec<f64> = destinations.iter().map(|b| b.top).collect();
    for link in &flow.links {
        let (Some(app_band), Some(destination_band)) = (apps.get(link.app), destinations.get(link.destination)) else {
            continue;
        };
        let amount = link.down + link.up;
        let out_span = app_band.height * amount / flow.apps[link.app].total().max(f64::MIN_POSITIVE);
        let in_span = destination_band.height * amount / flow.destinations[link.destination].total().max(f64::MIN_POSITIVE);
        let from = app_cursor[link.app];
        let to = destination_cursor[link.destination];
        ribbons.push(Ribbon {
            left: (app_x + app_width, from, from - out_span),
            right: (destination_x, to, to - in_span),
            outer: true,
            down: link.down,
            up: link.up,
        });
        app_cursor[link.app] -= out_span;
        destination_cursor[link.destination] -= in_span;
    }
    Layout { source_width, source_top, source_height, app_x, app_width, destination_x, destination_width, apps, destinations, ribbons }
}

fn attributes(bold: bool, secondary: bool) -> Retained<NSDictionary<NSString, AnyObject>> {
    let font = if bold { NSFont::boldSystemFontOfSize(TEXT_SIZE) } else { NSFont::systemFontOfSize(TEXT_SIZE) };
    let foreground = if secondary { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
    // SAFETY: the attribute statics are valid for the process's lifetime.
    unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, foreground.as_ref() as &AnyObject],
        )
    }
}

/// Draws centered text, shortened with an ellipsis to fit `max_width`.
fn text(string: &str, center: NSPoint, max_width: f64, bold: bool, secondary: bool) {
    let attributes = attributes(bold, secondary);
    let mut shown = string.to_string();
    let mut chars: Vec<char> = string.chars().collect();
    loop {
        let candidate = NSString::from_str(&shown);
        // SAFETY: the attributes hold a font and a color, as these calls expect, and this only
        // runs while the view is drawing.
        let size = unsafe { candidate.sizeWithAttributes(Some(&attributes)) };
        if size.width <= max_width || chars.len() <= 1 {
            // SAFETY: as above.
            unsafe {
                candidate.drawAtPoint_withAttributes(
                    NSPoint::new(center.x - size.width / 2.0, center.y - size.height / 2.0),
                    Some(&attributes),
                );
            }
            return;
        }
        chars.pop();
        shown = format!("{}…", chars.iter().collect::<String>().trim_end());
    }
}

fn label(name: &str, value: &str, x: f64, band: &Band, width: f64) {
    let center = x + width / 2.0;
    let middle = band.top - band.height / 2.0;
    if band.height >= TWO_LINE_HEIGHT {
        text(name, NSPoint::new(center, middle + 6.5), width - 6.0, false, false);
        text(value, NSPoint::new(center, middle - 6.5), width - 6.0, false, true);
    } else {
        text(name, NSPoint::new(center, middle), width - 6.0, false, false);
    }
}

/// A ribbon's two ends, rounded, and how many dots move each way.
type DotSignature = (i64, i64, i64, i64, usize, usize);

pub struct NetFlowIvars {
    flow: RefCell<Option<NetFlow>>,
    empty: RefCell<String>,
    dots: RefCell<Option<Retained<CALayer>>>,
    /// What the dots were last built from, so they're only rebuilt when it changes.
    built: RefCell<Vec<DotSignature>>,
    /// Sent when the app column's "Other" is clicked.
    other_action: RefCell<Option<(Retained<AnyObject>, Sel)>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeNetFlowView"]
    #[ivars = NetFlowIvars]
    pub struct NetFlowView;

    impl NetFlowView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if self.other_app_contains(point)
                && let Some((target, action)) = self.ivars().other_action.borrow().as_ref()
            {
                // SAFETY: the target was given with this action and implements it.
                let _: () = unsafe { objc2::msg_send![&**target, performSelector: *action, withObject: self] };
            }
        }
    }
);

impl NetFlowView {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(NetFlowIvars {
            flow: RefCell::new(None),
            empty: RefCell::new("Waiting for traffic".into()),
            dots: RefCell::new(None),
            built: RefCell::new(Vec::new()),
            other_action: RefCell::new(None),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view.setWantsLayer(true);
        view
    }

    /// `empty` is shown when there's no flow.
    pub fn set_flow(&self, flow: Option<NetFlow>, empty: &str) {
        if *self.ivars().empty.borrow() != empty {
            *self.ivars().empty.borrow_mut() = empty.to_string();
            self.setNeedsDisplay(true);
        }
        if *self.ivars().flow.borrow() != flow {
            *self.ivars().flow.borrow_mut() = flow;
            self.setNeedsDisplay(true);
            self.update_dots();
        }
    }

    /// Makes the app column's "Other" clickable. The target must outlive this view.
    pub fn set_other_action(&self, target: &AnyObject, action: Sel) {
        // SAFETY: the target is a live object; retaining it keeps it alive while it's stored.
        let target = unsafe { Retained::retain(target as *const AnyObject as *mut AnyObject) };
        *self.ivars().other_action.borrow_mut() = target.map(|t| (t, action));
    }

    /// The middle of the app column's "Other", if the flow has one, for the self-test.
    pub fn other_app_center(&self) -> Option<NSPoint> {
        let flow = self.ivars().flow.borrow();
        let flow = flow.as_ref().filter(|f| f.total() > 0.0)?;
        let size = self.bounds().size;
        let geometry = layout(flow, size.width, size.height);
        flow.apps.iter().zip(&geometry.apps).find(|(app, _)| app.label == OTHER).map(|(_, band)| {
            NSPoint::new(geometry.app_x + geometry.app_width / 2.0, band.top - band.height / 2.0)
        })
    }

    fn other_app_contains(&self, point: NSPoint) -> bool {
        let flow = self.ivars().flow.borrow();
        let Some(flow) = flow.as_ref().filter(|f| f.total() > 0.0) else { return false };
        let size = self.bounds().size;
        let geometry = layout(flow, size.width, size.height);
        flow.apps.iter().zip(&geometry.apps).any(|(app, band)| {
            app.label == OTHER
                && point.x >= geometry.app_x
                && point.x <= geometry.app_x + geometry.app_width
                && point.y <= band.top
                && point.y >= band.top - band.height
        })
    }

    /// How many dots are moving, for the self-test.
    pub fn moving_dots(&self) -> usize {
        // SAFETY: a plain read of the layer tree on the main thread.
        self.ivars().dots.borrow().as_ref().and_then(|c| unsafe { c.sublayers() }).map_or(0, |s| s.count())
    }

    fn container(&self) -> Option<Retained<CALayer>> {
        if let Some(existing) = self.ivars().dots.borrow().as_ref() {
            return Some(existing.clone());
        }
        let layer = self.layer()?;
        let container = CALayer::new();
        container.setFrame(self.bounds());
        layer.addSublayer(&container);
        *self.ivars().dots.borrow_mut() = Some(container.clone());
        Some(container)
    }

    fn update_dots(&self) {
        let Some(container) = self.container() else { return };
        let flow = self.ivars().flow.borrow();
        let size = self.bounds().size;
        let moving = flow.as_ref().filter(|f| f.live && f.total() > 0.0 && !motion::reduce_motion());
        let Some(flow) = moving else {
            motion::clear(&container);
            self.ivars().built.borrow_mut().clear();
            return;
        };
        let geometry = layout(flow, size.width, size.height);
        let round = |v: f64| (v / ROUND_TO).round() as i64;
        let signature: Vec<_> = geometry
            .ribbons
            .iter()
            .map(|r| {
                let middle = |(x, top, bottom): (f64, f64, f64)| (round(x), round((top + bottom) / 2.0));
                let (a, b) = middle(r.left);
                let (c, d) = middle(r.right);
                (a, b, c, d, motion::pace(r.down).0, motion::pace(r.up).0)
            })
            .collect();
        if *self.ivars().built.borrow() == signature {
            return;
        }
        motion::clear(&container);
        container.setFrame(self.bounds());
        for ribbon in &geometry.ribbons {
            let left = NSPoint::new(ribbon.left.0, (ribbon.left.1 + ribbon.left.2) / 2.0);
            let right = NSPoint::new(ribbon.right.0, (ribbon.right.1 + ribbon.right.2) / 2.0);
            let thickness = (ribbon.left.1 - ribbon.left.2).min(ribbon.right.1 - ribbon.right.2);
            let dot = (1.6 + thickness * 0.08).clamp(1.8, 3.2);
            let (count, duration) = motion::pace(ribbon.down);
            if count > 0 {
                motion::drift(&container, &motion::curve(right, left), count, duration, DOWN_DOT, dot);
            }
            let (count, duration) = motion::pace(ribbon.up);
            if count > 0 {
                motion::drift(&container, &motion::curve(left, right), count, duration, UP_DOT, dot);
            }
        }
        *self.ivars().built.borrow_mut() = signature;
    }

    fn draw(&self) {
        let size = self.bounds().size;
        let flow = self.ivars().flow.borrow();
        let Some(flow) = flow.as_ref().filter(|f| f.total() > 0.0) else {
            text(&self.ivars().empty.borrow(), NSPoint::new(size.width / 2.0, size.height / 2.0), size.width, false, true);
            return;
        };
        let geometry = layout(flow, size.width, size.height);
        node(0.0, geometry.source_top, geometry.source_width, geometry.source_height);
        for r in &geometry.ribbons {
            let tint = if r.outer { color(OUT_COLOR, 0.30) } else { color(DOWN_COLOR, 0.36) };
            ribbon(r.left.0, r.left.1, r.left.2, r.right.0, r.right.1, r.right.2, &tint);
        }
        let source_band = Band { top: geometry.source_top, height: geometry.source_height };
        label(&flow.source, &flow.format(flow.total()), 0.0, &source_band, geometry.source_width + 6.0);
        for (app, band) in flow.apps.iter().zip(&geometry.apps) {
            node(geometry.app_x, band.top, geometry.app_width, band.height);
            label(&app.label, &flow.format(app.total()), geometry.app_x, band, geometry.app_width);
        }
        for (destination, band) in flow.destinations.iter().zip(&geometry.destinations) {
            node(geometry.destination_x, band.top, geometry.destination_width, band.height);
            label(&destination.label, &flow.format(destination.total()), geometry.destination_x, band, geometry.destination_width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net_view::{FlowLink, FlowNode};

    fn sample() -> NetFlow {
        let node = |label: &str, down: f64| FlowNode { label: label.into(), down, up: down / 10.0 };
        NetFlow {
            source: "Wi-Fi".into(),
            apps: vec![node("Safari", 900.0), node("Mail", 100.0)],
            destinations: vec![node("example.net", 700.0), node("Other", 300.0)],
            links: vec![
                FlowLink { app: 0, destination: 0, down: 700.0, up: 70.0 },
                FlowLink { app: 0, destination: 1, down: 200.0, up: 20.0 },
                FlowLink { app: 1, destination: 1, down: 100.0, up: 10.0 },
            ],
            live: true,
        }
    }

    #[test]
    fn ribbons_fill_each_app_node_exactly() {
        let got = layout(&sample(), 300.0, 170.0);
        assert_eq!(got.ribbons.len(), 5);
        let safari = &got.apps[0];
        let out: f64 = got.ribbons.iter().filter(|r| r.outer && (r.left.1 <= safari.top + 0.001) && r.left.2 >= safari.top - safari.height - 0.001).map(|r| r.left.1 - r.left.2).sum();
        assert!((out - safari.height).abs() < 0.01, "{out} vs {}", safari.height);
        let into_source: f64 = got.ribbons.iter().filter(|r| !r.outer).map(|r| r.left.1 - r.left.2).sum();
        assert!((into_source - got.source_height).abs() < 0.01);
    }

    #[test]
    fn columns_stay_inside_the_view_and_in_order() {
        for width in [300.0, 668.0] {
            let got = layout(&sample(), width, 170.0);
            assert!(got.source_width < got.app_x);
            assert!(got.app_x + got.app_width < got.destination_x);
            assert!((got.destination_x + got.destination_width - width).abs() < 0.001);
        }
    }
}
