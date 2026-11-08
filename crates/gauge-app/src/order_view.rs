//! A live copy of the menu bar at the top of Settings: drag an item to reorder it, click it to
//! open its page. It uses the same order, text and spacing as the real bar.

use crate::ui::fix_width;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};

const PADDING: f64 = 4.0;
const EDGE: f64 = 8.0;
const TEXT_SIZE: f64 = 12.0;
const DRAG_THRESHOLD: f64 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub struct BarItem {
    pub name: String,
    pub title: String,
    pub shown: bool,
}

/// Each item's left edge and width. Spacing shrinks before anything overlaps, since the preview
/// is narrower than a real menu bar can be.
pub fn frames(widths: &[f64], spacing: f64, available: f64) -> Vec<(f64, f64)> {
    let total: f64 = widths.iter().sum();
    let gaps = widths.len().saturating_sub(1).max(1) as f64;
    let gap = spacing.min(((available - total) / gaps).max(0.0));
    let mut x = EDGE;
    widths
        .iter()
        .map(|width| {
            let frame = (x, *width);
            x += width + gap;
            frame
        })
        .collect()
}

pub fn index_at(frames: &[(f64, f64)], x: f64) -> Option<usize> {
    frames.iter().position(|(left, width)| x >= *left && x < left + width)
}

/// Moves one entry, shifting the others to make room.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from < items.len() && to < items.len() {
        let item = items.remove(from);
        items.insert(to, item);
    }
}

pub fn display_name(module: &str) -> String {
    match module {
        "cpu" => "CPU".to_string(),
        "gpu" => "GPU".to_string(),
        "battery" => "Power".to_string(),
        other => {
            let mut letters = other.chars();
            letters.next().map_or_else(String::new, |first| first.to_uppercase().chain(letters).collect())
        }
    }
}

pub struct PreviewCallbacks {
    pub on_order: Box<dyn Fn(Vec<String>)>,
    pub on_select: Box<dyn Fn(String)>,
}

pub struct PreviewIvars {
    items: RefCell<Vec<BarItem>>,
    spacing: Cell<f64>,
    dragging: Cell<Option<usize>>,
    press_x: Cell<f64>,
    moved: Cell<bool>,
    callbacks: PreviewCallbacks,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeMenuBarPreview"]
    #[ivars = PreviewIvars]
    pub struct MenuBarPreview;

    impl MenuBarPreview {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let x = self.convertPoint_fromView(event.locationInWindow(), None).x;
            self.ivars().dragging.set(index_at(&self.current_frames(), x));
            self.ivars().press_x.set(x);
            self.ivars().moved.set(false);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(from) = self.ivars().dragging.get() else { return };
            let x = self.convertPoint_fromView(event.locationInWindow(), None).x;
            if (x - self.ivars().press_x.get()).abs() < DRAG_THRESHOLD && !self.ivars().moved.get() {
                return;
            }
            self.ivars().moved.set(true);
            if let Some(to) = index_at(&self.current_frames(), x).filter(|to| *to != from) {
                move_item(&mut self.ivars().items.borrow_mut(), from, to);
                self.ivars().dragging.set(Some(to));
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let Some(index) = self.ivars().dragging.take() else { return };
            let items = self.ivars().items.borrow().clone();
            self.setNeedsDisplay(true);
            if self.ivars().moved.get() {
                (self.ivars().callbacks.on_order)(items.into_iter().map(|i| i.name).collect());
            } else if let Some(item) = items.into_iter().nth(index) {
                (self.ivars().callbacks.on_select)(item.name);
            }
        }
    }
);

fn attributes(color: &NSColor) -> Retained<NSDictionary<objc2_foundation::NSAttributedStringKey, AnyObject>> {
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(TEXT_SIZE, 0.0);
    // SAFETY: the attribute name statics are valid for the process's lifetime.
    unsafe {
        NSDictionary::from_slices(
            &[NSFontAttributeName, NSForegroundColorAttributeName],
            &[font.as_ref() as &AnyObject, color.as_ref() as &AnyObject],
        )
    }
}

fn shown_text(item: &BarItem) -> String {
    if item.title.is_empty() { display_name(&item.name) } else { item.title.clone() }
}

fn text_width(text: &str) -> f64 {
    let attributes = attributes(&NSColor::labelColor());
    // SAFETY: a valid attributes dictionary, and measuring has no other requirements.
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attributes)) }.width
}

