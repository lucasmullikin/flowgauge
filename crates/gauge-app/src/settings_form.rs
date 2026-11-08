//! What each settings page contains, and how a control's value changes the settings. Pure, so
//! it's testable; the window only turns these rows into controls.

use crate::view;
use gauge_core::settings::{
    BarStyle, FanMode, GraphScale, GraphStyle, MAX_ITEM_SPACING, MAX_PRESETS, MODULES, RANGE_LABELS, RANGES_SECONDS,
    Settings, TemperatureUnit,
};
use gauge_core::curve::CurvePoint;

pub const PAGES: [&str; 10] = ["General", "Network", "Memory", "CPU", "GPU", "Drives", "Fans", "Power", "Alerts", "Fan presets"];
const GENERAL: i64 = 0;
const ALERTS: i64 = 8;
pub const PRESETS: i64 = 9;
const FIELD_EDIT_PRESETS: i64 = 30;
const FIELD_ADD_PRESET: i64 = 1;
const PRESET_TAG_BASE: i64 = 1_000_000;
const PRESET_NAME: i64 = 0;
const PRESET_KIND: i64 = 1;
const PRESET_SPEED: i64 = 2;
const PRESET_DELETE: i64 = 3;
const PRESET_CURVE: i64 = 4;
const FIELD_SHOWN: i64 = 0;
const FIELD_BAR: i64 = 1;
const FIELD_STYLE: i64 = 2;
const FIELD_SCALE: i64 = 3;
const FIELD_PEAKS: i64 = 4;
const FIELD_RANGE: i64 = 5;
const FIELD_COLOR: i64 = 6;
const FIELD_COLOR2: i64 = 7;
const FIELD_TEMPLATE: i64 = 8;
const FIELD_ADD_VALUE: i64 = 9;
const FIELD_ADD_ICON: i64 = 10;
const FIELD_RESET_TEXT: i64 = 11;
const FIELD_TOP_APPS: i64 = 20;
const FIELD_WIFI: i64 = 21;
const FIELD_WIFI_NAME: i64 = 23;
const FIELD_PING: i64 = 22;
const FIELD_NAMES: i64 = 24;
const FIELD_CHECK_HOST: i64 = 25;
const FIELD_CLEAR_NETWORK: i64 = 26;
/// Clears saved app and destination history rather than changing a setting.
pub const CLEAR_NETWORK_TAG: i64 = 100 + FIELD_CLEAR_NETWORK;
const FIELD_COUNTRIES: i64 = 27;
const FIELD_UPDATE_GEO: i64 = 28;
/// Downloads the location file again rather than changing a setting.
pub const UPDATE_GEO_TAG: i64 = 100 + FIELD_UPDATE_GEO;
const FIELD_UNIT: i64 = 1;
const FIELD_LOGIN: i64 = 2;
const FIELD_SPACING: i64 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    /// A section title rather than a control.
    Heading,
    Checkbox(bool),
    /// Few choices, all visible at once.
    Segmented(Vec<&'static str>, usize),
    /// Two color wells on one row; the second uses the row's tag plus one.
    Colors(String, String),
    Text(String, &'static str),
    Slider { value: f64, min: f64, max: f64, unit: &'static str },
    /// A menu that runs a command rather than holding a value. The first entry is its title.
    Menu(Vec<String>),
    /// One tap adds that icon. Each button's tag comes from `icon_tag`.
    Icons,
    /// A button that runs a command.
    Button(&'static str),
    /// A draggable fan curve. The row's tag says which preset it belongs to.
    Curve(Vec<CurvePoint>),
    /// Plain explanatory text.
    Note(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormRow {
    pub label: String,
    pub control: Control,
    pub tag: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Bool(bool),
    Index(usize),
    Color(String),
    Text(String),
    Number(f64),
}

fn heading(label: impl Into<String>) -> FormRow {
    FormRow { label: label.into(), control: Control::Heading, tag: -1 }
}

pub fn preset_tag(index: usize, field: i64) -> i64 {
    PRESET_TAG_BASE + index as i64 * 100 + field
}

/// Which preset a curve's tag belongs to.
pub fn preset_index(tag: i64) -> Option<usize> {
    (tag >= PRESET_TAG_BASE).then(|| ((tag - PRESET_TAG_BASE) / 100) as usize)
}

fn preset_rows(settings: &Settings) -> Vec<FormRow> {
    let mut form = vec![FormRow {
        label: "Auto".into(),
        control: Control::Note("Lets macOS control the fans. This one can't be changed or removed."),
        tag: -1,
    }];
    for (index, preset) in settings.presets.iter().enumerate().skip(1) {
        form.push(heading(preset.name.clone()));
        form.push(FormRow {
            label: "Name".into(),
            control: Control::Text(preset.name.clone(), "Name this preset"),
            tag: preset_tag(index, PRESET_NAME),
        });
        let is_curve = matches!(preset.mode, FanMode::Curve { .. });
        form.push(FormRow {
            label: "Type".into(),
            control: Control::Segmented(vec!["Fixed speed", "Temperature curve"], usize::from(is_curve)),
            tag: preset_tag(index, PRESET_KIND),
        });
        match &preset.mode {
            FanMode::Curve { points } => form.push(FormRow {
                label: "Curve".into(),
                control: Control::Curve(points.clone()),
                tag: preset_tag(index, PRESET_CURVE),
            }),
            FanMode::Fixed { percent } => form.push(FormRow {
                label: "Speed".into(),
                control: Control::Slider { value: f64::from(*percent), min: 0.0, max: 100.0, unit: "%" },
                tag: preset_tag(index, PRESET_SPEED),
            }),
            FanMode::Auto => {}
        }
        form.push(FormRow {
            label: String::new(),
            control: Control::Button("Delete preset"),
            tag: preset_tag(index, PRESET_DELETE),
        });
    }
    if settings.presets.len() < MAX_PRESETS {
        form.push(heading("New"));
        form.push(FormRow {
            label: String::new(),
            control: Control::Button("Add preset"),
            tag: PRESETS * 100 + FIELD_ADD_PRESET,
        });
    }
    form
}

fn default_curve() -> Vec<CurvePoint> {
    vec![
        CurvePoint { temp_c: 45.0, percent: 10.0 },
        CurvePoint { temp_c: 65.0, percent: 45.0 },
        CurvePoint { temp_c: 80.0, percent: 80.0 },
        CurvePoint { temp_c: 90.0, percent: 100.0 },
    ]
}

/// For a Button that opens another settings page rather than changing anything.
pub fn page_opened_by(tag: i64) -> Option<i64> {
    (tag == 6 * 100 + FIELD_EDIT_PRESETS).then_some(PRESETS)
}

fn apply_preset(settings: &mut Settings, tag: i64, input: Input) -> bool {
    let (index, field) = (((tag - PRESET_TAG_BASE) / 100) as usize, (tag - PRESET_TAG_BASE) % 100);
    if index == 0 || index >= settings.presets.len() {
        return false;
    }
    match (field, input) {
        (PRESET_NAME, Input::Text(name)) => settings.rename_preset(index, &name),
        (PRESET_DELETE, Input::Index(_)) => settings.delete_preset(index),
        (PRESET_SPEED, Input::Number(value)) => {
            settings.presets[index].mode = FanMode::Fixed { percent: (value as f32).clamp(0.0, 100.0) };
            true
        }
        (PRESET_KIND, Input::Index(kind)) => {
            let is_curve = matches!(settings.presets[index].mode, FanMode::Curve { .. });
            if kind == 1 && !is_curve {
                settings.presets[index].mode = FanMode::Curve { points: default_curve() };
            } else if kind == 0 && is_curve {
                settings.presets[index].mode = FanMode::Fixed { percent: 50.0 };
            }
            true
        }
        _ => false,
    }
}

pub fn set_preset_curve(settings: &mut Settings, index: usize, points: Vec<CurvePoint>) {
    if index > 0 && let Some(preset) = settings.presets.get_mut(index) {
        preset.mode = FanMode::Curve { points };
    }
}

const ICON_TAG_BASE: i64 = 100_000;

pub fn icon_tag(page: i64, index: usize) -> i64 {
    ICON_TAG_BASE + page * 100 + index as i64
}

fn module_rows(page: i64, settings: &Settings) -> Vec<FormRow> {
    let name = MODULES[page as usize - 1];
    let Some(module) = settings.module(name) else {
        return Vec::new();
    };
    let tag = |field| page * 100 + field;
    let bar = match module.bar_style {
        BarStyle::Text => 0,
        BarStyle::Graph => 1,
        BarStyle::Both => 2,
    };
    let style = match module.graph_style {
        GraphStyle::Line => 0,
        GraphStyle::Area => 1,
        GraphStyle::Bars => 2,
    };
    let scale = usize::from(module.graph_scale == GraphScale::Log);
    vec![
        heading("Menu bar"),
        FormRow { label: "Show this item".into(), control: Control::Checkbox(module.shown), tag: tag(FIELD_SHOWN) },
        FormRow { label: "Display".into(), control: Control::Segmented(vec!["Text", "Graph", "Both"], bar), tag: tag(FIELD_BAR) },
        FormRow {
            label: "Text".into(),
            control: Control::Text(view::template_for(name, settings).to_string(), "Empty shows no text"),
            tag: tag(FIELD_TEMPLATE),
        },
        FormRow {
            label: "Add value".into(),
            control: Control::Menu(
                std::iter::once("Choose a value".to_string())
                    .chain(view::tokens(name).iter().map(|t| t.label.to_string()))
                    .collect(),
            ),
            tag: tag(FIELD_ADD_VALUE),
        },
        FormRow { label: "Add icon".into(), control: Control::Icons, tag: tag(FIELD_ADD_ICON) },
        FormRow { label: "".into(), control: Control::Button("Reset text"), tag: tag(FIELD_RESET_TEXT) },
        heading("Graph"),
        FormRow { label: "Style".into(), control: Control::Segmented(vec!["Line", "Area", "Bars"], style), tag: tag(FIELD_STYLE) },
        FormRow { label: "Scale".into(), control: Control::Segmented(vec!["Linear", "Logarithmic"], scale), tag: tag(FIELD_SCALE) },
        FormRow { label: "Time range".into(), control: Control::Segmented(RANGE_LABELS.to_vec(), module.range_index), tag: tag(FIELD_RANGE) },
        FormRow { label: "Show peaks".into(), control: Control::Checkbox(module.show_peaks), tag: tag(FIELD_PEAKS) },
        FormRow { label: "Colors".into(), control: Control::Colors(module.color.clone(), module.secondary_color.clone()), tag: tag(FIELD_COLOR) },
    ]
}

/// Moves the panel's slider into the active preset, if that preset is a fixed speed.
pub fn set_active_fixed_percent(settings: &mut Settings, percent: f32) {
    let active = settings.active_preset.clone();
    for preset in &mut settings.presets {
        if preset.name == active && matches!(preset.mode, FanMode::Fixed { .. }) {
            preset.mode = FanMode::Fixed { percent: percent.clamp(0.0, 100.0) };
        }
    }
}

pub fn rows(page: i64, settings: &Settings) -> Vec<FormRow> {
    match page {
        GENERAL => vec![
            FormRow {
                label: "Temperature".into(),
                control: Control::Segmented(vec!["Celsius", "Fahrenheit"], usize::from(settings.temperature_unit == TemperatureUnit::Fahrenheit)),
                tag: FIELD_UNIT,
            },
            FormRow { label: "Open at login".into(), control: Control::Checkbox(settings.launch_at_login), tag: FIELD_LOGIN },
            FormRow {
                label: "Item spacing".into(),
                control: Control::Slider { value: settings.item_spacing, min: 0.0, max: MAX_ITEM_SPACING, unit: " pt" },
                tag: FIELD_SPACING,
            },
        ],
        ALERTS => {
            let a = &settings.alerts;
            vec![
                heading("Storage"),
                FormRow { label: "Startup disk low".into(), control: Control::Checkbox(a.disk_low_enabled), tag: ALERTS * 100 },
                FormRow { label: "Alert below".into(), control: Control::Slider { value: f64::from(a.disk_low_percent), min: 1.0, max: 50.0, unit: "% free" }, tag: ALERTS * 100 + 1 },
                heading("Temperature"),
                FormRow { label: "Running hot".into(), control: Control::Checkbox(a.hot_enabled), tag: ALERTS * 100 + 2 },
                FormRow { label: "Alert above".into(), control: Control::Slider { value: f64::from(a.hot_c), min: 60.0, max: 110.0, unit: "°C" }, tag: ALERTS * 100 + 3 },
                heading("System"),
                FormRow { label: "Memory pressure critical".into(), control: Control::Checkbox(a.memory_pressure_enabled), tag: ALERTS * 100 + 4 },
                FormRow { label: "Connection down".into(), control: Control::Checkbox(a.connection_down_enabled), tag: ALERTS * 100 + 5 },
            ]
        }
        PRESETS => preset_rows(settings),
        _ if (1..=7).contains(&page) => {
            let mut form = module_rows(page, settings);
            match MODULES[page as usize - 1] {
                "network" => {
                    form.push(heading("Apps and connections"));
                    form.push(FormRow { label: "Track app traffic".into(), control: Control::Checkbox(settings.show_top_apps), tag: page * 100 + FIELD_TOP_APPS });
                    form.push(FormRow { label: "Name connections".into(), control: Control::Checkbox(settings.resolve_names), tag: page * 100 + FIELD_NAMES });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("Asks your DNS server for each address's name, so it can see which addresses you connect to. Many names are generic, like a cloud provider's."),
                        tag: -1,
                    });
                    form.push(FormRow { label: "Show countries".into(), control: Control::Checkbox(settings.show_countries), tag: page * 100 + FIELD_COUNTRIES });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("Downloads DB-IP's free country file (about 5 MB) once from db-ip.com. Lookups happen on this Mac, so no address is sent anywhere. IP Geolocation by DB-IP, CC BY 4.0."),
                        tag: -1,
                    });
                    form.push(FormRow { label: String::new(), control: Control::Button("Update location data"), tag: UPDATE_GEO_TAG });
                    form.push(FormRow { label: "History".into(), control: Control::Button("Clear app and destination history"), tag: CLEAR_NETWORK_TAG });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("App totals are kept for 90 days and destinations for 7 days, only on this Mac."),
                        tag: -1,
                    });
                    form.push(heading("Path check"));
                    form.push(FormRow {
                        label: "Check address".into(),
                        control: Control::Text(settings.check_host.clone(), "Off. Try example.com"),
                        tag: page * 100 + FIELD_CHECK_HOST,
                    });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("FlowGauge always checks your router and the DNS server macOS uses. Set an address to also ping it every 10 seconds and test the internet."),
                        tag: -1,
                    });
                    form.push(heading("Details"));
                    form.push(FormRow { label: "Show Wi-Fi details".into(), control: Control::Checkbox(settings.show_wifi), tag: page * 100 + FIELD_WIFI });
                    form.push(FormRow { label: "Show Wi-Fi name".into(), control: Control::Checkbox(settings.show_wifi_name), tag: page * 100 + FIELD_WIFI_NAME });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("Showing the name asks for Location permission. macOS treats it as location data."),
                        tag: -1,
                    });
                    form.push(FormRow {
                        label: "Ping address".into(),
                        control: Control::Text(settings.ping_host.clone(), "Your gateway, which stays on your network"),
                        tag: page * 100 + FIELD_PING,
                    });
                }
                "fans" => {
                    form.push(heading("Fan control"));
                    form.push(FormRow {
                        label: "Presets".into(),
                        control: Control::Button("Edit fan presets"),
                        tag: page * 100 + FIELD_EDIT_PRESETS,
                    });
                    form.push(FormRow {
                        label: String::new(),
                        control: Control::Note("Fans return to Auto if FlowGauge quits or stays above 100°C."),
                        tag: -1,
                    });
                }
                _ => {}
            }
            form
        }
        _ => Vec::new(),
    }
}

