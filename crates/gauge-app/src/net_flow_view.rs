//! The network flow: your connection on the left, apps in the middle, destinations on the
//! right. Downloads drift right to left, toward your Mac, and uploads drift outward.

use crate::flow_view::{Band, color, node, ribbon, stack};
use crate::motion;
use crate::net_view::{FlowTarget, HoverStyle, NetFlow, OTHER, hover_lines};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{AllocAnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearanceCustomization, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStringDrawing, NSTextAlignment, NSTextField, NSTextTab,
    NSBackingStoreType, NSPanel, NSTitlePosition, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindowOrderingMode,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSDictionary, NSMutableAttributedString, NSPoint, NSRect, NSSize, NSString,
};
use objc2_quartz_core::CALayer;
use std::cell::{Cell, RefCell};

pub const DOWN_COLOR: (f64, f64, f64) = (0.22, 0.54, 0.87);
pub const OUT_COLOR: (f64, f64, f64) = (0.45, 0.42, 0.92);
const DOWN_DOT: (f64, f64, f64) = (0.62, 0.80, 1.0);
const UP_DOT: (f64, f64, f64) = (0.36, 0.84, 0.68);
const TEXT_SIZE: f64 = 11.0;
const TWO_LINE_HEIGHT: f64 = 28.0;
const SOURCE_SHARE: f64 = 0.8;
const ROUND_TO: f64 = 4.0;
const CARD_TEXT_WIDTH: f64 = 230.0;
const CARD_MARGIN: f64 = 8.0;
const CARD_GAP: f64 = 14.0;
/// How far outside a thin ribbon still counts as resting on it.
const RIBBON_SLACK: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Ribbon {
    pub left: (f64, f64, f64),
    pub right: (f64, f64, f64),
    pub outer: bool,
    pub down: f64,
    pub up: f64,
    pub app: usize,
    /// Which of the flow's links it draws, for the ribbons from apps to destinations.
    pub link: Option<usize>,
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
    for (index, (app, band)) in flow.apps.iter().zip(&apps).enumerate() {
        let span = source_height * app.total() / total;
        ribbons.push(Ribbon {
            left: (source_width, into, into - span),
            right: (app_x, band.top, band.top - band.height),
            outer: false,
            down: app.down,
            up: app.up,
            app: index,
            link: None,
        });
        into -= span;
    }
    let mut app_cursor: Vec<f64> = apps.iter().map(|b| b.top).collect();
    let mut destination_cursor: Vec<f64> = destinations.iter().map(|b| b.top).collect();
    for (index, link) in flow.links.iter().enumerate() {
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
            app: link.app,
            link: Some(index),
        });
        app_cursor[link.app] -= out_span;
        destination_cursor[link.destination] -= in_span;
    }
    Layout { source_width, source_top, source_height, app_x, app_width, destination_x, destination_width, apps, destinations, ribbons }
}

/// Where a ribbon's top and bottom edges are at `x`, following the same curve it's drawn with.
fn ribbon_edges(r: &Ribbon, x: f64) -> Option<(f64, f64)> {
    let ((x1, top1, bottom1), (x2, top2, bottom2)) = (r.left, r.right);
    if x2 <= x1 || x < x1 || x > x2 {
        return None;
    }
    let s = (x - x1) / (x2 - x1);
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..24 {
        let t = (low + high) / 2.0;
        if 1.5 * t * (1.0 - t) + t * t * t < s { low = t } else { high = t }
    }
    let t = (low + high) / 2.0;
    let k = t * t * (3.0 - 2.0 * t);
    Some((top1 + (top2 - top1) * k, bottom1 + (bottom2 - bottom1) * k))
}

