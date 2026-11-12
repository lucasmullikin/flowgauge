//! The click panel for one monitor: graph first, then details, as chosen from the mockups.

use crate::flow_view::FlowView;
use crate::graph_view::{GraphData, GraphView, SeriesData};
use crate::ui::{fix_width, horizontal_stack, label, legend_text, rows_text, vertical_stack};
use crate::view::{self, Content, GraphSpec, Latest, Slot};
use gauge_core::history::{History, bridge_gaps, bucketize, format_time};
use gauge_core::settings::{ALL_HISTORY, FanMode, ModuleSettings, RANGE_LABELS, RANGES_SECONDS, Settings, parse_color};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2_app_kit::{
    NSButton, NSPopover, NSPopoverBehavior, NSSegmentSwitchTracking, NSSegmentedControl, NSSlider,
    NSStackView, NSTextField, NSView, NSViewController,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSString};
use std::cell::{Cell, RefCell};

pub const CONTENT_WIDTH: f64 = 300.0;
/// The Network panel is wider, for its flow diagram and connection names.
const NETWORK_WIDTH: f64 = 400.0;

pub fn panel_width(module: &str) -> f64 {
    if module == "network" { NETWORK_WIDTH } else { CONTENT_WIDTH }
}
const GRAPH_POINTS: usize = 130;
pub const INSET: f64 = 16.0;
const TEXT_SIZE: f64 = 12.0;
const SLOW_GRAPH_SECONDS: i64 = 30;
/// Longer than any monitor's sampling and the once-a-minute saves, but shorter than real absences.
const MAX_BRIDGED_GAP_SECONDS: f64 = 180.0;
const MIN_ALL_RANGE_SECONDS: i64 = 60;
const FALLBACK_COLOR: (f64, f64, f64) = (0.2, 0.5, 1.0);

pub fn slot_color(module: &ModuleSettings, slot: Slot) -> (f64, f64, f64) {
    let hex = match slot {
        Slot::Primary => &module.color,
        Slot::Secondary => &module.secondary_color,
    };
    parse_color(hex).unwrap_or(FALLBACK_COLOR)
}

fn segmented(mtm: MainThreadMarker, labels: &[&str], target: &AnyObject, action: Sel, tag: isize) -> Retained<NSSegmentedControl> {
    let titles: Vec<Retained<NSString>> = labels.iter().map(|l| NSString::from_str(l)).collect();
    // SAFETY: the target outlives the panel and implements the action.
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
    fix_width(&control, CONTENT_WIDTH);
    control
}

pub struct ChargeControls {
    pub flow: Retained<FlowView>,
    pub limit_label: Retained<NSTextField>,
    pub slider: Retained<NSSlider>,
    pub top_up: Retained<NSButton>,
}

pub struct NetworkControls {
    pub flow: Retained<crate::net_flow_view::NetFlowView>,
    pub path: Retained<crate::path_view::PathStrip>,
    pub message: Retained<NSTextField>,
}

pub struct FanControls {
    pub presets: Retained<NSSegmentedControl>,
    pub fixed_row: Retained<NSStackView>,
    pub slider: Retained<NSSlider>,
    pub install: Retained<NSButton>,
}

pub struct Panel {
    pub module: &'static str,
    pub popover: Retained<NSPopover>,
    pub root: Retained<NSStackView>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
    range: Retained<NSSegmentedControl>,
    graphs: Vec<(Retained<GraphView>, GraphSpec)>,
    since: Retained<NSTextField>,
    legend: Retained<NSTextField>,
    details: Retained<NSTextField>,
    fan: Option<FanControls>,
    charge: Option<ChargeControls>,
    network: Option<NetworkControls>,
    last_content: RefCell<Content>,
    last_graph_at: Cell<i64>,
}