/// Checkboxes report their state; plain buttons just say they were pressed.
pub fn input_for_button(tag: i64, on: bool) -> Input {
    let is_preset_button = tag >= PRESET_TAG_BASE || tag == PRESETS * 100 + FIELD_ADD_PRESET;
    if tag >= ICON_TAG_BASE || is_preset_button || tag % 100 == FIELD_RESET_TEXT {
        Input::Index(1)
    } else {
        Input::Bool(on)
    }
}

/// Commands change a text field that's on screen, so its page must be rebuilt to show that.
pub fn needs_page_refresh(tag: i64) -> bool {
    let edits_preset_list = tag >= PRESET_TAG_BASE && (tag - PRESET_TAG_BASE) % 100 != PRESET_SPEED;
    edits_preset_list || tag == PRESETS * 100 + FIELD_ADD_PRESET || tag >= ICON_TAG_BASE || tag / 100 != 0 && matches!(tag % 100, FIELD_ADD_VALUE | FIELD_ADD_ICON | FIELD_RESET_TEXT)
}

fn append_to_template(settings: &mut Settings, name: &str, text: &str) {
    let mut template = view::template_for(name, settings).to_string();
    template.push_str(text);
    if let Some(module) = settings.module_mut(name) {
        module.bar_template = Some(template);
    }
}

