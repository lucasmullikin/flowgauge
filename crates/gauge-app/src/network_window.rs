//! The full Network window: the flow on top, then speed and the app timeline beside the path
//! check and the busiest connections.

use crate::graph_math::Unit;
use crate::graph_view::{GraphData, GraphView, SeriesData};
use crate::net_flow_view::NetFlowView;
use crate::net_view::{self, APP_SORTS, NetFlow, TIMELINE_APPS, TIMELINE_RANGES, WINDOW};
use crate::panel::{Panel, slot_color};
use crate::path_view::PathStrip;
use crate::timeline_view::TimelineView;
use crate::ui::{fix_width, horizontal_stack, label, legend_text, rows_text, vertical_stack};
use crate::view::{Latest, Row, Slot, format_rate};
use gauge_core::history::{History, bridge_gaps, bucketize, local_midnight};
use gauge_core::names::Names;
use gauge_core::settings::{ALL_HISTORY, RANGE_LABELS, Settings};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSLayoutAttribute, NSSegmentSwitchTracking, NSSegmentedControl,
    NSStackView, NSTextField, NSTextView, NSTitlePosition, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSSize, NSString};
use std::cell::{Cell, RefCell};

const WIDTH: f64 = 760.0;
const MARGIN: f64 = 16.0;
const COLUMN_GAP: f64 = 14.0;
const CARD_PADDING: f64 = 12.0;
const GRAPH_POINTS: usize = 120;
const MAX_BRIDGED_GAP_SECONDS: f64 = 180.0;
const SLOW_REFRESH_SECONDS: i64 = 30;
const CONNECTION_ROWS: usize = 10;
const TEXT_SIZE: f64 = 12.0;
pub const FLOW_MODES: [&str; 3] = ["Now", "Today", "7 days"];
const FLOW_HEIGHT: f64 = 270.0;
const LIST_WIDTH: f64 = 270.0;
/// The app list sorts by total unless you pick another column.
const SORT_TOTAL: usize = 0;
const SORT_IN_OTHER: usize = 3;

fn column_width() -> f64 {
    (WIDTH - MARGIN * 2.0 - COLUMN_GAP) / 2.0
}

fn inner_width() -> f64 {
    column_width() - CARD_PADDING * 2.0
}

fn segmented(mtm: MainThreadMarker, labels: &[&str], target: &AnyObject, action: Sel, tag: isize) -> Retained<NSSegmentedControl> {
    let titles: Vec<Retained<NSString>> = labels.iter().map(|l| NSString::from_str(l)).collect();
    // SAFETY: the target outlives the window and implements the action.
    let control = unsafe {
        NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
            &NSArray::from_retained_slice(&titles),
            NSSegmentSwitchTracking::SelectOne,
            Some(target),
            Some(action),
            mtm,
        )
    };
    control.setTag(tag);
    control.setSelectedSegment(0);
    control
}

/// A rounded plate holding a titled stack, with an optional control on the title's right.
fn card(mtm: MainThreadMarker, title: &str, accessory: Option<&NSView>, width: f64) -> (Retained<NSBox>, Retained<NSStackView>) {
    let plate = NSBox::new(mtm);
    plate.setBoxType(NSBoxType::Custom);
    plate.setTitlePosition(NSTitlePosition::NoTitle);
    plate.setBorderWidth(0.0);
    plate.setCornerRadius(9.0);
    plate.setFillColor(&NSColor::systemGrayColor().colorWithAlphaComponent(0.1));
    let stack = vertical_stack(mtm, 8.0, CARD_PADDING);
    let header = horizontal_stack(mtm, 8.0);
    let name = label(mtm, title, 13.0, true);
    header.addArrangedSubview(&name);
    if let Some(accessory) = accessory {
        let spacer = NSView::new(mtm);
        header.addArrangedSubview(&spacer);
        header.addArrangedSubview(accessory);
    }
    fix_width(&header, width - CARD_PADDING * 2.0);
    stack.addArrangedSubview(&header);
    plate.setContentView(Some(&stack));
    fix_width(&plate, width);
    (plate, stack)
}

fn secondary(mtm: MainThreadMarker, width: f64) -> Retained<NSTextField> {
    let text = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
    text.setSelectable(false);
    text.setTextColor(Some(&NSColor::secondaryLabelColor()));
    fix_width(&text, width);
    text.setPreferredMaxLayoutWidth(width);
    text
}

