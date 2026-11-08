//! A window listing every sensor, updated while it's open.

use crate::ui::rows_text;
use crate::view::Row;
use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2_app_kit::{NSBackingStoreType, NSScrollView, NSTextField, NSWindow, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSEdgeInsets, NSPoint, NSRect, NSSize, NSString};
use std::cell::Cell;

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 560.0;
const MARGIN: f64 = 16.0;
const TEXT_SIZE: f64 = 12.0;

pub struct SensorsWindow {
    pub window: Retained<NSWindow>,
    scroll: Retained<NSScrollView>,
    text: Retained<NSTextField>,
    scrolled_to_top: Cell<bool>,
    last_count: Cell<usize>,
}

impl SensorsWindow {
    pub fn new(mtm: MainThreadMarker) -> SensorsWindow {
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable;
        let frame = NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, HEIGHT));
        // SAFETY: the rect and style are valid. The window stays alive with this struct, and
        // isn't released on close so it can be shown again.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("All Sensors"));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setAutomaticallyAdjustsContentInsets(false);
        scroll.setContentInsets(NSEdgeInsets { top: MARGIN, left: MARGIN, bottom: MARGIN, right: MARGIN });
        scroll.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let text = NSTextField::wrappingLabelWithString(&NSString::from_str("Reading sensors"), mtm);
        text.setSelectable(false);
        text.setPreferredMaxLayoutWidth(WIDTH - MARGIN * 2.0);
        scroll.setDocumentView(Some(&text));
        window.setContentView(Some(&scroll));
        window.center();
        SensorsWindow { window, scroll, text, scrolled_to_top: Cell::new(false), last_count: Cell::new(0) }
    }

    /// Keeps the reader's scroll position as values change.
    pub fn set_rows(&self, rows: &[Row]) {
        if rows.is_empty() {
            return;
        }
        let width = WIDTH - MARGIN * 2.0;
        self.text.setAttributedStringValue(&rows_text(rows, width - 4.0, TEXT_SIZE));
        if self.last_count.replace(rows.len()) != rows.len() {
            let height = self.text.fittingSize().height;
            self.text.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(width, height)));
        }
        if !self.scrolled_to_top.replace(true) {
            let clip = self.scroll.contentView();
            let visible = clip.bounds().size.height;
            let height = self.text.frame().size.height;
            let top = if self.text.isFlipped() { -MARGIN } else { (height - visible + MARGIN).max(-MARGIN) };
            clip.scrollToPoint(NSPoint::new(-MARGIN, top));
            self.scroll.reflectScrolledClipView(&clip);
        }
    }
}
