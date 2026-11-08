//! A custom view that draws one history graph, with a time cursor on hover.

use crate::graph_math::{Unit, format_ago, fraction, log_floor, nice_max, runs, ticks};
use crate::view::format_value;
use gauge_core::history::Bucket;
use gauge_core::settings::{GraphScale, GraphStyle};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing, NSTrackingArea, NSTrackingAreaOptions, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};

const LABEL_SIZE: f64 = 10.0;
const INSET_TOP: f64 = 4.0;
/// Axis labels get their own column, so data never runs behind them.
const GUTTER: f64 = 52.0;

pub struct SeriesData {
    pub color: (f64, f64, f64),
    pub buckets: Vec<Option<Bucket>>,
}

pub struct GraphData {
    pub series: Vec<SeriesData>,
    pub style: GraphStyle,
    pub scale: GraphScale,
    pub show_peaks: bool,
    pub unit: Unit,
    pub range_seconds: i64,
}

impl Default for GraphData {
    fn default() -> Self {
        GraphData {
            series: Vec::new(),
            style: GraphStyle::Area,
            scale: GraphScale::Linear,
            show_peaks: false,
            unit: Unit::Percent,
            range_seconds: 3600,
        }
    }
}

pub struct GraphIvars {
    data: RefCell<GraphData>,
    hover_x: Cell<Option<f64>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeGraphView"]
    #[ivars = GraphIvars]
    pub struct GraphView;

    impl GraphView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.ivars().hover_x.set(Some(point.x));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.ivars().hover_x.set(None);
            self.setNeedsDisplay(true);
        }
    }
);

fn attributes(color: &NSColor) -> Retained<NSDictionary<objc2_foundation::NSAttributedStringKey, AnyObject>> {
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(LABEL_SIZE, 0.0);
    // SAFETY: the attribute name statics are valid for the process's lifetime.
    unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[&*font as &AnyObject, color as &AnyObject],
        )
    }
}

fn draw_text(text: &str, point: NSPoint, color: &NSColor) {
    let attributes = attributes(color);
    // SAFETY: called while drawing, with a valid attributes dictionary.
    unsafe { NSString::from_str(text).drawAtPoint_withAttributes(point, Some(&attributes)) };
}

fn text_width(text: &str) -> f64 {
    let attributes = attributes(&NSColor::labelColor());
    // SAFETY: a valid attributes dictionary, and measuring has no other requirements.
    let size: NSSize = unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attributes)) };
    size.width
}

