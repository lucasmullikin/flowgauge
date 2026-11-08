//! The settings window: a live menu bar across the top, a sidebar of monitors, and one aligned
//! form per page. Pages come from `settings_form`, so this only builds controls.

use crate::curve_view::CurveView;
use crate::order_view::{BarItem, MenuBarPreview, PreviewCallbacks};
use crate::settings_form::{self, Control, FormRow, PAGES, icon_tag};
use crate::ui::{fix_width, horizontal_stack, label, vertical_stack};
use crate::view::ICONS;
use gauge_core::curve::CurvePoint;
use gauge_core::settings::{MODULES, Settings, parse_color};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBezelStyle, NSBox, NSBoxType, NSButton,
    NSButtonType, NSColor, NSColorWell, NSColorWellStyle, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSLayoutAttribute, NSPopUpButton,
    NSLayoutConstraint, NSScreen, NSScrollView, NSSegmentSwitchTracking, NSSegmentedControl, NSSlider, NSStackView,
    NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSAttributedString, NSDictionary, NSMutableAttributedString, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const LABEL_WIDTH: f64 = 120.0;
const CONTROL_WIDTH: f64 = 360.0;
const SIDEBAR_WIDTH: f64 = 140.0;
const FORM_WIDTH: f64 = LABEL_WIDTH + CONTROL_WIDTH + 12.0;
const MARGIN: f64 = 16.0;
const PREVIEW_HEIGHT: f64 = 30.0;
const ICON_COLUMNS: usize = 10;
const READOUT_WIDTH: f64 = 70.0;
const SCROLLER_ROOM: f64 = 16.0;
const WINDOW_CHROME_HEIGHT: f64 = 120.0;
const FALLBACK_SCREEN_HEIGHT: f64 = 800.0;
/// Tall enough to show the whole sidebar.
const MIN_PAGE_HEIGHT: f64 = 240.0;

pub struct SettingsWindow {
    pub window: Retained<NSWindow>,
    scroll: Retained<NSScrollView>,
    sidebar: Vec<Retained<NSButton>>,
    preview: Retained<MenuBarPreview>,
    scroll_height: Retained<NSLayoutConstraint>,
    current: RefCell<Option<Retained<NSStackView>>>,
    readouts: RefCell<Vec<(i64, Retained<NSTextField>, &'static str)>>,
    page: Cell<i64>,
    has_items: Cell<bool>,
}

/// Slider readouts drop decimals, since the sliders aren't that precise.
pub fn readout_text(value: f64, unit: &str) -> String {
    format!("{value:.0}{unit}")
}

fn sidebar_title(page: usize, selected: bool, settings: &Settings) -> Retained<NSAttributedString> {
    let font = NSFont::systemFontOfSize(13.0);
    let module = page.checked_sub(1).and_then(|i| MODULES.get(i)).and_then(|m| settings.module(m));
    let text_color = if module.is_some_and(|m| !m.shown) { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
    // A monitor's own color can match the selection highlight, so the selected dot turns white.
    let dot_color = match (module.and_then(|m| parse_color(&m.color)), selected) {
        (None, _) => NSColor::clearColor(),
        (Some(_), true) => NSColor::whiteColor(),
        (Some((r, g, b)), false) => NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0),
    };
    let build = |text: &str, color: &NSColor| {
        // SAFETY: the attribute statics are valid, and every object is alive for the call.
        unsafe {
            let attributes = NSDictionary::from_slices(
                &[NSFontAttributeName, NSForegroundColorAttributeName],
                &[font.as_ref() as &AnyObject, color.as_ref() as &AnyObject],
            );
            NSAttributedString::new_with_attributes(&NSString::from_str(text), &attributes)
        }
    };
    let title = NSMutableAttributedString::new();
    title.appendAttributedString(&build("●  ", &dot_color));
    title.appendAttributedString(&build(PAGES[page], &text_color));
    Retained::into_super(title)
}

fn segmented(mtm: MainThreadMarker, options: &[&str], selected: usize, tag: i64, target: &AnyObject) -> Retained<NSView> {
    let titles: Vec<Retained<NSString>> = options.iter().map(|o| NSString::from_str(o)).collect();
    // SAFETY: the target outlives the window and implements the action.
    let control = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &NSArray::from_retained_slice(&titles),
            NSSegmentSwitchTracking::SelectOne,
            Some(target),
            Some(sel!(settingChanged:)),
            mtm,
        )
    };
    control.setSelectedSegment(selected as isize);
    control.setTag(tag as isize);
    Retained::into_super(Retained::into_super(control))
}

