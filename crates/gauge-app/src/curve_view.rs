//! Draggable fan curve for the settings window.

use gauge_core::curve::CurvePoint;
use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBezierPath, NSColor, NSEvent, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use std::cell::{Cell, RefCell};

pub const MIN_C: f32 = 30.0;
pub const MAX_C: f32 = 105.0;
const GRAB_RADIUS: f64 = 12.0;
const MIN_GAP_C: f32 = 1.0;

pub fn to_view(point: CurvePoint, width: f64, height: f64) -> (f64, f64) {
    (
        f64::from((point.temp_c - MIN_C) / (MAX_C - MIN_C)) * width,
        f64::from(point.percent / 100.0) * height,
    )
}

pub fn from_view(x: f64, y: f64, width: f64, height: f64) -> CurvePoint {
    CurvePoint {
        temp_c: (x / width) as f32 * (MAX_C - MIN_C) + MIN_C,
        percent: (y / height * 100.0).clamp(0.0, 100.0) as f32,
    }
}

pub fn nearest(points: &[CurvePoint], x: f64, y: f64, width: f64, height: f64) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let (px, py) = to_view(*point, width, height);
            (index, ((px - x).powi(2) + (py - y).powi(2)).sqrt())
        })
        .filter(|(_, distance)| *distance <= GRAB_RADIUS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// Moves one point, keeping points in temperature order so the curve stays valid.
pub fn drag(points: &mut [CurvePoint], index: usize, to: CurvePoint) {
    let low = if index > 0 { points[index - 1].temp_c + MIN_GAP_C } else { MIN_C };
    let high = points.get(index + 1).map_or(MAX_C, |p| p.temp_c - MIN_GAP_C);
    if let Some(point) = points.get_mut(index) {
        point.temp_c = to.temp_c.clamp(low, high);
        point.percent = to.percent;
    }
}

type Callback = Box<dyn Fn(Vec<CurvePoint>)>;

pub struct CurveIvars {
    points: RefCell<Vec<CurvePoint>>,
    dragging: Cell<Option<usize>>,
    on_change: RefCell<Option<Callback>>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeCurveView"]
    #[ivars = CurveIvars]
    pub struct CurveView;

    impl CurveView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let size = self.bounds().size;
            let points = self.ivars().points.borrow();
            self.ivars().dragging.set(nearest(&points, point.x, point.y, size.width, size.height));
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(index) = self.ivars().dragging.get() else { return };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let size = self.bounds().size;
            drag(
                &mut self.ivars().points.borrow_mut(),
                index,
                from_view(point.x, point.y, size.width, size.height),
            );
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if self.ivars().dragging.take().is_some()
                && let Some(callback) = &*self.ivars().on_change.borrow()
            {
                callback(self.ivars().points.borrow().clone());
            }
        }
    }
);

impl CurveView {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64, points: Vec<CurvePoint>, on_change: Callback) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(CurveIvars {
            points: RefCell::new(points),
            dragging: Cell::new(None),
            on_change: RefCell::new(Some(on_change)),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        view.widthAnchor().constraintEqualToConstant(width).setActive(true);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view
    }

    fn draw(&self) {
        let size = self.bounds().size;
        NSColor::separatorColor().colorWithAlphaComponent(0.08).setFill();
        NSBezierPath::bezierPathWithRect(self.bounds()).fill();
        NSColor::separatorColor().setStroke();
        for share in [0.25, 0.5, 0.75] {
            let line = NSBezierPath::bezierPath();
            line.setLineWidth(0.5);
            line.moveToPoint(NSPoint::new(0.0, size.height * share));
            line.lineToPoint(NSPoint::new(size.width, size.height * share));
            line.stroke();
            let column = NSBezierPath::bezierPath();
            column.setLineWidth(0.5);
            column.moveToPoint(NSPoint::new(size.width * share, 0.0));
            column.lineToPoint(NSPoint::new(size.width * share, size.height));
            column.stroke();
        }
        let points = self.ivars().points.borrow();
        let path = NSBezierPath::bezierPath();
        path.setLineWidth(2.0);
        for (index, point) in points.iter().enumerate() {
            let (x, y) = to_view(*point, size.width, size.height);
            if index == 0 {
                path.moveToPoint(NSPoint::new(x, y));
            } else {
                path.lineToPoint(NSPoint::new(x, y));
            }
        }
        NSColor::controlAccentColor().setStroke();
        path.stroke();
        NSColor::controlAccentColor().setFill();
        for point in points.iter() {
            let (x, y) = to_view(*point, size.width, size.height);
            NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(x - 5.0, y - 5.0),
                NSSize::new(10.0, 10.0),
            ))
            .fill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points() -> Vec<CurvePoint> {
        vec![
            CurvePoint { temp_c: 45.0, percent: 10.0 },
            CurvePoint { temp_c: 65.0, percent: 45.0 },
            CurvePoint { temp_c: 80.0, percent: 80.0 },
        ]
    }

    #[test]
    fn view_coordinates_round_trip() {
        let point = CurvePoint { temp_c: 60.0, percent: 40.0 };
        let (x, y) = to_view(point, 300.0, 100.0);
        let back = from_view(x, y, 300.0, 100.0);
        assert!((back.temp_c - 60.0).abs() < 0.01 && (back.percent - 40.0).abs() < 0.01);
    }

    #[test]
    fn clicks_grab_only_nearby_points() {
        let (x, y) = to_view(points()[1], 300.0, 100.0);
        assert_eq!(nearest(&points(), x + 3.0, y, 300.0, 100.0), Some(1));
        assert_eq!(nearest(&points(), 0.0, 100.0, 300.0, 100.0), None);
    }

    #[test]
    fn dragging_cannot_cross_a_neighbor() {
        let mut list = points();
        drag(&mut list, 1, CurvePoint { temp_c: 95.0, percent: 200.0_f32.min(100.0) });
        assert_eq!(list[1].temp_c, 79.0);
        assert!(gauge_core::curve::percent_for(&list, 60.0) < 100.0);
        drag(&mut list, 1, CurvePoint { temp_c: 10.0, percent: 0.0 });
        assert_eq!(list[1].temp_c, 46.0);
    }
}