impl GraphView {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(GraphIvars {
            data: RefCell::new(GraphData::default()),
            hover_x: Cell::new(None),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        let options = NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        // SAFETY: the view owns the tracking area and outlives it.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                mtm.alloc(),
                frame,
                options,
                Some(view.as_ref()),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    pub fn set_data(&self, data: GraphData) {
        *self.ivars().data.borrow_mut() = data;
        self.setNeedsDisplay(true);
    }

    fn draw(&self) {
        let data = self.ivars().data.borrow();
        let bounds = self.bounds();
        let (width, height) = (bounds.size.width - GUTTER, bounds.size.height);
        let plot_height = height - INSET_TOP;

        let values = || {
            data.series
                .iter()
                .flat_map(|s| s.buckets.iter().flatten())
                .map(|b| if data.show_peaks { b.max } else { b.avg })
        };
        let peak = values().fold(0.0, f32::max);
        let max = data.unit.fixed_max().unwrap_or_else(|| nice_max(peak));
        let quietest = values().filter(|v| *v > 0.0).min_by(f32::total_cmp);
        let floor = log_floor(quietest, max, data.unit);
        let y_of = |value: f32| f64::from(fraction(value, max, floor, data.scale)) * plot_height;

        let background = NSBezierPath::bezierPathWithRect(NSRect::new(NSPoint::ZERO, NSSize::new(width, height)));
        NSColor::systemGrayColor().colorWithAlphaComponent(0.08).setFill();
        background.fill();

        NSColor::separatorColor().setStroke();
        for tick in ticks(max, floor, data.scale) {
            let y = y_of(tick).min(plot_height);
            let line = NSBezierPath::bezierPath();
            line.setLineWidth(0.5);
            line.moveToPoint(NSPoint::new(0.0, y));
            line.lineToPoint(NSPoint::new(width, y));
            line.stroke();
            let label = format_value(tick, data.unit);
            let label_y = (y - LABEL_SIZE / 2.0 - 2.0).clamp(0.0, height - LABEL_SIZE - 3.0);
            draw_text(&label, NSPoint::new(width + 4.0, label_y), &NSColor::secondaryLabelColor());
        }

        for series in &data.series {
            self.draw_series(series, &data, width, plot_height, max, floor);
        }
        self.draw_hover(&data, width, plot_height, &y_of);
    }

    fn draw_series(&self, series: &SeriesData, data: &GraphData, width: f64, height: f64, max: f32, floor: f32) {
        let count = series.buckets.len();
        if count == 0 {
            return;
        }
        let (r, g, b) = series.color;
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0);
        let step = width / count.max(2) as f64;
        let x_of = |index: usize| (index as f64 + 0.5) * step;
        let y_of = |value: f32| f64::from(fraction(value, max, floor, data.scale)) * height;

        if data.style == GraphStyle::Bars {
            color.colorWithAlphaComponent(0.65).setFill();
            for (index, bucket) in series.buckets.iter().enumerate() {
                if let Some(bucket) = bucket {
                    let rect = NSRect::new(
                        NSPoint::new(index as f64 * step, 0.0),
                        NSSize::new((step - 0.5).max(0.5), y_of(bucket.avg)),
                    );
                    NSBezierPath::bezierPathWithRect(rect).fill();
                }
            }
            return;
        }

        for run in runs(&series.buckets) {
            let path = NSBezierPath::bezierPath();
            path.setLineWidth(1.2);
            let values: Vec<(f64, f32, f32)> = run
                .clone()
                .filter_map(|i| series.buckets[i].map(|b| (x_of(i), b.avg, b.max)))
                .collect();
            let Some(first) = values.first().copied() else { continue };
            let last = values[values.len() - 1];
            if values.len() == 1 {
                // A lone reading has no line to draw, so show it as a dot.
                color.setFill();
                NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                    NSPoint::new(first.0 - 1.5, y_of(first.1) - 1.5),
                    NSSize::new(3.0, 3.0),
                ))
                .fill();
                continue;
            }
            if data.show_peaks {
                let peaks = NSBezierPath::bezierPath();
                peaks.setLineWidth(0.5);
                peaks.moveToPoint(NSPoint::new(first.0, y_of(first.2)));
                for (x, _, peak) in &values {
                    peaks.lineToPoint(NSPoint::new(*x, y_of(*peak)));
                }
                color.colorWithAlphaComponent(0.45).setStroke();
                peaks.stroke();
            }
            path.moveToPoint(NSPoint::new(first.0, y_of(first.1)));
            for (x, avg, _) in &values {
                path.lineToPoint(NSPoint::new(*x, y_of(*avg)));
            }
            if data.style == GraphStyle::Area {
                let fill = NSBezierPath::bezierPath();
                fill.moveToPoint(NSPoint::new(first.0, 0.0));
                for (x, avg, _) in &values {
                    fill.lineToPoint(NSPoint::new(*x, y_of(*avg)));
                }
                fill.lineToPoint(NSPoint::new(last.0, 0.0));
                fill.closePath();
                color.colorWithAlphaComponent(0.18).setFill();
                fill.fill();
            }
            color.setStroke();
            path.stroke();
        }
    }

    fn draw_hover(&self, data: &GraphData, width: f64, height: f64, y_of: &dyn Fn(f32) -> f64) {
        let Some(hover) = self.ivars().hover_x.get() else { return };
        let Some(count) = data.series.first().map(|s| s.buckets.len()).filter(|c| *c > 0) else {
            return;
        };
        // Over the labels column, there's no time to point at.
        if hover > width {
            return;
        }
        let x = hover.clamp(0.0, width);
        let index = ((x / width * count as f64) as usize).min(count - 1);
        let line = NSBezierPath::bezierPath();
        line.setLineWidth(0.75);
        line.moveToPoint(NSPoint::new(x, 0.0));
        line.lineToPoint(NSPoint::new(x, height));
        NSColor::secondaryLabelColor().setStroke();
        line.stroke();

        let seconds_ago = ((count - 1 - index) as f64 / count as f64 * data.range_seconds as f64) as i64;
        let mut text = format_ago(seconds_ago);
        for series in &data.series {
            match series.buckets.get(index).copied().flatten() {
                Some(bucket) => {
                    text.push_str(&format!("  {}", format_value(bucket.avg, data.unit)));
                    let dot = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                        NSPoint::new(x - 2.5, y_of(bucket.avg) - 2.5),
                        NSSize::new(5.0, 5.0),
                    ));
                    let (r, g, b) = series.color;
                    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
                    dot.fill();
                }
                None => text.push_str("  no data"),
            }
        }
        let label_width = text_width(&text);
        let label_x = (x + 4.0).min(width - label_width - 2.0).max(2.0);
        let label_y = height - LABEL_SIZE - 3.0;
        let plate = NSBezierPath::bezierPathWithRect(NSRect::new(
            NSPoint::new(label_x - 2.0, label_y - 1.0),
            NSSize::new(label_width + 4.0, LABEL_SIZE + 4.0),
        ));
        objc2_app_kit::NSColor::windowBackgroundColor().colorWithAlphaComponent(0.85).setFill();
        plate.fill();
        draw_text(&text, NSPoint::new(label_x, label_y), &NSColor::labelColor());
    }
}
