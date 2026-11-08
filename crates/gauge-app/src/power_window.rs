//! The full Power window: the flow diagram over three cards and a row of graphs.

use crate::flow_view::FlowView;
use crate::graph_math::Unit;
use crate::graph_view::{GraphData, GraphView, SeriesData};
use crate::panel::slot_color;
use crate::ui::{fix_width, horizontal_stack, label, rows_text, vertical_stack};
use crate::view::{self, Latest, Slot};
use gauge_core::history::{History, bridge_gaps, bucketize};
use gauge_core::settings::{GraphScale, GraphStyle, RANGE_LABELS, Settings, TemperatureUnit};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSLayoutAttribute, NSSegmentSwitchTracking, NSSegmentedControl,
    NSTextField, NSTitlePosition, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSSize, NSString};
use std::cell::Cell;

const WIDTH: f64 = 700.0;
const MARGIN: f64 = 16.0;
const COLUMNS: f64 = 3.0;
const COLUMN_GAP: f64 = 12.0;
const GRAPH_POINTS: usize = 100;
const MAX_BRIDGED_GAP_SECONDS: f64 = 180.0;
const SLOW_GRAPH_SECONDS: i64 = 30;
const CARD_TEXT_SIZE: f64 = 12.0;

fn column_width() -> f64 {
    (WIDTH - MARGIN * 2.0 - COLUMN_GAP * (COLUMNS - 1.0)) / COLUMNS
}

