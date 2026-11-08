//! User settings, stored as JSON. Missing or invalid values fall back to defaults, so an old or
//! hand-edited file can never stop the app starting.

use crate::curve::CurvePoint;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MODULES: [&str; 7] = ["network", "memory", "cpu", "gpu", "drives", "fans", "battery"];
/// `ALL_HISTORY` means from the first saved reading until now, so that graph keeps growing.
pub const ALL_HISTORY: i64 = 0;
pub const RANGES_SECONDS: [i64; 8] = [600, 3600, 21600, 86400, 604800, 2592000, 31536000, ALL_HISTORY];
pub const RANGE_LABELS: [&str; 8] = ["10m", "1h", "6h", "1d", "7d", "30d", "1y", "All"];
const DEFAULT_RANGE_INDEX: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarStyle {
    Text,
    Graph,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphStyle {
    Line,
    Area,
    Bars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphScale {
    Linear,
    Log,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModuleSettings {
    pub shown: bool,
    pub bar_style: BarStyle,
    pub graph_style: GraphStyle,
    pub graph_scale: GraphScale,
    pub show_peaks: bool,
    pub range_index: usize,
    pub color: String,
    pub secondary_color: String,
    /// What the menu bar item says; `None` uses the built-in text for that monitor.
    pub bar_template: Option<String>,
}

impl Default for ModuleSettings {
    fn default() -> Self {
        ModuleSettings {
            shown: true,
            bar_style: BarStyle::Text,
            graph_style: GraphStyle::Area,
            graph_scale: GraphScale::Linear,
            show_peaks: false,
            range_index: DEFAULT_RANGE_INDEX,
            color: "#0A84FF".into(),
            secondary_color: "#FF9F0A".into(),
            bar_template: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum FanMode {
    Auto,
    Fixed { percent: f32 },
    Curve { points: Vec<CurvePoint> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanPreset {
    pub name: String,
    #[serde(flatten)]
    pub mode: FanMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemperatureUnit {
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alerts {
    pub disk_low_enabled: bool,
    pub disk_low_percent: f32,
    pub hot_enabled: bool,
    pub hot_c: f32,
    pub memory_pressure_enabled: bool,
    pub connection_down_enabled: bool,
}

impl Default for Alerts {
    fn default() -> Self {
        Alerts {
            disk_low_enabled: true,
            disk_low_percent: 10.0,
            hot_enabled: false,
            hot_c: 98.0,
            memory_pressure_enabled: true,
            connection_down_enabled: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub network: ModuleSettings,
    pub memory: ModuleSettings,
    pub cpu: ModuleSettings,
    pub gpu: ModuleSettings,
    pub drives: ModuleSettings,
    pub fans: ModuleSettings,
    pub battery: ModuleSettings,
    pub temperature_unit: TemperatureUnit,
    pub show_top_apps: bool,
    pub show_wifi: bool,
    /// macOS treats the network name as location data, so this asks for Location permission.
    pub show_wifi_name: bool,
    pub launch_at_login: bool,
    pub alerts: Alerts,
    /// The charge limit to keep macOS at. `None` leaves macOS's own setting alone; 100 is no limit.
    pub charge_limit: Option<u8>,
    /// Charging to full once, ignoring the limit until the battery is full.
    pub top_up: bool,
    /// Menu bar items, leftmost first.
    pub order: Vec<String>,
    /// Extra points of space around each menu bar item.
    pub item_spacing: f64,
    /// Empty means ping the gateway only, so nothing leaves the local network by default.
    pub ping_host: String,
    /// Names remote addresses by asking your DNS server, so it's off until turned on.
    pub resolve_names: bool,
    /// Shows which country each connection goes to. Needs a file downloaded from db-ip.com.
    pub show_countries: bool,
    /// Where the path check pings and looks up. Empty checks only this network.
    pub check_host: String,
    pub presets: Vec<FanPreset>,
    pub active_preset: String,
}

impl Default for Settings {
    fn default() -> Self {
        let hidden = ModuleSettings { shown: false, ..ModuleSettings::default() };
        Settings {
            network: ModuleSettings { graph_scale: GraphScale::Log, ..ModuleSettings::default() },
            memory: ModuleSettings { color: "#BF5AF2".into(), secondary_color: "#FF453A".into(), ..ModuleSettings::default() },
            cpu: ModuleSettings { color: "#30D158".into(), ..ModuleSettings::default() },
            gpu: ModuleSettings { color: "#64D2FF".into(), ..hidden },
            drives: ModuleSettings { color: "#FFD60A".into(), secondary_color: "#FF375F".into(), graph_scale: GraphScale::Log, ..ModuleSettings::default() },
            fans: ModuleSettings { color: "#FF9F0A".into(), secondary_color: "#FF453A".into(), ..ModuleSettings::default() },
            battery: ModuleSettings { color: "#30D158".into(), secondary_color: "#FFD60A".into(), ..ModuleSettings::default() },
            temperature_unit: TemperatureUnit::Celsius,
            show_top_apps: true,
            show_wifi: true,
            show_wifi_name: false,
            launch_at_login: false,
            alerts: Alerts::default(),
            charge_limit: None,
            top_up: false,
            order: MODULES.iter().map(|m| m.to_string()).collect(),
            item_spacing: DEFAULT_ITEM_SPACING,
            ping_host: String::new(),
            resolve_names: false,
            show_countries: false,
            check_host: String::new(),
            presets: vec![
                FanPreset { name: "Auto".into(), mode: FanMode::Auto },
                FanPreset {
                    name: "Cool".into(),
                    mode: FanMode::Curve {
                        points: vec![
                            CurvePoint { temp_c: 45.0, percent: 10.0 },
                            CurvePoint { temp_c: 65.0, percent: 45.0 },
                            CurvePoint { temp_c: 80.0, percent: 80.0 },
                            CurvePoint { temp_c: 90.0, percent: 100.0 },
                        ],
                    },
                },
                FanPreset { name: "Fixed".into(), mode: FanMode::Fixed { percent: 50.0 } },
                FanPreset { name: "Max".into(), mode: FanMode::Fixed { percent: 100.0 } },
            ],
            active_preset: "Auto".into(),
        }
    }
}

pub const MAX_PRESETS: usize = 8;
pub const AUTO_PRESET: &str = "Auto";
pub const MAX_ITEM_SPACING: f64 = 40.0;
const DEFAULT_ITEM_SPACING: f64 = 20.0;

/// Every monitor exactly once: keeps the saved order, drops unknowns and duplicates, and adds
/// any monitor a hand-edited or older file is missing.
pub fn normalized_order(saved: &[String]) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    for name in saved {
        if MODULES.contains(&name.as_str()) && !order.contains(name) {
            order.push(name.clone());
        }
    }
    for name in MODULES {
        if !order.iter().any(|m| m == name) {
            order.push(name.to_string());
        }
    }
    order
}

pub fn parse_color(hex: &str) -> Option<(f64, f64, f64)> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let channel = |at: usize| {
        u8::from_str_radix(&digits[at..at + 2], 16).ok().map(|v| f64::from(v) / 255.0)
    };
    Some((channel(0)?, channel(2)?, channel(4)?))
}

impl Settings {
    pub fn module(&self, name: &str) -> Option<&ModuleSettings> {
        Some(match name {
            "network" => &self.network,
            "memory" => &self.memory,
            "cpu" => &self.cpu,
            "gpu" => &self.gpu,
            "drives" => &self.drives,
            "fans" => &self.fans,
            "battery" => &self.battery,
            _ => return None,
        })
    }

    pub fn module_mut(&mut self, name: &str) -> Option<&mut ModuleSettings> {
        Some(match name {
            "network" => &mut self.network,
            "memory" => &mut self.memory,
            "cpu" => &mut self.cpu,
            "gpu" => &mut self.gpu,
            "drives" => &mut self.drives,
            "fans" => &mut self.fans,
            "battery" => &mut self.battery,
            _ => return None,
        })
    }

    /// Keeps presets usable after hand-editing: Auto always comes first, and names are present
    /// and unique. It's what fans fall back to, so it can't be lost.
    pub fn normalize_presets(&mut self) {
        let mut seen: Vec<String> = vec![AUTO_PRESET.to_string()];
        let mut kept = vec![FanPreset { name: AUTO_PRESET.to_string(), mode: FanMode::Auto }];
        for preset in self.presets.drain(..) {
            let name = preset.name.trim().to_string();
            if name.is_empty() || seen.contains(&name) || kept.len() >= MAX_PRESETS {
                continue;
            }
            seen.push(name.clone());
            kept.push(FanPreset { name, mode: preset.mode });
        }
        self.presets = kept;
        if !self.presets.iter().any(|p| p.name == self.active_preset) {
            AUTO_PRESET.clone_into(&mut self.active_preset);
        }
    }

    /// Adds a preset with a free name. `None` when the list is full.
    pub fn add_preset(&mut self) -> Option<usize> {
        if self.presets.len() >= MAX_PRESETS {
            return None;
        }
        let name = (1..)
            .map(|n| format!("Preset {n}"))
            .find(|name| !self.presets.iter().any(|p| p.name == *name))?;
        self.presets.push(FanPreset { name, mode: FanMode::Fixed { percent: 50.0 } });
        Some(self.presets.len() - 1)
    }

    /// Auto can't be removed. Deleting the active preset returns the fans to Auto.
    pub fn delete_preset(&mut self, index: usize) -> bool {
        if index == 0 || index >= self.presets.len() {
            return false;
        }
        let removed = self.presets.remove(index);
        if removed.name == self.active_preset {
            AUTO_PRESET.clone_into(&mut self.active_preset);
        }
        true
    }

    /// Refuses empty and duplicate names, and any change to Auto.
    pub fn rename_preset(&mut self, index: usize, name: &str) -> bool {
        let name = name.trim();
        if index == 0
            || index >= self.presets.len()
            || name.is_empty()
            || self.presets.iter().enumerate().any(|(i, p)| i != index && p.name == name)
        {
            return false;
        }
        if self.presets[index].name == self.active_preset {
            name.clone_into(&mut self.active_preset);
        }
        name.clone_into(&mut self.presets[index].name);
        true
    }

    /// Each monitor's main color, in `MODULES` order.
    pub fn module_colors(&self) -> Vec<&str> {
        MODULES.iter().filter_map(|name| self.module(name).map(|m| m.color.as_str())).collect()
    }

    pub fn preset(&self) -> Option<&FanPreset> {
        self.presets.iter().find(|p| p.name == self.active_preset)
    }

    /// Fans start on Auto every launch unless a preset is chosen again, so a bad curve can't
    /// come back by itself after a restart.
    pub fn load(path: &Path) -> Settings {
        let mut settings: Settings = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        "Auto".clone_into(&mut settings.active_preset);
        settings.order = normalized_order(&settings.order);
        settings.item_spacing = settings.item_spacing.clamp(0.0, MAX_ITEM_SPACING);
        settings.normalize_presets();
        settings.charge_limit = settings.charge_limit.map(|limit| limit.clamp(crate::battery::MIN_CHARGE_LIMIT, 100));
        for name in MODULES {
            if let Some(module) = settings.module_mut(name) {
                module.range_index = module.range_index.min(RANGES_SECONDS.len() - 1);
            }
        }
        settings
    }

    /// Writes through a temporary file so a crash can't leave half a settings file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            make_private_dir(parent)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, text)?;
        std::fs::rename(temporary, path)
    }
}

/// Creates the folder if needed and makes it readable only by you, so other accounts on the Mac
/// can't read your history, settings or connection records inside it.
pub fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub fn support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join("Library/Application Support/Gauge")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gauge-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn round_trips_through_a_file() {
        let path = temp_file("round");
        let mut settings = Settings::default();
        settings.network.color = "#112233".into();
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).network.color, "#112233");
    }

    #[test]
    fn missing_or_corrupt_files_give_defaults() {
        assert_eq!(Settings::load(Path::new("/nonexistent/x.json")), Settings::default());
        let path = temp_file("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn missing_fields_keep_defaults() {
        let path = temp_file("partial");
        std::fs::write(&path, r#"{"show_wifi": false, "cpu": {"shown": false}}"#).unwrap();
        let settings = Settings::load(&path);
        assert!(!settings.show_wifi);
        assert!(!settings.cpu.shown);
        assert!(settings.network.shown);
    }

    #[test]
    fn fans_always_start_on_auto() {
        let path = temp_file("preset");
        let settings = Settings { active_preset: "Max".into(), ..Settings::default() };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).active_preset, "Auto");
    }

    #[test]
    fn bad_range_index_is_clamped() {
        let path = temp_file("range");
        std::fs::write(&path, r#"{"cpu": {"range_index": 99}}"#).unwrap();
        assert_eq!(Settings::load(&path).cpu.range_index, RANGES_SECONDS.len() - 1);
    }

    #[test]
    fn presets_serialize_with_their_mode() {
        let settings = Settings::default();
        let text = serde_json::to_string(&settings.presets).unwrap();
        let back: Vec<FanPreset> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, settings.presets);
    }

    #[test]
    fn graphs_default_to_one_hour() {
        assert_eq!(RANGES_SECONDS[Settings::default().network.range_index], 3600);
        assert_eq!(RANGE_LABELS[ModuleSettings::default().range_index], "1h");
    }

    #[test]
    fn order_keeps_every_monitor_exactly_once() {
        let saved: Vec<String> = ["fans", "bogus", "fans", "cpu"].map(String::from).to_vec();
        let order = normalized_order(&saved);
        assert_eq!(order[..2], ["fans", "cpu"]);
        assert_eq!(order.len(), MODULES.len());
        for module in MODULES {
            assert_eq!(order.iter().filter(|m| *m == module).count(), 1);
        }
    }

    #[test]
    fn auto_always_exists_first_and_names_stay_unique() {
        let mut settings = Settings {
            presets: vec![
                FanPreset { name: "Quiet".into(), mode: FanMode::Fixed { percent: 10.0 } },
                FanPreset { name: "Quiet".into(), mode: FanMode::Fixed { percent: 20.0 } },
                FanPreset { name: " ".into(), mode: FanMode::Auto },
                FanPreset { name: "Auto".into(), mode: FanMode::Fixed { percent: 0.0 } },
            ],
            ..Settings::default()
        };
        settings.normalize_presets();
        settings.charge_limit = settings.charge_limit.map(|limit| limit.clamp(crate::battery::MIN_CHARGE_LIMIT, 100));
        let names: Vec<&str> = settings.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Auto", "Quiet"]);
        assert_eq!(settings.presets[0].mode, FanMode::Auto);
    }

    #[test]
    fn presets_can_be_added_up_to_the_limit_with_free_names() {
        let mut settings = Settings::default();
        let start = settings.presets.len();
        assert_eq!(settings.add_preset(), Some(start));
        assert_eq!(settings.presets[start].name, "Preset 1");
        while settings.presets.len() < MAX_PRESETS {
            assert!(settings.add_preset().is_some());
        }
        assert_eq!(settings.add_preset(), None);
    }

    #[test]
    fn deleting_keeps_auto_and_moves_the_active_preset_to_auto() {
        let mut settings = Settings { active_preset: "Max".into(), ..Settings::default() };
        assert!(!settings.delete_preset(0));
        let max = settings.presets.iter().position(|p| p.name == "Max").unwrap();
        assert!(settings.delete_preset(max));
        assert_eq!(settings.active_preset, "Auto");
        assert!(!settings.delete_preset(99));
    }

    #[test]
    fn renaming_refuses_bad_names_and_follows_the_active_preset() {
        let mut settings = Settings { active_preset: "Cool".into(), ..Settings::default() };
        let cool = settings.presets.iter().position(|p| p.name == "Cool").unwrap();
        assert!(!settings.rename_preset(0, "Other"));
        assert!(!settings.rename_preset(cool, "  "));
        assert!(!settings.rename_preset(cool, "Max"));
        assert!(settings.rename_preset(cool, " Night "));
        assert_eq!(settings.presets[cool].name, "Night");
        assert_eq!(settings.active_preset, "Night");
    }

    #[test]
    fn spacing_defaults_to_twenty_points() {
        assert_eq!(Settings::default().item_spacing, 20.0);
    }

    #[test]
    fn spacing_from_a_file_is_clamped() {
        let path = temp_file("spacing");
        std::fs::write(&path, r#"{"item_spacing": 900}"#).unwrap();
        assert_eq!(Settings::load(&path).item_spacing, MAX_ITEM_SPACING);
    }

    #[test]
    fn parses_colors_and_rejects_junk() {
        assert_eq!(parse_color("#FF0000"), Some((1.0, 0.0, 0.0)));
        for bad in ["FF0000", "#FFF", "#GG0000", "#ééé"] {
            assert_eq!(parse_color(bad), None, "{bad}");
        }
    }
}