fn color_well(mtm: MainThreadMarker, hex: &str, tag: i64, target: &AnyObject) -> Retained<NSColorWell> {
    let well = NSColorWell::colorWellWithStyle(NSColorWellStyle::Minimal, mtm);
    let (r, g, b) = parse_color(hex).unwrap_or((0.0, 0.5, 1.0));
    well.setColor(&NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0));
    // SAFETY: the target outlives the window and implements the action.
    unsafe {
        well.setTarget(Some(target));
        well.setAction(Some(sel!(settingChanged:)));
    }
    well.setTag(tag as isize);
    fix_width(&well, 44.0);
    well
}

fn icon_palette(mtm: MainThreadMarker, page: i64, target: &AnyObject) -> Retained<NSView> {
    let rows = vertical_stack(mtm, 4.0, 0.0);
    for (row, icons) in ICONS.chunks(ICON_COLUMNS).enumerate() {
        let line = horizontal_stack(mtm, 4.0);
        for (column, icon) in icons.iter().enumerate() {
            // SAFETY: the target outlives the window and implements the action.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(icon),
                    Some(target),
                    Some(sel!(settingChanged:)),
                    mtm,
                )
            };
            button.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            button.setTag(icon_tag(page, row * ICON_COLUMNS + column) as isize);
            fix_width(&button, 30.0);
            line.addArrangedSubview(&button);
        }
        rows.addArrangedSubview(&line);
    }
    Retained::into_super(rows)
}

impl SettingsWindow {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject, callbacks: PreviewCallbacks) -> SettingsWindow {
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        let width = SIDEBAR_WIDTH + FORM_WIDTH + SCROLLER_ROOM + MARGIN * 3.0;
        // SAFETY: the rect and style are valid. The window stays alive with this struct, and
        // isn't released on close so it can be shown again.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(width, 480.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("Gauge Settings"));

        let root = vertical_stack(mtm, 0.0, 0.0);
        let top = vertical_stack(mtm, 6.0, MARGIN);
        let preview = MenuBarPreview::new(mtm, width - MARGIN * 2.0, PREVIEW_HEIGHT, callbacks);
        top.addArrangedSubview(&preview);
        let hint = label(mtm, "Your menu bar. Drag an item to move it, or click it to edit.", 11.0, false);
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        top.addArrangedSubview(&hint);
        root.addArrangedSubview(&top);