/// The part of the flow under `point`: a node, or else the ribbon whose middle is closest.
pub fn target_at(flow: &NetFlow, geometry: &Layout, point: NSPoint) -> Option<FlowTarget> {
    let within = |x: f64, width: f64, band: &Band| {
        point.x >= x && point.x <= x + width && point.y <= band.top && point.y >= band.top - band.height
    };
    if within(0.0, geometry.source_width, &Band { top: geometry.source_top, height: geometry.source_height }) {
        return Some(FlowTarget::Source);
    }
    if let Some(i) = geometry.apps.iter().position(|b| within(geometry.app_x, geometry.app_width, b)) {
        return Some(FlowTarget::App(i));
    }
    if let Some(i) = geometry.destinations.iter().position(|b| within(geometry.destination_x, geometry.destination_width, b)) {
        return Some(FlowTarget::Destination(i));
    }
    geometry
        .ribbons
        .iter()
        .filter_map(|r| {
            let (top, bottom) = ribbon_edges(r, point.x)?;
            (point.y <= top + RIBBON_SLACK && point.y >= bottom - RIBBON_SLACK).then(|| (r, (point.y - (top + bottom) / 2.0).abs()))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(r, _)| r.link.map_or(FlowTarget::App(r.app), FlowTarget::Link))
        .filter(|_| !flow.apps.is_empty())
}

/// Whether a ribbon belongs to what the pointer rests on, so it can stand out.
fn related(target: FlowTarget, r: &Ribbon, flow: &NetFlow) -> bool {
    match target {
        FlowTarget::Source => r.link.is_none(),
        FlowTarget::App(a) => r.app == a,
        FlowTarget::Destination(d) => r.link.and_then(|l| flow.links.get(l)).is_some_and(|l| l.destination == d),
        FlowTarget::Link(l) => r.link == Some(l),
    }
}

/// The hover card's text, with the amounts lined up on the right.
fn card_text(lines: &[(HoverStyle, String)]) -> Retained<NSMutableAttributedString> {
    let text = NSMutableAttributedString::new();
    // SAFETY: plain Foundation and AppKit object construction on the main thread; every
    // attribute value has the type its key expects.
    unsafe {
        let tab = NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment::Right,
            CARD_TEXT_WIDTH,
            &NSDictionary::new(),
        );
        let paragraph = NSMutableParagraphStyle::new();
        paragraph.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));
        paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        for (i, (style, line)) in lines.iter().enumerate() {
            let font = match style {
                HoverStyle::Title => NSFont::boldSystemFontOfSize(12.0),
                HoverStyle::Heading => NSFont::boldSystemFontOfSize(10.0),
                HoverStyle::Row | HoverStyle::Faint => NSFont::monospacedDigitSystemFontOfSize_weight(TEXT_SIZE, 0.0),
            };
            let color = match style {
                HoverStyle::Title | HoverStyle::Row => NSColor::labelColor(),
                HoverStyle::Heading | HoverStyle::Faint => NSColor::secondaryLabelColor(),
            };
            let attributes: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName],
                &[font.as_ref() as &AnyObject, color.as_ref() as &AnyObject, paragraph.as_ref() as &AnyObject],
            );
            let gap = if i + 1 < lines.len() { "\n" } else { "" };
            let part = NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(&format!("{line}{gap}")),
                Some(&attributes),
            );
            text.appendAttributedString(&part);
        }
    }
    text
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

/// The hover card's window, its box and its text.
type Card = (Retained<NSPanel>, Retained<NSBox>, Retained<NSTextField>);