impl Panel {
    pub fn new(mtm: MainThreadMarker, module: &'static str, index: usize, settings: &Settings, target: &AnyObject) -> Panel {
        let root = vertical_stack(mtm, 8.0, INSET);
        // Pinned, because a popover otherwise sizes this stack to its content and drops the insets.
        let width = panel_width(module);
        fix_width(&root, width + INSET * 2.0);
        let header = horizontal_stack(mtm, 8.0);
        let title = label(mtm, "", 13.0, true);
        let subtitle = label(mtm, "", TEXT_SIZE, false);
        subtitle.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        header.addArrangedSubview(&title);
        header.addArrangedSubview(&subtitle);
        root.addArrangedSubview(&header);

        let fan = (module == "fans").then(|| {
            let names: Vec<&str> = settings.presets.iter().map(|p| p.name.as_str()).collect();
            let presets = segmented(mtm, &names, target, sel!(presetChanged:), 0);
            root.addArrangedSubview(&presets);
            let fixed_row = horizontal_stack(mtm, 8.0);
            fixed_row.addArrangedSubview(&label(mtm, "Speed", TEXT_SIZE, false));
            // SAFETY: the target outlives the panel and implements the action.
            let slider = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(
                    50.0, 0.0, 100.0, Some(target), Some(sel!(fixedChanged:)), mtm,
                )
            };
            fix_width(&slider, CONTENT_WIDTH - 50.0);
            fixed_row.addArrangedSubview(&slider);
            root.addArrangedSubview(&fixed_row);
            // SAFETY: the target outlives the panel and implements the action.
            let install = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str("Install fan helper…"),
                    Some(target),
                    Some(sel!(installHelper:)),
                    mtm,
                )
            };
            root.addArrangedSubview(&install);
            FanControls { presets, fixed_row, slider, install }
        });

        let is_power = module == "battery";
        let is_network = module == "network";
        // These panels lead with a flow diagram, so their range buttons and graph come after it.
        let has_flow = is_power || is_network;
        let range = segmented(mtm, &RANGE_LABELS, target, sel!(rangeChanged:), index as isize);
        if !has_flow {
            root.addArrangedSubview(&range);
        }
        let network = is_network.then(|| {
            let flow = crate::net_flow_view::NetFlowView::new(mtm, width, 200.0);
            root.addArrangedSubview(&flow);
            let path = crate::path_view::PathStrip::new(mtm, width, 46.0, false);
            root.addArrangedSubview(&path);
            let message = label(mtm, "", 11.0, false);
            message.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
            fix_width(&message, width);
            root.addArrangedSubview(&message);
            NetworkControls { flow, path, message }
        });
        let charge = is_power.then(|| {
            let flow = FlowView::new(mtm, CONTENT_WIDTH, 170.0);
            root.addArrangedSubview(&flow);
            let line = horizontal_stack(mtm, 8.0);
            let limit_label = label(mtm, "", TEXT_SIZE, true);
            line.addArrangedSubview(&limit_label);
            // SAFETY: the target outlives the panel and implements the action.
            let top_up = unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::from_str("Top Up"), Some(target), Some(sel!(topUp:)), mtm)
            };
            line.addArrangedSubview(&top_up);
            root.addArrangedSubview(&line);
            // SAFETY: the target outlives the panel and implements the action.
            let slider = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(100.0, f64::from(gauge_core::battery::MIN_CHARGE_LIMIT), 100.0, Some(target), Some(sel!(chargeLimitChanged:)), mtm)
            };
            slider.setNumberOfTickMarks(5);
            slider.setAllowsTickMarkValuesOnly(true);
            fix_width(&slider, CONTENT_WIDTH);
            root.addArrangedSubview(&slider);
            ChargeControls { flow, limit_label, slider, top_up }
        });

        if has_flow {
            root.addArrangedSubview(&range);
        }
        let specs = view::graphs(module, settings);
        let graphs: Vec<(Retained<GraphView>, GraphSpec)> = specs
            .into_iter()
            .map(|spec| {
                if let Some(caption) = spec.caption {
                    let name = label(mtm, caption, 11.0, false);
                    name.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
                    root.addArrangedSubview(&name);
                }
                let graph = GraphView::new(mtm, width, spec.height);
                root.addArrangedSubview(&graph);
                (graph, spec)
            })
            .collect();

        let since = label(mtm, "", 11.0, false);
        since.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        fix_width(&since, width);
        root.addArrangedSubview(&since);

        let legend = label(mtm, "", TEXT_SIZE, false);
        fix_width(&legend, width);
        root.addArrangedSubview(&legend);

        let details = NSTextField::wrappingLabelWithString(&NSString::from_str(""), mtm);
        details.setSelectable(false);
        fix_width(&details, width);
        details.setPreferredMaxLayoutWidth(width);
        root.addArrangedSubview(&details);

        let footer = horizontal_stack(mtm, 8.0);
        let mut buttons = vec![("Settings…", sel!(openSettings:)), ("Quit FlowGauge", sel!(quit:))];
        if module == "fans" {
            buttons.insert(0, ("All sensors…", sel!(openSensors:)));
        }
        if module == "battery" {
            buttons.insert(0, ("Details…", sel!(openPower:)));
        }
        if is_network {
            buttons.insert(0, ("Details…", sel!(openNetwork:)));
        }
        for (text, action) in buttons {
            // SAFETY: the target outlives the panel and implements the action.
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::from_str(text), Some(target), Some(action), mtm)
            };
            footer.addArrangedSubview(&button);
        }
        root.addArrangedSubview(&footer);

        let controller = NSViewController::new(mtm);
        controller.setView(&root);
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setAnimates(false);
        popover.setContentViewController(Some(&controller));

        Panel {
            module,
            popover,
            root,
            title,
            subtitle,
            range,
            graphs,
            since,
            legend,
            details,
            fan,
            charge,
            network,
            last_content: RefCell::new(Content::default()),
            last_graph_at: Cell::new(0),
        }
    }

    fn settings_for<'a>(&self, settings: &'a Settings) -> Option<&'a ModuleSettings> {
        settings.module(self.module)
    }

    /// Updates text. Returns quickly when nothing changed, so an idle panel doesn't redraw.
    pub fn refresh_text(&self, latest: &Latest, settings: &Settings) {
        let content = view::content(self.module, latest, settings);
        if let Some(fan) = &self.fan {
            self.refresh_fan(fan, latest, settings);
        }
        if let Some(charge) = &self.charge {
            self.refresh_charge(charge, latest, settings);
        }
        if let Some(network) = &self.network {
            let empty = if settings.show_top_apps { "Waiting for traffic" } else { "App traffic is off in Settings" };
            network.flow.set_flow(crate::net_view::live_flow(latest, crate::net_view::PANEL), empty);
            let path = crate::net_view::path_view(latest, !settings.check_host.is_empty());
            network.message.setStringValue(&NSString::from_str(&path.message));
            network.path.set_path(path);
        }
        if *self.last_content.borrow() == content {
            return;
        }
        self.title.setStringValue(&NSString::from_str(&content.title));
        self.subtitle.setStringValue(&NSString::from_str(&content.subtitle));
        if let (Some((_, spec)), Some(module)) = (self.graphs.first(), self.settings_for(settings)) {
            let entries: Vec<(String, (f64, f64, f64))> = content
                .legend
                .iter()
                .zip(&spec.series)
                .map(|(text, (_, slot))| (text.clone(), slot_color(module, *slot)))
                .collect();
            self.legend.setAttributedStringValue(&legend_text(&entries, TEXT_SIZE));
        }
        self.details.setAttributedStringValue(&rows_text(&content.rows, panel_width(self.module) - 2.0, TEXT_SIZE));
        let rows_changed = self.last_content.borrow().rows.len() != content.rows.len();
        *self.last_content.borrow_mut() = content;
        if rows_changed {
            self.fit();
        }
    }

    fn refresh_charge(&self, charge: &ChargeControls, latest: &Latest, settings: &Settings) {
        charge.flow.set_flow(view::power_flow(latest));
        let text = view::charge_limit_label(settings.charge_limit, latest.charge_limit, settings.top_up);
        charge.limit_label.setStringValue(&NSString::from_str(&text));
        charge.slider.setDoubleValue(view::slider_position(settings.charge_limit, latest.charge_limit));
        let title = if settings.top_up { "Cancel Top Up" } else { "Top Up" };
        charge.top_up.setTitle(&NSString::from_str(title));
    }

    fn refresh_fan(&self, fan: &FanControls, latest: &Latest, settings: &Settings) {
        let selected = settings.presets.iter().position(|p| p.name == settings.active_preset);
        fan.presets.setSelectedSegment(selected.map_or(-1, |i| i as isize));
        let fixed = settings.preset().and_then(|p| match &p.mode {
            FanMode::Fixed { percent } => Some(*percent),
            _ => None,
        });
        let show_install = latest.fan_state == crate::fan_control::FanState::HelperMissing;
        let changed = fan.fixed_row.isHidden() == fixed.is_some() || fan.install.isHidden() == show_install;
        fan.fixed_row.setHidden(fixed.is_none());
        fan.install.setHidden(!show_install);
        if let Some(percent) = fixed {
            fan.slider.setDoubleValue(f64::from(percent));
        }
        if changed {
            self.fit();
        }
    }

    pub fn fit(&self) {
        self.root.layoutSubtreeIfNeeded();
        self.popover.setContentSize(self.root.fittingSize());
    }

    pub fn range_seconds(settings: &ModuleSettings) -> i64 {
        RANGES_SECONDS[settings.range_index.min(RANGES_SECONDS.len() - 1)]
    }

    /// Short ranges redraw every second. Long ones change slowly, so they query less often.
    pub fn refresh_graphs(&self, history: &History, settings: &Settings, now: i64, force: bool) {
        let Some(module) = self.settings_for(settings) else { return };
        let first = history.first_time().ok().flatten();
        let range = match Self::range_seconds(module) {
            // The whole record so far, so this graph keeps growing with the app.
            ALL_HISTORY => first.map_or(MIN_ALL_RANGE_SECONDS, |t| (now - t).max(MIN_ALL_RANGE_SECONDS)),
            fixed => fixed,
        };
        let due = if range <= 3600 { 1 } else { SLOW_GRAPH_SECONDS };
        if !force && now - self.last_graph_at.get() < due {
            return;
        }
        self.last_graph_at.set(now);
        if let Some(first) = first {
            self.since.setStringValue(&NSString::from_str(&format!("History since {}", format_time(first))));
        }
        self.range.setSelectedSegment(module.range_index as isize);
        for (graph, spec) in &self.graphs {
            let series = spec
                .series
                .iter()
                .map(|(id, slot)| {
                    let mut buckets = bucketize(
                        &history.rows(id, range, now).unwrap_or_default(),
                        now - range,
                        now,
                        GRAPH_POINTS,
                    );
                    bridge_gaps(&mut buckets, range as f64 / GRAPH_POINTS as f64, MAX_BRIDGED_GAP_SECONDS);
                    SeriesData { color: slot_color(module, *slot), buckets }
                })
                .collect();
            graph.set_data(GraphData {
                series,
                style: module.graph_style,
                scale: if spec.unit == crate::graph_math::Unit::Percent {
                    gauge_core::settings::GraphScale::Linear
                } else {
                    module.graph_scale
                },
                show_peaks: module.show_peaks,
                unit: spec.unit,
                range_seconds: range,
            });
        }
    }

    /// Smallest left and right gaps between any content and the panel's edge.
    pub fn margins(&self) -> (f64, f64) {
        let width = self.root.bounds().size.width;
        let views: Vec<&NSView> = vec![&self.title, &self.range, &self.legend, &self.details];
        let graphs = self.graphs.iter().map(|(graph, _)| -> &NSView { graph });
        let mut left = f64::MAX;
        let mut right = f64::MAX;
        for view in views.into_iter().chain(graphs) {
            // Some panels leave out parts, like the Power panel's time range.
            // SAFETY: a plain read of the view hierarchy on the main thread.
            if unsafe { view.superview() }.is_none() {
                continue;
            }
            let frame = view.convertRect_toView(view.bounds(), Some(&self.root));
            left = left.min(frame.origin.x);
            right = right.min(width - frame.origin.x - frame.size.width);
        }
        (left, right)
    }

    pub fn fan_controls(&self) -> Option<&FanControls> {
        self.fan.as_ref()
    }

    pub fn range_control(&self) -> &NSSegmentedControl {
        &self.range
    }

    pub fn root_view(&self) -> &NSView {
        &self.root
    }
}