        let columns = horizontal_stack(mtm, MARGIN);
        columns.setAlignment(NSLayoutAttribute::Top);
        let side = vertical_stack(mtm, 2.0, 0.0);
        let mut sidebar = Vec::new();
        for (index, page) in PAGES.iter().enumerate() {
            // SAFETY: the target outlives the window and implements the action.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(page),
                    Some(target),
                    Some(sel!(sidebarClicked:)),
                    mtm,
                )
            };
            button.setButtonType(NSButtonType::PushOnPushOff);
            button.setBezelStyle(NSBezelStyle::AccessoryBarAction);
            button.setAlignment(NSTextAlignment::Left);
            button.setTag(index as isize);
            fix_width(&button, SIDEBAR_WIDTH);
            side.addArrangedSubview(&button);
            sidebar.push(button);
        }
        columns.addArrangedSubview(&side);
        // Pages scroll, since a long list of fan presets can be taller than the screen.
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(FORM_WIDTH, 400.0)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.widthAnchor().constraintEqualToConstant(FORM_WIDTH + SCROLLER_ROOM).setActive(true);
        let scroll_height = scroll.heightAnchor().constraintEqualToConstant(400.0);
        scroll_height.setActive(true);
        columns.addArrangedSubview(&scroll);
        columns.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: 0.0, left: MARGIN, bottom: MARGIN, right: MARGIN });
        root.addArrangedSubview(&columns);
        // Fitted stacks ignore bottom insets, so a spacer holds the window's bottom margin.
        let bottom = NSView::new(mtm);
        bottom.heightAnchor().constraintEqualToConstant(MARGIN).setActive(true);
        root.addArrangedSubview(&bottom);

        window.setContentView(Some(&root));
        window.center();
        SettingsWindow {
            window,
            scroll,
            scroll_height,
            sidebar,
            preview,
            current: RefCell::new(None),
            readouts: RefCell::new(Vec::new()),
            page: Cell::new(0),
            has_items: Cell::new(false),
        }
    }

    pub fn page(&self) -> i64 {
        self.page.get()
    }

    pub fn has_preview_items(&self) -> bool {
        self.has_items.get()
    }

    pub fn set_preview(&self, items: Vec<BarItem>, spacing: f64) {
        self.has_items.set(true);
        self.preview.set_items(items, spacing);
    }

    pub fn update_readout(&self, tag: i64, value: f64) {
        for (readout_tag, field, unit) in self.readouts.borrow().iter() {
            if *readout_tag == tag {
                field.setStringValue(&NSString::from_str(&readout_text(value, unit)));
            }
        }
    }

    fn control_for(
        &self,
        mtm: MainThreadMarker,
        row: &FormRow,
        page: i64,
        target: &AnyObject,
        on_curve: &Rc<dyn Fn(usize, Vec<CurvePoint>)>,
    ) -> Retained<NSView> {
        let action = Some(sel!(settingChanged:));
        // SAFETY: the target outlives the window and implements the action.
        unsafe {
            match &row.control {
                Control::Heading => NSView::new(mtm),
                Control::Note(text) => {
                    let note = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
                    note.setSelectable(false);
                    note.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(11.0)));
                    note.setTextColor(Some(&NSColor::secondaryLabelColor()));
                    fix_width(&note, CONTROL_WIDTH);
                    note.setPreferredMaxLayoutWidth(CONTROL_WIDTH);
                    Retained::into_super(Retained::into_super(note))
                }
                Control::Curve(points) => {
                    let column = vertical_stack(mtm, 4.0, 0.0);
                    let index = settings_form::preset_index(row.tag).unwrap_or(0);
                    let on_curve = on_curve.clone();
                    column.addArrangedSubview(&CurveView::new(mtm, CONTROL_WIDTH, 110.0, points.clone(), Box::new(move |points| on_curve(index, points))));
                    let note = label(mtm, "Drag the points. Left to right is 30–105°C, bottom to top is speed.", 11.0, false);
                    note.setTextColor(Some(&NSColor::secondaryLabelColor()));
                    column.addArrangedSubview(&note);
                    Retained::into_super(column)
                }
                Control::Checkbox(on) => {
                    let button = NSButton::checkboxWithTitle_target_action(&NSString::from_str(""), Some(target), action, mtm);
                    button.setState(if *on { NSControlStateValueOn } else { NSControlStateValueOff });
                    button.setTag(row.tag as isize);
                    Retained::into_super(Retained::into_super(button))
                }
                Control::Segmented(options, selected) => segmented(mtm, options, *selected, row.tag, target),
                Control::Colors(first, second) => {
                    let line = horizontal_stack(mtm, 8.0);
                    line.addArrangedSubview(&color_well(mtm, first, row.tag, target));
                    line.addArrangedSubview(&color_well(mtm, second, row.tag + 1, target));
                    Retained::into_super(line)
                }
                Control::Menu(entries) => {
                    let popup = NSPopUpButton::initWithFrame_pullsDown(
                        NSPopUpButton::alloc(mtm),
                        NSRect::new(NSPoint::ZERO, NSSize::new(200.0, 24.0)),
                        true,
                    );
                    for entry in entries {
                        popup.addItemWithTitle(&NSString::from_str(entry));
                    }
                    popup.setTarget(Some(target));
                    popup.setAction(action);
                    popup.setTag(row.tag as isize);
                    fix_width(&popup, 200.0);
                    Retained::into_super(Retained::into_super(Retained::into_super(popup)))
                }
                Control::Icons => icon_palette(mtm, page, target),
                Control::Button(title) => {
                    let button = NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), action, mtm);
                    button.setTag(row.tag as isize);
                    Retained::into_super(Retained::into_super(button))
                }
                Control::Text(text, placeholder) => {
                    let field = NSTextField::textFieldWithString(&NSString::from_str(text), mtm);
                    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                    field.setTarget(Some(target));
                    field.setAction(action);
                    field.setTag(row.tag as isize);
                    // Preset names apply on Return, since renaming rebuilds the panels.
                    field.setContinuous(settings_form::preset_index(row.tag).is_none());
                    fix_width(&field, CONTROL_WIDTH);
                    Retained::into_super(Retained::into_super(field))
                }
                Control::Slider { value, min, max, unit } => {
                    let line = horizontal_stack(mtm, 8.0);
                    let slider = NSSlider::sliderWithValue_minValue_maxValue_target_action(*value, *min, *max, Some(target), action, mtm);
                    slider.setTag(row.tag as isize);
                    fix_width(&slider, CONTROL_WIDTH - READOUT_WIDTH - 8.0);
                    line.addArrangedSubview(&slider);
                    let readout = label(mtm, &readout_text(*value, unit), 12.0, false);
                    readout.setTextColor(Some(&NSColor::secondaryLabelColor()));
                    fix_width(&readout, READOUT_WIDTH);
                    line.addArrangedSubview(&readout);
                    self.readouts.borrow_mut().push((row.tag, readout, unit));
                    Retained::into_super(line)
                }
            }
        }
    }

    pub fn show_page(
        &self,
        mtm: MainThreadMarker,
        page: i64,
        settings: &Settings,
        target: &AnyObject,
        on_curve: Rc<dyn Fn(usize, Vec<CurvePoint>)>,
    ) {
        self.page.set(page);
        for (index, button) in self.sidebar.iter().enumerate() {
            button.setState(if index as i64 == page { NSControlStateValueOn } else { NSControlStateValueOff });
            button.setAttributedTitle(&sidebar_title(index, index as i64 == page, settings));
        }
        self.current.borrow_mut().take();
        self.readouts.borrow_mut().clear();
        let form = vertical_stack(mtm, 9.0, 0.0);
        form.addArrangedSubview(&label(mtm, PAGES[page as usize], 20.0, true));
        for row in settings_form::rows(page, settings) {
            if row.control == Control::Heading {
                let spacer = NSView::new(mtm);
                spacer.heightAnchor().constraintEqualToConstant(4.0).setActive(true);
                form.addArrangedSubview(&spacer);
                form.addArrangedSubview(&label(mtm, &row.label, 13.0, true));
                let rule = NSBox::new(mtm);
                rule.setBoxType(NSBoxType::Separator);
                fix_width(&rule, FORM_WIDTH);
                form.addArrangedSubview(&rule);
                continue;
            }
            let line = horizontal_stack(mtm, 12.0);
            line.setAlignment(if matches!(row.control, Control::Curve(_)) {
                NSLayoutAttribute::Top
            } else if matches!(row.control, Control::Colors(..)) {
                NSLayoutAttribute::CenterY
            } else {
                NSLayoutAttribute::FirstBaseline
            });
            let name = label(mtm, &row.label, 13.0, false);
            name.setAlignment(NSTextAlignment::Right);
            name.setTextColor(Some(&NSColor::secondaryLabelColor()));
            fix_width(&name, LABEL_WIDTH);
            line.addArrangedSubview(&name);
            line.addArrangedSubview(&self.control_for(mtm, &row, page, target, &on_curve));
            form.addArrangedSubview(&line);
        }
        form.layoutSubtreeIfNeeded();
        let height = form.fittingSize().height;
        form.setTranslatesAutoresizingMaskIntoConstraints(true);
        form.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(FORM_WIDTH, height)));
        self.scroll.setDocumentView(Some(&form));
        self.scroll_height.setConstant(page_height(height, self.available_height(mtm)));
        form.scrollPoint(NSPoint::new(0.0, if form.isFlipped() { 0.0 } else { height }));
        *self.current.borrow_mut() = Some(form);
        if let Some(content) = self.window.contentView() {
            content.layoutSubtreeIfNeeded();
            self.window.setContentSize(content.fittingSize());
        }
    }

    /// Room for a page after the window's title bar, menu bar preview and margins.
    fn available_height(&self, mtm: MainThreadMarker) -> f64 {
        NSScreen::mainScreen(mtm).map_or(FALLBACK_SCREEN_HEIGHT, |s| s.visibleFrame().size.height) - WINDOW_CHROME_HEIGHT
    }
}

/// A page's height, up to what fits on screen. Anything taller scrolls.
pub fn page_height(content: f64, available: f64) -> f64 {
    content.min(available.max(MIN_PAGE_HEIGHT)).max(MIN_PAGE_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_pages_scroll_and_short_ones_keep_the_sidebar_visible() {
        assert_eq!(page_height(600.0, 900.0), 600.0);
        assert_eq!(page_height(1400.0, 900.0), 900.0);
        assert_eq!(page_height(100.0, 900.0), MIN_PAGE_HEIGHT);
    }

    #[test]
    fn readouts_are_whole_numbers_with_their_unit() {
        assert_eq!(readout_text(12.4, " pt"), "12 pt");
        assert_eq!(readout_text(98.0, "°C"), "98°C");
    }
}