pub struct NetFlowIvars {
    flow: RefCell<Option<NetFlow>>,
    empty: RefCell<String>,
    dots: RefCell<Option<Retained<CALayer>>>,
    /// What the dots were last built from, so they're only rebuilt when it changes.
    built: RefCell<Vec<DotSignature>>,
    /// Sent when the app column's "Other" is clicked.
    other_action: RefCell<Option<(Retained<AnyObject>, Sel)>>,
    /// Where the pointer rests, in this view's coordinates.
    pointer: Cell<Option<NSPoint>>,
    hovered: Cell<Option<FlowTarget>>,
    /// A small window that follows the pointer, so the card never covers what it describes.
    card: RefCell<Option<Card>>,
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

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.hover(Some(self.convertPoint_fromView(event.locationInWindow(), None)));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.hover(None);
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
            pointer: Cell::new(None),
            hovered: Cell::new(None),
            card: RefCell::new(None),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view.setWantsLayer(true);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the tracking area's owner is this view, which outlives it as the area is
        // only ever held by the view itself.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(NSTrackingArea::alloc(), NSRect::ZERO, options, Some(&view), None)
        };
        view.addTrackingArea(&area);
        view
    }

    /// Shows the details for whatever is under `point`, or hides them when it's `None`.
    fn hover(&self, point: Option<NSPoint>) {
        self.ivars().pointer.set(point);
        let target = {
            let flow = self.ivars().flow.borrow();
            let size = self.bounds().size;
            match (point, flow.as_ref().filter(|f| f.total() > 0.0)) {
                (Some(p), Some(flow)) => target_at(flow, &layout(flow, size.width, size.height), p).map(|t| (t, hover_lines(flow, t))),
                _ => None,
            }
        };
        let hovered = target.as_ref().map(|(t, _)| *t);
        if self.ivars().hovered.get() != hovered {
            self.ivars().hovered.set(hovered);
            self.setNeedsDisplay(true);
        }
        match (target, point) {
            (Some((_, lines)), Some(p)) if !lines.is_empty() => self.show_card(&lines, p),
            _ => {
                if let Some((window, _, _)) = self.ivars().card.borrow().as_ref() {
                    window.orderOut(None);
                }
            }
        }
    }

    fn show_card(&self, lines: &[(HoverStyle, String)], point: NSPoint) {
        let Some(parent) = self.window() else { return };
        let mtm = self.mtm();
        let mut stored = self.ivars().card.borrow_mut();
        let (window, card, label) = stored.get_or_insert_with(|| {
            let card = NSBox::new(mtm);
            card.setBoxType(NSBoxType::Custom);
            card.setTitlePosition(NSTitlePosition::NoTitle);
            card.setFillColor(&NSColor::windowBackgroundColor());
            card.setBorderColor(&NSColor::separatorColor());
            card.setBorderWidth(1.0);
            card.setCornerRadius(8.0);
            card.setContentViewMargins(NSSize::new(CARD_MARGIN, CARD_MARGIN));
            let label = NSTextField::wrappingLabelWithString(&NSString::new(), mtm);
            label.setPreferredMaxLayoutWidth(CARD_TEXT_WIDTH);
            card.setContentView(Some(&label));
            // SAFETY: a borderless panel made and used on the main thread. It's never closed,
            // only ordered out, so not releasing it on close keeps the Retained valid.
            let window = unsafe {
                let window = NSPanel::initWithContentRect_styleMask_backing_defer(
                    NSPanel::alloc(mtm),
                    NSRect::ZERO,
                    NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                    NSBackingStoreType::Buffered,
                    false,
                );
                window.setReleasedWhenClosed(false);
                window
            };
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
            window.setHasShadow(true);
            window.setIgnoresMouseEvents(true);
            window.setContentView(Some(&card));
            (window, card, label)
        });
        label.setAttributedStringValue(&card_text(lines));
        let fitting = label.fittingSize();
        label.setFrameSize(NSSize::new(CARD_TEXT_WIDTH, fitting.height));
        card.sizeToFit();
        let size = card.frame().size;
        let at = parent.convertPointToScreen(self.convertPoint_toView(point, None));
        let mut origin = NSPoint::new(at.x + CARD_GAP, at.y - CARD_GAP - size.height);
        if let Some(screen) = parent.screen() {
            let visible = screen.visibleFrame();
            if origin.x + size.width > visible.origin.x + visible.size.width {
                origin.x = at.x - CARD_GAP - size.width;
            }
            if origin.y < visible.origin.y {
                origin.y = at.y + CARD_GAP;
            }
        }
        window.setFrame_display(NSRect::new(origin, size), true);
        window.setAppearance(Some(&self.effectiveAppearance()));
        if window.parentWindow().is_none() {
            window.setLevel(parent.level());
            // SAFETY: both windows are alive on the main thread, and this view keeps the card
            // window alive for as long as it can be attached.
            unsafe { parent.addChildWindow_ordered(window, NSWindowOrderingMode::Above) };
        }
        window.orderFront(None);
    }

    /// The card, for pictures.
    pub fn card_view(&self) -> Option<Retained<NSView>> {
        self.ivars().card.borrow().as_ref().map(|(_, card, _)| Retained::into_super(card.clone()))
    }

    /// Rests the pointer on the first app, as a person would, and returns the card's text.
    /// The card stays up, for pictures; `hover(None)` hides it.
    pub fn hover_first_app(&self) -> Option<String> {
        let point = {
            let flow = self.ivars().flow.borrow();
            let flow = flow.as_ref().filter(|f| f.total() > 0.0)?;
            let size = self.bounds().size;
            let geometry = layout(flow, size.width, size.height);
            let band = geometry.apps.first()?;
            NSPoint::new(geometry.app_x + geometry.app_width * 0.75, band.top - band.height / 2.0)
        };
        self.hover(Some(point));
        self.ivars().card.borrow().as_ref().filter(|(window, _, _)| window.isVisible()).map(|(_, _, label)| label.stringValue().to_string())
    }

    /// As `hover_first_app`, then moves the pointer away again.
    pub fn hover_first_app_for_test(&self) -> Option<String> {
        let text = self.hover_first_app();
        self.hover(None);
        text
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
            self.hover(self.ivars().pointer.get());
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
        let hovered = self.ivars().hovered.get();
        for r in &geometry.ribbons {
            let strength = match hovered {
                None => 1.0,
                Some(target) if related(target, r, flow) => 1.8,
                Some(_) => 0.45,
            };
            let tint = if r.outer { color(OUT_COLOR, 0.30 * strength) } else { color(DOWN_COLOR, 0.36 * strength) };
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
                FlowLink { app: 0, destination: 0, down: 700.0, up: 70.0, hosts: vec![] },
                FlowLink { app: 0, destination: 1, down: 200.0, up: 20.0, hosts: vec![] },
                FlowLink { app: 1, destination: 1, down: 100.0, up: 10.0, hosts: vec![] },
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
    fn the_pointer_finds_nodes_and_ribbons() {
        let flow = sample();
        let got = layout(&flow, 300.0, 170.0);
        let middle = |x: f64, width: f64, band: &Band| NSPoint::new(x + width / 2.0, band.top - band.height / 2.0);
        assert_eq!(target_at(&flow, &got, middle(got.app_x, got.app_width, &got.apps[1])), Some(FlowTarget::App(1)));
        assert_eq!(
            target_at(&flow, &got, middle(got.destination_x, got.destination_width, &got.destinations[0])),
            Some(FlowTarget::Destination(0))
        );
        assert_eq!(target_at(&flow, &got, NSPoint::new(2.0, 85.0)), Some(FlowTarget::Source));
        for (index, r) in got.ribbons.iter().enumerate() {
            let x = (r.left.0 + r.right.0) / 2.0;
            let (top, bottom) = ribbon_edges(r, x).unwrap();
            let expected = r.link.map_or(FlowTarget::App(r.app), FlowTarget::Link);
            assert_eq!(target_at(&flow, &got, NSPoint::new(x, (top + bottom) / 2.0)), Some(expected), "ribbon {index}");
        }
        let gap = NSPoint::new(got.source_width + 1.0, 1.0);
        assert_eq!(target_at(&flow, &got, gap), None);
    }

    #[test]
    fn ribbon_edges_meet_both_ends() {
        let r = Ribbon { left: (10.0, 100.0, 80.0), right: (110.0, 40.0, 30.0), outer: true, down: 1.0, up: 0.0, app: 0, link: Some(0) };
        let (top, bottom) = ribbon_edges(&r, 10.0).unwrap();
        assert!((top - 100.0).abs() < 0.01 && (bottom - 80.0).abs() < 0.01);
        let (top, bottom) = ribbon_edges(&r, 110.0).unwrap();
        assert!((top - 40.0).abs() < 0.01 && (bottom - 30.0).abs() < 0.01);
        let (top, _) = ribbon_edges(&r, 60.0).unwrap();
        assert!((top - 70.0).abs() < 0.01, "halfway is halfway: {top}");
        assert!(ribbon_edges(&r, 5.0).is_none());
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