pub struct NetworkWindow {
    pub window: Retained<NSWindow>,
    flow: Retained<NetFlowView>,
    flow_mode: Cell<usize>,
    /// Totals for Today and 7 days, worked out with the graphs since they change slowly.
    history_flows: RefCell<[Option<NetFlow>; 2]>,
    /// Every app's download and upload for Today and 7 days.
    history_apps: RefCell<[Vec<(String, f64, f64)>; 2]>,
    app_sort: Cell<usize>,
    sort_control: Retained<NSSegmentedControl>,
    apps_list: Retained<NSTextView>,
    last_list: RefCell<Vec<Row>>,
    range: Retained<NSSegmentedControl>,
    graph: Retained<GraphView>,
    legend: Retained<NSTextField>,
    timeline: Retained<TimelineView>,
    timeline_range: Cell<usize>,
    busiest: Retained<NSTextField>,
    path: Retained<PathStrip>,
    path_message: Retained<NSTextField>,
    connections: Retained<NSTextField>,
    last_slow_at: Cell<i64>,
    last_graph_at: Cell<i64>,
}

impl NetworkWindow {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject, range_tag: isize) -> NetworkWindow {
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        // SAFETY: the rect and style are valid. The window stays alive with this struct, and
        // isn't released on close so it can be shown again.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(WIDTH, 700.0)),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        window.setTitle(&NSString::from_str("Network"));
        let root = vertical_stack(mtm, COLUMN_GAP, MARGIN);

        let modes = segmented(mtm, &FLOW_MODES, target, sel!(flowModeChanged:), 0);
        let full = WIDTH - MARGIN * 2.0;
        let (flow_plate, flow_stack) = card(mtm, "Flow", Some(&modes), full);
        let flow_row = horizontal_stack(mtm, COLUMN_GAP);
        flow_row.setAlignment(NSLayoutAttribute::Top);
        let flow = NetFlowView::new(mtm, full - CARD_PADDING * 2.0 - COLUMN_GAP - LIST_WIDTH, FLOW_HEIGHT);
        flow.set_other_action(target, sel!(showOtherApps:));
        flow_row.addArrangedSubview(&flow);
        let list_column = vertical_stack(mtm, 6.0, 0.0);
        let sort_control = segmented(mtm, &APP_SORTS, target, sel!(appSortChanged:), 0);
        fix_width(&sort_control, LIST_WIDTH);
        list_column.addArrangedSubview(&sort_control);
        let scroll = NSTextView::scrollableTextView(mtm);
        scroll.setDrawsBackground(false);
        scroll.setHasVerticalScroller(true);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.widthAnchor().constraintEqualToConstant(LIST_WIDTH).setActive(true);
        scroll.heightAnchor().constraintEqualToConstant(FLOW_HEIGHT - 32.0).setActive(true);
        let apps_list = scroll
            .documentView()
            .and_then(|view| view.downcast::<NSTextView>().ok())
            .unwrap_or_else(|| NSTextView::new(mtm));
        apps_list.setEditable(false);
        apps_list.setSelectable(true);
        apps_list.setDrawsBackground(false);
        apps_list.setTextContainerInset(NSSize::new(0.0, 2.0));
        list_column.addArrangedSubview(&scroll);
        flow_row.addArrangedSubview(&list_column);
        flow_stack.addArrangedSubview(&flow_row);
        root.addArrangedSubview(&flow_plate);

        let columns = horizontal_stack(mtm, COLUMN_GAP);
        columns.setAlignment(NSLayoutAttribute::Top);
        let left = vertical_stack(mtm, COLUMN_GAP, 0.0);
        let right = vertical_stack(mtm, COLUMN_GAP, 0.0);

        let (speed_plate, speed_stack) = card(mtm, "Speed", None, column_width());
        let range = segmented(mtm, &RANGE_LABELS, target, sel!(rangeChanged:), range_tag);
        fix_width(&range, inner_width());
        speed_stack.addArrangedSubview(&range);
        let graph = GraphView::new(mtm, inner_width(), 110.0);
        speed_stack.addArrangedSubview(&graph);
        let legend = label(mtm, "", TEXT_SIZE, false);
        fix_width(&legend, inner_width());
        speed_stack.addArrangedSubview(&legend);
        left.addArrangedSubview(&speed_plate);

        let timeline_labels: Vec<&str> = TIMELINE_RANGES.iter().map(|r| r.0).collect();
        let timeline_control = segmented(mtm, &timeline_labels, target, sel!(timelineRangeChanged:), 0);
        let (timeline_plate, timeline_stack) = card(mtm, "Apps over time", Some(&timeline_control), column_width());
        let timeline = TimelineView::new(mtm, inner_width(), TIMELINE_APPS);
        timeline_stack.addArrangedSubview(&timeline);
        let busiest = secondary(mtm, inner_width());
        timeline_stack.addArrangedSubview(&busiest);
        left.addArrangedSubview(&timeline_plate);

