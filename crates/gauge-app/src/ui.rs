//! Small helpers for building AppKit content, shared by panels and the settings window.

use crate::view::Row;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSLayoutAttribute,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStackView, NSTextAlignment, NSTextField,
    NSTextTab, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary,
    NSEdgeInsets, NSMutableAttributedString, NSString,
};

pub fn vertical_stack(mtm: MainThreadMarker, spacing: f64, inset: f64) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(spacing);
    stack.setEdgeInsets(NSEdgeInsets { top: inset, left: inset, bottom: inset, right: inset });
    stack
}

pub fn horizontal_stack(mtm: MainThreadMarker, spacing: f64) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setSpacing(spacing);
    stack
}

pub fn fix_width(view: &NSView, width: f64) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.widthAnchor().constraintEqualToConstant(width).setActive(true);
}

pub fn label(mtm: MainThreadMarker, text: &str, size: f64, bold: bool) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    field.setFont(Some(&font));
    field
}

fn attributes(
    font: &NSFont,
    color: &NSColor,
    paragraph: Option<&NSMutableParagraphStyle>,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    // SAFETY: the attribute name statics are valid for the process's lifetime.
    unsafe {
        match paragraph {
            Some(paragraph) => NSDictionary::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName],
                &[font as &AnyObject, color as &AnyObject, paragraph as &AnyObject],
            ),
            None => NSDictionary::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[font as &AnyObject, color as &AnyObject],
            ),
        }
    }
}

fn append(target: &NSMutableAttributedString, text: &str, attributes: &NSDictionary<NSAttributedStringKey, AnyObject>) {
    // SAFETY: both strings and the dictionary are valid objects.
    let part = unsafe {
        NSAttributedString::new_with_attributes(&NSString::from_str(text), attributes)
    };
    target.appendAttributedString(&part);
}

/// Keys in muted color on the left, values right-aligned at `width`.
pub fn rows_text(rows: &[Row], width: f64, size: f64) -> Retained<NSAttributedString> {
    let paragraph = NSMutableParagraphStyle::new();
    // SAFETY: an empty options dictionary is valid for a right-aligned tab stop.
    let tab = unsafe {
        NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment::Right,
            width,
            &NSDictionary::new(),
        )
    };
    paragraph.setTabStops(Some(&NSArray::from_retained_slice(&[tab])));
    paragraph.setLineSpacing(2.0);
    let regular = NSFont::monospacedDigitSystemFontOfSize_weight(size, 0.0);
    let bold = NSFont::boldSystemFontOfSize(size);
    let key_attributes = attributes(&regular, &NSColor::secondaryLabelColor(), Some(&paragraph));
    let value_attributes = attributes(&regular, &NSColor::labelColor(), Some(&paragraph));
    let header_attributes = attributes(&bold, &NSColor::labelColor(), Some(&paragraph));
    let text = NSMutableAttributedString::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            append(&text, "\n", &value_attributes);
        }
        if row.header {
            append(&text, &row.key, &header_attributes);
        } else {
            append(&text, &row.key, &key_attributes);
            append(&text, "\t", &value_attributes);
            append(&text, &row.value, &value_attributes);
        }
    }
    Retained::into_super(text)
}

/// Colored dot and text for each entry, for graph legends.
pub fn legend_text(entries: &[(String, (f64, f64, f64))], size: f64) -> Retained<NSAttributedString> {
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, 0.0);
    let plain = attributes(&font, &NSColor::labelColor(), None);
    let text = NSMutableAttributedString::new();
    for (index, (entry, (r, g, b))) in entries.iter().enumerate() {
        if index > 0 {
            append(&text, "     ", &plain);
        }
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(*r, *g, *b, 1.0);
        append(&text, "● ", &attributes(&font, &color, None));
        append(&text, entry, &plain);
    }
    Retained::into_super(text)
}