/// Returns false for a tag or value that doesn't fit, leaving settings unchanged.
pub fn apply(settings: &mut Settings, tag: i64, input: Input) -> bool {
    if tag >= PRESET_TAG_BASE {
        return apply_preset(settings, tag, input);
    }
    if tag == PRESETS * 100 + FIELD_ADD_PRESET {
        return settings.add_preset().is_some();
    }
    if tag >= ICON_TAG_BASE {
        let (page, index) = ((tag - ICON_TAG_BASE) / 100, (tag - ICON_TAG_BASE) % 100);
        return match (usize::try_from(page - 1).ok().and_then(|p| MODULES.get(p)), view::ICONS.get(index as usize)) {
            (Some(name), Some(icon)) => {
                append_to_template(settings, name, icon);
                true
            }
            _ => false,
        };
    }
    let (page, field) = (tag / 100, tag % 100);
    match (page, field, input) {
        (GENERAL, FIELD_UNIT, Input::Index(index)) => {
            settings.temperature_unit = if index == 1 { TemperatureUnit::Fahrenheit } else { TemperatureUnit::Celsius };
        }
        (GENERAL, FIELD_LOGIN, Input::Bool(on)) => settings.launch_at_login = on,
        (GENERAL, FIELD_SPACING, Input::Number(value)) => settings.item_spacing = value.clamp(0.0, MAX_ITEM_SPACING),
        (ALERTS, 0, Input::Bool(on)) => settings.alerts.disk_low_enabled = on,
        (ALERTS, 1, Input::Number(value)) => settings.alerts.disk_low_percent = value as f32,
        (ALERTS, 2, Input::Bool(on)) => settings.alerts.hot_enabled = on,
        (ALERTS, 3, Input::Number(value)) => settings.alerts.hot_c = value as f32,
        (ALERTS, 4, Input::Bool(on)) => settings.alerts.memory_pressure_enabled = on,
        (ALERTS, 5, Input::Bool(on)) => settings.alerts.connection_down_enabled = on,
        (1, FIELD_TOP_APPS, Input::Bool(on)) => settings.show_top_apps = on,
        (1, FIELD_WIFI, Input::Bool(on)) => settings.show_wifi = on,
        (1, FIELD_WIFI_NAME, Input::Bool(on)) => settings.show_wifi_name = on,
        (1, FIELD_PING, Input::Text(host)) => settings.ping_host = host.trim().to_string(),
        (1, FIELD_NAMES, Input::Bool(on)) => settings.resolve_names = on,
        (1, FIELD_COUNTRIES, Input::Bool(on)) => settings.show_countries = on,
        (1, FIELD_CHECK_HOST, Input::Text(host)) => settings.check_host = host.trim().to_string(),
        (1..=7, FIELD_ADD_VALUE, Input::Index(index)) => {
            let name = MODULES[page as usize - 1];
            // Entry 0 is the menu's title.
            let Some(token) = index.checked_sub(1).and_then(|i| view::tokens(name).get(i)) else {
                return false;
            };
            append_to_template(settings, name, &format!("{{{}}}", token.name));
        }
        (1..=7, FIELD_RESET_TEXT, Input::Index(index)) => {
            if index == 0 {
                return false;
            }
            if let Some(module) = settings.module_mut(MODULES[page as usize - 1]) {
                module.bar_template = None;
            }
        }
        (1..=7, _, input) => {
            let Some(module) = settings.module_mut(MODULES[page as usize - 1]) else {
                return false;
            };
            match (field, input) {
                (FIELD_SHOWN, Input::Bool(on)) => module.shown = on,
                (FIELD_PEAKS, Input::Bool(on)) => module.show_peaks = on,
                (FIELD_BAR, Input::Index(index)) => {
                    module.bar_style = [BarStyle::Text, BarStyle::Graph, BarStyle::Both][index.min(2)];
                }
                (FIELD_STYLE, Input::Index(index)) => {
                    module.graph_style = [GraphStyle::Line, GraphStyle::Area, GraphStyle::Bars][index.min(2)];
                }
                (FIELD_SCALE, Input::Index(index)) => {
                    module.graph_scale = if index == 1 { GraphScale::Log } else { GraphScale::Linear };
                }
                (FIELD_RANGE, Input::Index(index)) => module.range_index = index.min(RANGES_SECONDS.len() - 1),
                (FIELD_COLOR, Input::Color(color)) => module.color = color,
                (FIELD_COLOR2, Input::Color(color)) => module.secondary_color = color,
                (FIELD_TEMPLATE, Input::Text(text)) => module.bar_template = Some(text),
                _ => return false,
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_has_controls() {
        let settings = Settings::default();
        for page in 0..PAGES.len() as i64 {
            assert!(!rows(page, &settings).is_empty(), "{}", PAGES[page as usize]);
        }
    }

    #[test]
    fn every_row_can_be_applied() {
        for page in 0..PAGES.len() as i64 {
            for row in rows(page, &Settings::default()) {
                let mut settings = Settings::default();
                if page_opened_by(row.tag).is_some() || row.tag == CLEAR_NETWORK_TAG || row.tag == UPDATE_GEO_TAG {
                    continue;
                }
                let (tag, input) = match row.control {
                    Control::Heading | Control::Note(_) | Control::Curve(_) => continue,
                    Control::Checkbox(on) => (row.tag, Input::Bool(!on)),
                    Control::Segmented(_, index) => (row.tag, Input::Index(index)),
                    Control::Colors(color, _) => (row.tag, Input::Color(color)),
                    Control::Text(text, _) => (row.tag, Input::Text(text)),
                    Control::Slider { value, .. } => (row.tag, Input::Number(value)),
                    Control::Menu(_) | Control::Button(_) => (row.tag, Input::Index(1)),
                    Control::Icons => (icon_tag(page, 0), Input::Index(1)),
                };
                assert!(apply(&mut settings, tag, input), "{}", row.label);
            }
        }
    }

    #[test]
    fn segmented_options_fit_their_saved_choice() {
        let settings = Settings::default();
        for page in 0..PAGES.len() as i64 {
            for row in rows(page, &settings) {
                if let Control::Segmented(options, selected) = row.control {
                    assert!(selected < options.len(), "{}", row.label);
                }
            }
        }
    }

    #[test]
    fn a_checkbox_changes_the_right_module() {
        let mut settings = Settings::default();
        let row = rows(3, &settings).into_iter().find(|r| r.label == "Show this item").unwrap();
        apply(&mut settings, row.tag, Input::Bool(false));
        assert!(!settings.cpu.shown);
        assert!(settings.network.shown);
    }

    #[test]
    fn adding_a_value_and_icon_extends_the_text() {
        let mut settings = Settings::default();
        assert!(apply(&mut settings, icon_tag(1, 4), Input::Index(1)));
        assert_eq!(settings.network.bar_template.as_deref(), Some("↑{up} ↓{down}▲"));
        assert!(apply(&mut settings, 100 + FIELD_ADD_VALUE, Input::Index(5)));
        assert_eq!(settings.network.bar_template.as_deref(), Some("↑{up} ↓{down}▲{ping}"));
        assert!(needs_page_refresh(100 + FIELD_ADD_VALUE));
        assert!(needs_page_refresh(icon_tag(1, 0)));
        assert!(!needs_page_refresh(100 + FIELD_TEMPLATE));
    }

    #[test]
    fn a_menu_title_click_adds_nothing() {
        let mut settings = Settings::default();
        assert!(!apply(&mut settings, 100 + FIELD_ADD_VALUE, Input::Index(0)));
        assert_eq!(settings.network.bar_template, None);
    }

    #[test]
    fn reset_returns_to_the_original_text() {
        let mut settings = Settings::default();
        apply(&mut settings, 100 + FIELD_TEMPLATE, Input::Text("x".into()));
        assert_eq!(view::template_for("network", &settings), "x");
        apply(&mut settings, 100 + FIELD_RESET_TEXT, Input::Index(1));
        assert_eq!(view::template_for("network", &settings), "↑{up} ↓{down}");
    }

    #[test]
    fn icons_reach_the_right_monitor_and_bad_tags_do_nothing() {
        let mut settings = Settings::default();
        assert!(apply(&mut settings, icon_tag(2, 0), Input::Index(1)));
        assert_eq!(settings.memory.bar_template.as_deref(), Some("RAM {percent}↑"));
        assert!(!apply(&mut settings, icon_tag(0, 0), Input::Index(1)));
        assert!(!apply(&mut settings, icon_tag(1, 99), Input::Index(1)));
    }

    #[test]
    fn buttons_and_checkboxes_report_differently() {
        assert_eq!(input_for_button(100 + FIELD_SHOWN, true), Input::Bool(true));
        assert_eq!(input_for_button(100 + FIELD_RESET_TEXT, false), Input::Index(1));
        assert_eq!(input_for_button(icon_tag(1, 2), false), Input::Index(1));
    }

    #[test]
    fn the_slider_only_changes_a_fixed_active_preset() {
        let mut settings = Settings { active_preset: "Cool".into(), ..Settings::default() };
        let before = settings.presets.clone();
        set_active_fixed_percent(&mut settings, 10.0);
        assert_eq!(settings.presets, before);
    }

    #[test]
    fn presets_can_be_added_renamed_retyped_and_deleted() {
        let mut settings = Settings::default();
        let start = settings.presets.len();
        assert!(apply(&mut settings, PRESETS * 100 + FIELD_ADD_PRESET, Input::Index(1)));
        assert_eq!(settings.presets.len(), start + 1);
        assert!(apply(&mut settings, preset_tag(start, PRESET_NAME), Input::Text("Night".into())));
        assert_eq!(settings.presets[start].name, "Night");
        assert!(apply(&mut settings, preset_tag(start, PRESET_SPEED), Input::Number(30.0)));
        assert_eq!(settings.presets[start].mode, FanMode::Fixed { percent: 30.0 });
        assert!(apply(&mut settings, preset_tag(start, PRESET_KIND), Input::Index(1)));
        assert!(matches!(settings.presets[start].mode, FanMode::Curve { .. }));
        assert!(apply(&mut settings, preset_tag(start, PRESET_DELETE), Input::Index(1)));
        assert_eq!(settings.presets.len(), start);
    }

    #[test]
    fn auto_cannot_be_edited() {
        let mut settings = Settings::default();
        assert!(!apply(&mut settings, preset_tag(0, PRESET_NAME), Input::Text("x".into())));
        assert!(!apply(&mut settings, preset_tag(0, PRESET_DELETE), Input::Index(1)));
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn curves_save_to_the_right_preset() {
        let mut settings = Settings::default();
        let points = vec![CurvePoint { temp_c: 50.0, percent: 50.0 }];
        set_preset_curve(&mut settings, 2, points.clone());
        assert_eq!(settings.presets[2].mode, FanMode::Curve { points });
        assert_eq!(preset_index(preset_tag(2, PRESET_CURVE)), Some(2));
    }

    #[test]
    fn every_preset_shows_on_the_page_with_its_controls() {
        let form = rows(PRESETS, &Settings::default());
        assert!(form.iter().any(|r| r.label == "Cool"));
        assert!(form.iter().any(|r| matches!(r.control, Control::Curve(_))));
        assert!(form.iter().any(|r| r.control == Control::Button("Add preset")));
    }

    #[test]
    fn spacing_is_clamped() {
        let mut settings = Settings::default();
        apply(&mut settings, FIELD_SPACING, Input::Number(500.0));
        assert_eq!(settings.item_spacing, MAX_ITEM_SPACING);
    }

    #[test]
    fn unknown_tags_and_wrong_types_change_nothing() {
        let mut settings = Settings::default();
        assert!(!apply(&mut settings, 9999, Input::Bool(true)));
        assert!(!apply(&mut settings, 100, Input::Text("x".into())));
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn out_of_range_choices_are_clamped() {
        let mut settings = Settings::default();
        apply(&mut settings, 100 + FIELD_RANGE, Input::Index(99));
        assert_eq!(settings.network.range_index, RANGES_SECONDS.len() - 1);
    }

    #[test]
    fn ping_host_is_trimmed_and_fixed_speed_is_clamped() {
        let mut settings = Settings::default();
        apply(&mut settings, 100 + FIELD_PING, Input::Text(" 1.1.1.1 ".into()));
        assert_eq!(settings.ping_host, "1.1.1.1");
        let settings_fixed = |s: &Settings| s.presets.iter().find_map(|p| match (&p.name[..], &p.mode) {
            ("Fixed", FanMode::Fixed { percent }) => Some(*percent),
            _ => None,
        });
        settings.active_preset = "Fixed".into();
        set_active_fixed_percent(&mut settings, 140.0);
        assert_eq!(settings_fixed(&settings), Some(100.0));
    }
}