        let (path_plate, path_stack) = card(mtm, "Path health", None, column_width());
        let path = PathStrip::new(mtm, inner_width(), 64.0, true);
        path_stack.addArrangedSubview(&path);
        let path_message = secondary(mtm, inner_width());
        path_stack.addArrangedSubview(&path_message);
        right.addArrangedSubview(&path_plate);

        let (connections_plate, connections_stack) = card(mtm, "Recent connections", None, column_width());
        let connections = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        connections.setSelectable(false);
        fix_width(&connections, inner_width());
        connections.setPreferredMaxLayoutWidth(inner_width());
        connections_stack.addArrangedSubview(&connections);
        right.addArrangedSubview(&connections_plate);

        columns.addArrangedSubview(&left);
        columns.addArrangedSubview(&right);
        root.addArrangedSubview(&columns);
        window.setContentView(Some(&root));
        window.center();
        NetworkWindow {
            window,
            flow,
            flow_mode: Cell::new(0),
            history_flows: RefCell::new([None, None]),
            history_apps: RefCell::new([Vec::new(), Vec::new()]),
            app_sort: Cell::new(SORT_TOTAL),
            sort_control,
            apps_list,
            last_list: RefCell::new(Vec::new()),
            range,
            graph,
            legend,
            timeline,
            timeline_range: Cell::new(0),
            busiest,
            path,
            path_message,
            connections,
            last_slow_at: Cell::new(0),
            last_graph_at: Cell::new(0),
        }
    }

    pub fn set_flow_mode(&self, mode: usize) {
        self.flow_mode.set(mode.min(FLOW_MODES.len() - 1));
    }

    pub fn set_app_sort(&self, sort: usize) {
        self.app_sort.set(sort.min(APP_SORTS.len() - 1));
        self.sort_control.setSelectedSegment(self.app_sort.get() as isize);
    }

    /// Clicking Other in the flow lists the apps grouped inside it.
    pub fn show_other(&self) {
        self.set_app_sort(SORT_IN_OTHER);
    }

    pub fn set_timeline_range(&self, index: usize) {
        self.timeline_range.set(index.min(TIMELINE_RANGES.len() - 1));
        self.last_slow_at.set(0);
    }

    pub fn refresh_text(&self, latest: &Latest, settings: &Settings) {
        let flow = match self.flow_mode.get() {
            0 => net_view::live_flow(latest, WINDOW),
            mode => self.history_flows.borrow()[mode - 1].clone(),
        };
        let apps: Vec<(String, f64, f64)> = match self.flow_mode.get() {
            0 => latest.traffic.as_ref().map_or_else(Vec::new, |t| t.apps.iter().map(|a| (a.name.clone(), a.down, a.up)).collect()),
            mode => self.history_apps.borrow()[mode - 1].clone(),
        };
        let in_flow: Vec<String> = flow.as_ref().map_or_else(Vec::new, |f| f.apps.iter().map(|a| a.label.clone()).collect());
        let rows = net_view::app_rows(&apps, self.app_sort.get(), &in_flow, self.flow_mode.get() == 0);
        if *self.last_list.borrow() != rows {
            // SAFETY: the text view always has storage, and this runs on the main thread.
            if let Some(storage) = unsafe { self.apps_list.textStorage() } {
                storage.setAttributedString(&rows_text(&rows, LIST_WIDTH - 22.0, TEXT_SIZE));
            }
            *self.last_list.borrow_mut() = rows;
        }
        let empty = if !settings.show_top_apps {
            "App traffic is off in Settings"
        } else if self.flow_mode.get() == 0 {
            "Waiting for traffic"
        } else {
            "No traffic saved for this period yet"
        };
        self.flow.set_flow(flow, empty);
        let path = net_view::path_view(latest, !settings.check_host.is_empty());
        let dns = latest.path.as_ref().and_then(net_view::dns_line).map(|line| format!("\nDNS: {line}.")).unwrap_or_default();
        let message = format!("{}. {}.{dns}", path.message, path.loss);
        self.path_message.setStringValue(&NSString::from_str(&message));
        self.path.set_path(path);
        self.connections.setAttributedStringValue(&rows_text(&connection_rows(latest), inner_width() - 2.0, TEXT_SIZE));
        if let Some(default) = latest.interfaces.iter().find(|i| i.is_default) {
            let module = &settings.network;
            let entries = vec![
                (format!("Down {}", format_rate(default.in_per_sec)), slot_color(module, Slot::Primary)),
                (format!("Up {}", format_rate(default.out_per_sec)), slot_color(module, Slot::Secondary)),
            ];
            self.legend.setAttributedStringValue(&legend_text(&entries, TEXT_SIZE));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn refresh_graphs(
        &self,
        history: &History,
        settings: &Settings,
        names: &Names,
        geo: Option<&gauge_core::geo::Geo>,
        latest: &Latest,
        now: i64,
        force: bool,
    ) {
        let module = &settings.network;
        let selected = Panel::range_seconds(module);
        let range = match selected {
            ALL_HISTORY => history.first_time().ok().flatten().map_or(60, |t| (now - t).max(60)),
            fixed => fixed,
        };
        let due = if range <= 3600 { 1 } else { SLOW_REFRESH_SECONDS };
        if force || now - self.last_graph_at.get() >= due {
            self.last_graph_at.set(now);
            self.range.setSelectedSegment(module.range_index as isize);
            let series = [("net.down", Slot::Primary), ("net.up", Slot::Secondary)]
                .into_iter()
                .map(|(id, slot)| {
                    let rows = history.rows(id, range, now).unwrap_or_default();
                    let mut buckets = bucketize(&rows, now - range, now, GRAPH_POINTS);
                    bridge_gaps(&mut buckets, range as f64 / GRAPH_POINTS as f64, MAX_BRIDGED_GAP_SECONDS);
                    SeriesData { color: slot_color(module, slot), buckets }
                })
                .collect();
            self.graph.set_data(GraphData {
                series,
                style: module.graph_style,
                scale: module.graph_scale,
                show_peaks: module.show_peaks,
                unit: Unit::Rate,
                range_seconds: range,
            });
        }
        if !force && now - self.last_slow_at.get() < SLOW_REFRESH_SECONDS {
            return;
        }
        self.last_slow_at.set(now);
        let (start, cell, count, label) = net_view::timeline_window(now, self.timeline_range.get());
        let cells = history.app_cells(start, cell).unwrap_or_default();
        let timeline = net_view::timeline(&cells, count, label);
        let busiest = match &timeline.busiest {
            Some((app, bytes)) => format!("Busiest: {app}, {}", crate::view::format_bytes(*bytes)),
            None => String::new(),
        };
        self.busiest.setStringValue(&NSString::from_str(&busiest));
        self.timeline.set_timeline(timeline);
        let source = net_view::source_label(latest);
        let resolve = settings.resolve_names;
        // Only names already looked up for live traffic, so this never sends new lookups.
        let name_of = |address| if resolve { names.cached(address) } else { None };
        let country_of = |address| geo.and_then(|g| g.country(address)).map(|code| net_view::country_name(&code));
        let mut flows: [Option<NetFlow>; 2] = [None, None];
        let mut app_lists: [Vec<(String, f64, f64)>; 2] = [Vec::new(), Vec::new()];
        for (slot, start) in [local_midnight(now), now - 7 * 86400].into_iter().enumerate() {
            let apps = history.app_totals(start).unwrap_or_default();
            let hosts = history.host_totals(start).unwrap_or_default();
            flows[slot] = net_view::history_flow(&source, &apps, &hosts, &name_of, &country_of, WINDOW);
            app_lists[slot] = apps.into_iter().map(|a| (a.app, a.bytes_in as f64, a.bytes_out as f64)).collect();
        }
        *self.history_flows.borrow_mut() = flows;
        *self.history_apps.borrow_mut() = app_lists;
    }

    /// Clicks the flow's "Other" the way a person would, and says whether the list switched to
    /// the apps inside it. `None` when the flow has no Other to click.
    pub fn click_other_for_test(&self) -> Option<bool> {
        let point = self.flow.other_app_center()?;
        let location = self.flow.convertPoint_toView(point, None);
        let event = objc2_app_kit::NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            objc2_app_kit::NSEventType::LeftMouseDown,
            location,
            objc2_app_kit::NSEventModifierFlags::empty(),
            0.0,
            self.window.windowNumber(),
            None,
            0,
            1,
            1.0,
        )?;
        self.flow.mouseDown(&event);
        Some(self.app_sort.get() == SORT_IN_OTHER)
    }

    pub fn moving_dots(&self) -> usize {
        self.flow.moving_dots()
    }

    pub fn content(&self) -> Option<Retained<NSView>> {
        self.window.contentView()
    }
}

fn connection_rows(latest: &Latest) -> Vec<Row> {
    let row = |key: String, value: String| Row { key, value, header: false };
    let Some(traffic) = &latest.traffic else {
        return vec![row("Waiting for traffic".into(), String::new())];
    };
    let recent = net_view::recent_connections(traffic, CONNECTION_ROWS);
    if recent.is_empty() {
        return vec![row("Nothing has moved in the last minute".into(), String::new())];
    }
    recent.into_iter().map(|(app, place, value)| row(format!("{app}: {place}"), value)).collect()
}