/// A graph with the history series it draws, and how to label them.
type PowerGraph = (Retained<GraphView>, Vec<(&'static str, Slot)>, Unit);
type GraphSpec = (&'static str, Vec<(&'static str, Slot)>, Unit);

struct Card {
    body: Retained<NSTextField>,
}

pub struct PowerWindow {
    pub window: Retained<NSWindow>,
    flow: Retained<FlowView>,
    cards: [Card; 3],
    range: Retained<NSSegmentedControl>,
    graphs: [PowerGraph; 3],
    last_graph_at: Cell<i64>,
}

fn card(mtm: MainThreadMarker, title: &str) -> (Retained<NSBox>, Card) {
    let plate = NSBox::new(mtm);
    plate.setBoxType(NSBoxType::Custom);
    plate.setTitlePosition(NSTitlePosition::NoTitle);
    plate.setBorderWidth(0.0);
    plate.setCornerRadius(8.0);
    plate.setFillColor(&NSColor::systemGrayColor().colorWithAlphaComponent(0.1));
    let stack = vertical_stack(mtm, 4.0, 10.0);
    stack.addArrangedSubview(&label(mtm, title, 13.0, true));
    let inner = column_width() - 20.0;
    let body = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    body.setSelectable(false);
    fix_width(&body, inner);
    body.setPreferredMaxLayoutWidth(inner);
    stack.addArrangedSubview(&body);
    plate.setContentView(Some(&stack));
    fix_width(&plate, column_width());
    (plate, Card { body })
}

impl PowerWindow {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject, range_tag: isize) -> PowerWindow {
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        // SAFETY: the rect and style are valid. The window stays alive with this struct, and
        // isn't released on close so it can be shown again.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 640.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("Power"));
        let root = vertical_stack(mtm, 14.0, MARGIN);
        let flow = FlowView::new(mtm, WIDTH - MARGIN * 2.0, 190.0);
        root.addArrangedSubview(&flow);

        let card_row = horizontal_stack(mtm, COLUMN_GAP);
        card_row.setAlignment(NSLayoutAttribute::Top);
        let (battery_plate, battery_card) = card(mtm, "Battery");
        let (health_plate, health_card) = card(mtm, "Health");
        let (adapter_plate, adapter_card) = card(mtm, "Power adapter");
        for plate in [&battery_plate, &health_plate, &adapter_plate] {
            card_row.addArrangedSubview(plate);
        }
        root.addArrangedSubview(&card_row);

        let titles: Vec<Retained<NSString>> = RANGE_LABELS.iter().map(|l| NSString::from_str(l)).collect();
        // SAFETY: the target outlives the window and implements the action.
        let range = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_retained_slice(&titles),
                NSSegmentSwitchTracking::SelectOne,
                Some(target),
                Some(sel!(rangeChanged:)),
                mtm,
            )
        };
        range.setTag(range_tag);
        fix_width(&range, WIDTH - MARGIN * 2.0);
        root.addArrangedSubview(&range);

        let graph_row = horizontal_stack(mtm, COLUMN_GAP);
        let specs: [GraphSpec; 3] = [
            ("Power use and incoming", vec![("power.load", Slot::Primary), ("power.in", Slot::Secondary)], Unit::Watts),
            ("Battery charge", vec![("battery.percent", Slot::Primary)], Unit::Percent),
            ("Battery temperature", vec![("battery.temp", Slot::Secondary)], Unit::Celsius),
        ];
        let graphs = specs.map(|(title, series, unit)| {
            let column = vertical_stack(mtm, 4.0, 0.0);
            let name = label(mtm, title, 11.0, false);
            name.setTextColor(Some(&NSColor::secondaryLabelColor()));
            column.addArrangedSubview(&name);
            let graph = GraphView::new(mtm, column_width(), 110.0);
            column.addArrangedSubview(&graph);
            graph_row.addArrangedSubview(&column);
            (graph, series, unit)
        });
        root.addArrangedSubview(&graph_row);
        window.setContentView(Some(&root));
        window.center();
        PowerWindow {
            window,
            flow,
            cards: [battery_card, health_card, adapter_card],
            range,
            graphs,
            last_graph_at: Cell::new(0),
        }
    }

    pub fn refresh_text(&self, latest: &Latest, settings: &Settings) {
        self.flow.set_flow(view::power_flow(latest));
        let cards = view::power_cards(latest, settings);
        for (card, (_, rows)) in self.cards.iter().zip(cards.iter()) {
            card.body.setAttributedStringValue(&rows_text(rows, column_width() - 24.0, CARD_TEXT_SIZE));
        }
    }

    pub fn refresh_graphs(&self, history: &History, settings: &Settings, now: i64, force: bool) {
        let module = &settings.battery;
        let selected = crate::panel::Panel::range_seconds(module);
        let first = history.first_time().ok().flatten();
        let range = match selected {
            gauge_core::settings::ALL_HISTORY => first.map_or(60, |t| (now - t).max(60)),
            fixed => fixed,
        };
        let due = if range <= 3600 { 1 } else { SLOW_GRAPH_SECONDS };
        if !force && now - self.last_graph_at.get() < due {
            return;
        }
        self.last_graph_at.set(now);
        self.range.setSelectedSegment(module.range_index as isize);
        for (graph, ids, unit) in &self.graphs {
            let fahrenheit = *unit == Unit::Celsius && settings.temperature_unit == TemperatureUnit::Fahrenheit;
            let series = ids
                .iter()
                .map(|(id, slot)| {
                    let mut rows = history.rows(id, range, now).unwrap_or_default();
                    if fahrenheit {
                        for row in &mut rows {
                            row.avg = row.avg * 9.0 / 5.0 + 32.0;
                            row.max = row.max * 9.0 / 5.0 + 32.0;
                        }
                    }
                    let mut buckets = bucketize(&rows, now - range, now, GRAPH_POINTS);
                    bridge_gaps(&mut buckets, range as f64 / GRAPH_POINTS as f64, MAX_BRIDGED_GAP_SECONDS);
                    SeriesData { color: slot_color(module, *slot), buckets }
                })
                .collect();
            graph.set_data(GraphData {
                series,
                style: GraphStyle::Area,
                scale: GraphScale::Linear,
                show_peaks: false,
                unit: if fahrenheit { Unit::Fahrenheit } else { *unit },
                range_seconds: range,
            });
        }
    }

    pub fn content(&self) -> Option<Retained<NSView>> {
        self.window.contentView()
    }
}