impl MenuBarPreview {
    pub fn new(mtm: MainThreadMarker, width: f64, height: f64, callbacks: PreviewCallbacks) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(width, height));
        let this = Self::alloc(mtm).set_ivars(PreviewIvars {
            items: RefCell::new(Vec::new()),
            spacing: Cell::new(0.0),
            dragging: Cell::new(None),
            press_x: Cell::new(0.0),
            moved: Cell::new(false),
            callbacks,
        });
        // SAFETY: initWithFrame: is NSView's designated initializer and the ivars are set.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        fix_width(&view, width);
        view.heightAnchor().constraintEqualToConstant(height).setActive(true);
        view
    }

    /// Takes fresh titles, but leaves a drag in progress alone.
    pub fn set_items(&self, items: Vec<BarItem>, spacing: f64) {
        if self.ivars().dragging.get().is_some() {
            return;
        }
        if *self.ivars().items.borrow() != items || self.ivars().spacing.get() != spacing {
            *self.ivars().items.borrow_mut() = items;
            self.ivars().spacing.set(spacing);
            self.setNeedsDisplay(true);
        }
    }

    fn current_frames(&self) -> Vec<(f64, f64)> {
        let widths: Vec<f64> = self
            .ivars()
            .items
            .borrow()
            .iter()
            .map(|item| text_width(&shown_text(item)) + PADDING)
            .collect();
        frames(&widths, self.ivars().spacing.get(), self.bounds().size.width - EDGE * 2.0)
    }

    fn draw(&self) {
        let bounds = self.bounds();
        NSColor::systemGrayColor().colorWithAlphaComponent(0.18).setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, 7.0, 7.0).fill();
        let items = self.ivars().items.borrow();
        let inset = 3.0;
        for (index, ((x, width), item)) in self.current_frames().into_iter().zip(items.iter()).enumerate() {
            let held = self.ivars().dragging.get() == Some(index);
            let plate = NSRect::new(
                NSPoint::new(x, inset),
                NSSize::new(width, bounds.size.height - inset * 2.0),
            );
            if held {
                NSColor::controlAccentColor().setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(plate, 5.0, 5.0).fill();
            }
            let color = match (held, item.shown) {
                (true, _) => NSColor::whiteColor(),
                (false, true) => NSColor::labelColor(),
                (false, false) => NSColor::tertiaryLabelColor(),
            };
            let attributes = attributes(&color);
            let text = NSString::from_str(&shown_text(item));
            // SAFETY: valid attributes, and this only runs while the view is drawing.
            unsafe {
                let size = text.sizeWithAttributes(Some(&attributes));
                text.drawAtPoint_withAttributes(
                    NSPoint::new(
                        x + (width - size.width) / 2.0,
                        (bounds.size.height - size.height) / 2.0,
                    ),
                    Some(&attributes),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_lay_out_left_to_right_with_spacing() {
        let got = frames(&[50.0, 30.0], 10.0, 500.0);
        assert_eq!(got, [(EDGE, 50.0), (EDGE + 60.0, 30.0)]);
    }

    #[test]
    fn spacing_shrinks_rather_than_overflowing() {
        let got = frames(&[100.0, 100.0], 40.0, 210.0);
        assert_eq!(got[1].0, EDGE + 110.0);
        let tight = frames(&[100.0, 100.0], 40.0, 150.0);
        assert_eq!(tight[1].0, EDGE + 100.0);
    }

    #[test]
    fn positions_find_the_item_under_them() {
        let list = frames(&[50.0, 30.0], 10.0, 500.0);
        assert_eq!(index_at(&list, EDGE + 5.0), Some(0));
        assert_eq!(index_at(&list, EDGE + 55.0), None);
        assert_eq!(index_at(&list, EDGE + 65.0), Some(1));
        assert_eq!(index_at(&list, 400.0), None);
    }

    #[test]
    fn names_read_properly() {
        assert_eq!(display_name("cpu"), "CPU");
        assert_eq!(display_name("network"), "Network");
    }

    #[test]
    fn moving_shifts_the_others() {
        let mut items = vec!["a", "b", "c", "d"];
        move_item(&mut items, 0, 2);
        assert_eq!(items, ["b", "c", "a", "d"]);
        move_item(&mut items, 3, 0);
        assert_eq!(items, ["d", "b", "c", "a"]);
        move_item(&mut items, 9, 0);
        assert_eq!(items.len(), 4);
    }
}
