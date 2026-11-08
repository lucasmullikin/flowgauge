//! What the Network panel and window show: the flow from your connection to apps to
//! destinations, the path check, and the app timeline. Pure, so it's testable.

use crate::view::{Latest, format_bytes, format_rate};
use gauge_core::history::AppHour;
use gauge_core::names::short_domain;
use gauge_core::pathcheck::{Health, Step};
use gauge_core::traffic::is_local;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};

/// How many apps and destinations a flow shows before grouping the rest as "Other".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub apps: usize,
    pub destinations: usize,
}

pub const PANEL: Limits = Limits { apps: 4, destinations: 3 };
pub const WINDOW: Limits = Limits { apps: 8, destinations: 5 };
pub const APP_SORTS: [&str; 4] = ["Total", "Down", "Up", "In Other"];
/// Longer names are cut so they don't run into their numbers in the narrow app list.
const LIST_NAME_CHARS: usize = 15;
pub const TIMELINE_APPS: usize = 6;
pub const OTHER: &str = "Other";
pub const LOCAL: &str = "Local network";
const WEAK_WIFI_DBM: i64 = -75;
const ROUTER_SLOW_MS: f32 = 50.0;
const DNS_SLOW_MS: f32 = 150.0;
const INTERNET_SLOW_MS: f32 = 200.0;
/// Rates are averaged over about this many seconds, so numbers settle enough to read.
const SMOOTH_SECONDS: f64 = 6.0;
/// How long the peak used for ordering takes to fade, so lists don't reshuffle every update.
const PEAK_SECONDS: f64 = 30.0;
/// How long a quiet connection stays listed.
pub const HOLD_SECONDS: f64 = 60.0;
/// Below this an app counts as quiet, and leaves the flow.
const QUIET_RATE: f64 = 1.0;
/// A connection idle for at least this long says how long.
const SHOW_IDLE_SECONDS: f64 = 3.0;

/// One destination of one app, averaged over the last few seconds, in bytes per second.
#[derive(Debug, Clone, PartialEq)]
pub struct DestinationNow {
    pub address: IpAddr,
    pub name: Option<String>,
    /// The country's name, when locations are on.
    pub country: Option<String>,
    pub down: f64,
    pub up: f64,
    pub rtt_ms: Option<f32>,
    /// Seconds since it last moved any data.
    pub idle_seconds: f64,
    /// Its recent busiest rate, fading slowly, for a steady order.
    pub peak: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppNow {
    pub name: String,
    pub down: f64,
    pub up: f64,
    pub destinations: Vec<DestinationNow>,
}

/// Live traffic per app, busiest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrafficNow {
    pub apps: Vec<AppNow>,
}

/// A DNS server macOS is using, who runs it, and the interface that set it.
#[derive(Debug, Clone, PartialEq)]
pub struct DnsServer {
    pub address: String,
    pub provider: String,
    pub interface: Option<String>,
}

/// The router, DNS and internet checks, and a count that goes up with every check.
#[derive(Debug, Clone, PartialEq)]
pub struct PathNow {
    pub router: Step,
    pub dns: Step,
    pub internet: Step,
    pub checks: u64,
    pub gateway: Option<Ipv4Addr>,
    /// In the order macOS tries them. The first answers ordinary lookups.
    pub dns_servers: Vec<DnsServer>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Smoothed {
    down: f64,
    up: f64,
    peak: f64,
    last_active: f64,
}

impl Smoothed {
    fn step(&mut self, down: f64, up: f64, seconds: f64, clock: f64) {
        let keep = (-seconds / SMOOTH_SECONDS).exp();
        self.down = self.down * keep + down * (1.0 - keep);
        self.up = self.up * keep + up * (1.0 - keep);
        self.peak = (self.peak * (-seconds / PEAK_SECONDS).exp()).max(down + up);
        if down + up > 0.0 {
            self.last_active = clock;
        }
    }

    fn expired(&self, clock: f64) -> bool {
        clock - self.last_active > HOLD_SECONDS && self.down + self.up < QUIET_RATE
    }
}

/// Smooths live traffic and holds quiet connections for a while, so they can be read.
#[derive(Debug, Default)]
pub struct Recent {
    clock: f64,
    apps: BTreeMap<String, Smoothed>,
    places: BTreeMap<(String, IpAddr), (Smoothed, Option<f32>)>,
}

impl Recent {
    /// `apps` and `places` are bytes per second over the last `seconds`.
    pub fn update(
        &mut self,
        apps: &BTreeMap<String, (f64, f64)>,
        places: &BTreeMap<(String, IpAddr), (f64, f64)>,
        rtt_ms: &BTreeMap<IpAddr, f32>,
        seconds: f64,
    ) {
        self.clock += seconds;
        let clock = self.clock;
        for app in apps.keys() {
            self.apps.entry(app.clone()).or_insert_with(|| Smoothed { last_active: clock, ..Smoothed::default() });
        }
        for (app, smoothed) in &mut self.apps {
            let (down, up) = apps.get(app).copied().unwrap_or_default();
            smoothed.step(down, up, seconds, clock);
        }
        for key in places.keys() {
            self.places.entry(key.clone()).or_insert_with(|| (Smoothed { last_active: clock, ..Smoothed::default() }, None));
        }
        for ((app, address), (smoothed, rtt)) in &mut self.places {
            let (down, up) = places.get(&(app.clone(), *address)).copied().unwrap_or_default();
            smoothed.step(down, up, seconds, clock);
            if let Some(latest) = rtt_ms.get(address) {
                *rtt = Some(*latest);
            }
        }
        self.apps.retain(|_, s| !s.expired(clock));
        self.places.retain(|_, (s, _)| !s.expired(clock));
    }

    pub fn clear(&mut self) {
        *self = Recent::default();
    }

    pub fn snapshot(&self, name_of: &dyn Fn(IpAddr) -> Option<String>) -> TrafficNow {
        let mut apps: Vec<AppNow> = self
            .apps
            .iter()
            .map(|(name, s)| AppNow {
                name: name.clone(),
                down: s.down,
                up: s.up,
                destinations: self
                    .places
                    .iter()
                    .filter(|((app, _), _)| app == name)
                    .map(|((_, address), (p, rtt))| DestinationNow {
                        address: *address,
                        name: if is_local(address) { None } else { name_of(*address) },
                        country: None,
                        down: p.down,
                        up: p.up,
                        rtt_ms: *rtt,
                        idle_seconds: self.clock - p.last_active,
                        peak: p.peak,
                    })
                    .collect(),
            })
            .collect();
        apps.sort_by(|a, b| (b.down + b.up).total_cmp(&(a.down + a.up)).then_with(|| a.name.cmp(&b.name)));
        TrafficNow { apps }
    }
}

/// Connections busiest first by their recent peak, which changes slowly, then by name.
pub fn recent_connections(traffic: &TrafficNow, limit: usize) -> Vec<(String, String, String)> {
    let mut all: Vec<(&str, &DestinationNow)> =
        traffic.apps.iter().flat_map(|a| a.destinations.iter().map(move |d| (a.name.as_str(), d))).collect();
    all.sort_by(|a, b| b.1.peak.total_cmp(&a.1.peak).then_with(|| a.1.address.cmp(&b.1.address)));
    all.into_iter()
        .take(limit)
        .map(|(app, d)| {
            let place = if is_local(&d.address) {
                format!("{} (local)", d.address)
            } else {
                let place = d.name.clone().unwrap_or_else(|| d.address.to_string());
                match &d.country {
                    Some(country) => format!("{place} ({country})"),
                    None => place,
                }
            };
            let rate = if d.idle_seconds >= SHOW_IDLE_SECONDS {
                format!("idle {:.0} s", d.idle_seconds)
            } else {
                format_rate(d.down + d.up)
            };
            let rtt = d.rtt_ms.map(|ms| format!(", {ms:.0} ms")).unwrap_or_default();
            (display_name(app), place, format!("{rate}{rtt}"))
        })
        .collect()
}

/// What answers ordinary lookups, and what it sits ahead of.
/// A country's name in the Mac's language, from its two-letter code.
pub fn country_name(code: &str) -> String {
    let locale = objc2_foundation::NSLocale::currentLocale();
    locale
        .localizedStringForCountryCode(&objc2_foundation::NSString::from_str(code))
        .map_or_else(|| code.to_string(), |name| name.to_string())
}

pub fn dns_line(path: &PathNow) -> Option<String> {
    let first = path.dns_servers.first()?;
    let via = match &first.interface {
        Some(i) if i.starts_with("utun") || i.starts_with("ipsec") => format!(", set by a VPN on {i}"),
        _ => String::new(),
    };
    let mut line = format!("{} ({}){via}", first.provider, first.address);
    if let Some(next) = path.dns_servers.iter().find(|s| s.provider != first.provider) {
        // "Your router" reads as "your router" mid-sentence; names like Cloudflare keep capitals.
        let provider = if next.provider.starts_with("Your ") || next.provider.starts_with("A ") {
            let mut letters = next.provider.chars();
            letters.next().map(|first| first.to_lowercase().collect::<String>() + letters.as_str()).unwrap_or_default()
        } else {
            next.provider.clone()
        };
        line.push_str(&format!(", ahead of {provider} ({})", next.address));
    }
    Some(line)
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowNode {
    pub label: String,
    pub down: f64,
    pub up: f64,
}

impl FlowNode {
    pub fn total(&self) -> f64 {
        self.down + self.up
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowLink {
    pub app: usize,
    pub destination: usize,
    pub down: f64,
    pub up: f64,
}

/// `live` flows are in bytes per second and move. Others are totals over a period.
#[derive(Debug, Clone, PartialEq)]
pub struct NetFlow {
    pub source: String,
    pub apps: Vec<FlowNode>,
    pub destinations: Vec<FlowNode>,
    pub links: Vec<FlowLink>,
    pub live: bool,
}

impl NetFlow {
    pub fn total(&self) -> f64 {
        self.apps.iter().map(FlowNode::total).sum()
    }

    pub fn format(&self, value: f64) -> String {
        if self.live { format_rate(value) } else { format_bytes(value) }
    }
}

/// An app's traffic, and how it splits across destination labels.
pub struct AppEntry {
    pub app: String,
    pub down: f64,
    pub up: f64,
    pub destinations: Vec<(String, f64, f64)>,
}

/// How a remote address is labeled: the local network, its country when locations are on,
/// a name's domain, or the address.
pub fn destination_label(address: &IpAddr, name: Option<&str>, country: Option<&str>) -> String {
    if is_local(address) {
        LOCAL.to_string()
    } else if let Some(country) = country {
        country.to_string()
    } else if let Some(name) = name {
        short_domain(name)
    } else {
        address.to_string()
    }
}

fn top_labels(totals: BTreeMap<String, f64>, keep: usize) -> Vec<String> {
    let mut sorted: Vec<(String, f64)> = totals.into_iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted.into_iter().take(keep).map(|(label, _)| label).collect()
}

/// Keeps the busiest apps and destinations, and puts the rest in "Other". Traffic an app sent
/// without a connection `nettop` could see also goes to "Other", so each app's paths add up.
pub fn build_flow(source: &str, entries: &[AppEntry], live: bool, limits: Limits) -> Option<NetFlow> {
    let mut app_totals: BTreeMap<String, f64> = BTreeMap::new();
    let mut destination_totals: BTreeMap<String, f64> = BTreeMap::new();
    for entry in entries {
        *app_totals.entry(entry.app.clone()).or_default() += entry.down + entry.up;
        for (label, down, up) in &entry.destinations {
            *destination_totals.entry(label.clone()).or_default() += down + up;
        }
    }
    let crowded_apps = app_totals.len() > limits.apps;
    let kept_apps = top_labels(app_totals, if crowded_apps { limits.apps - 1 } else { limits.apps });
    destination_totals.remove(OTHER);
    let crowded = destination_totals.len() > limits.destinations;
    let kept_destinations =
        top_labels(destination_totals, if crowded { limits.destinations - 1 } else { limits.destinations });

    let mut apps: Vec<FlowNode> = kept_apps.iter().map(|l| FlowNode { label: l.clone(), down: 0.0, up: 0.0 }).collect();
    let mut destinations: Vec<FlowNode> =
        kept_destinations.iter().map(|l| FlowNode { label: l.clone(), down: 0.0, up: 0.0 }).collect();
    let mut links: BTreeMap<(usize, usize), (f64, f64)> = BTreeMap::new();
    let index_or_other = |nodes: &mut Vec<FlowNode>, label: &str, kept: &[String]| -> usize {
        let wanted = if kept.iter().any(|k| k == label) { label } else { OTHER };
        match nodes.iter().position(|n| n.label == wanted) {
            Some(i) => i,
            None => {
                nodes.push(FlowNode { label: OTHER.into(), down: 0.0, up: 0.0 });
                nodes.len() - 1
            }
        }
    };
    for entry in entries {
        let app = index_or_other(&mut apps, &entry.app, &kept_apps);
        apps[app].down += entry.down;
        apps[app].up += entry.up;
        let (mut down_left, mut up_left) = (entry.down, entry.up);
        for (label, down, up) in &entry.destinations {
            let destination = index_or_other(&mut destinations, label, &kept_destinations);
            let link = links.entry((app, destination)).or_default();
            let (down, up) = (down.min(down_left), up.min(up_left));
            link.0 += down;
            link.1 += up;
            down_left -= down;
            up_left -= up;
        }
        if down_left + up_left > 0.0 {
            let destination = index_or_other(&mut destinations, OTHER, &kept_destinations);
            let link = links.entry((app, destination)).or_default();
            link.0 += down_left;
            link.1 += up_left;
        }
    }
    for ((_, destination), (down, up)) in &links {
        destinations[*destination].down += down;
        destinations[*destination].up += up;
    }
    let links: Vec<FlowLink> = links
        .into_iter()
        .filter(|(_, (down, up))| down + up > 0.0)
        .map(|((app, destination), (down, up))| FlowLink { app, destination, down, up })
        .collect();
    let flow = NetFlow { source: source.to_string(), apps, destinations, links, live };
    (flow.total() > 0.0).then_some(flow)
}

/// The default interface, named the way people know it.
pub fn source_label(latest: &Latest) -> String {
    let default = latest.interfaces.iter().find(|i| i.is_default);
    let wifi_interface = latest.wifi.as_ref().and_then(|w| w.interface.clone());
    match default {
        Some(i) if wifi_interface.as_deref() == Some(i.name.as_str()) => "Wi-Fi".into(),
        Some(i) if i.name.starts_with("utun") || i.name.starts_with("ipsec") => "VPN".into(),
        Some(i) if i.name.starts_with("en") => "Ethernet".into(),
        Some(i) => i.name.clone(),
        None => "Offline".into(),
    }
}

pub fn live_flow(latest: &Latest, limits: Limits) -> Option<NetFlow> {
    let traffic = latest.traffic.as_ref()?;
    let entries: Vec<AppEntry> = traffic
        .apps
        .iter()
        .filter(|app| app.down + app.up >= QUIET_RATE)
        .map(|app| AppEntry {
            app: display_name(&app.name),
            down: app.down,
            up: app.up,
            destinations: app
                .destinations
                .iter()
                .map(|d| (destination_label(&d.address, d.name.as_deref(), d.country.as_deref()), d.down, d.up))
                .collect(),
        })
        .collect();
    build_flow(&source_label(latest), &entries, true, limits)
}

/// A flow of totals since some time, from saved history.
pub fn history_flow(
    source: &str,
    apps: &[AppHour],
    hosts: &BTreeMap<(String, String), (u64, u64)>,
    name_of: &dyn Fn(IpAddr) -> Option<String>,
    country_of: &dyn Fn(IpAddr) -> Option<String>,
    limits: Limits,
) -> Option<NetFlow> {
    let mut totals: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for row in apps {
        let entry = totals.entry(row.app.clone()).or_default();
        entry.0 += row.bytes_in as f64;
        entry.1 += row.bytes_out as f64;
    }
    let entries: Vec<AppEntry> = totals
        .into_iter()
        .map(|(app, (down, up))| {
            let destinations = hosts
                .iter()
                .filter(|((a, _), _)| *a == app)
                .filter_map(|((_, host), (d, u))| {
                    let address: IpAddr = host.parse().ok()?;
                    let (name, country) = if is_local(&address) { (None, None) } else { (name_of(address), country_of(address)) };
                    Some((destination_label(&address, name.as_deref(), country.as_deref()), *d as f64, *u as f64))
                })
                .collect();
            AppEntry { app: display_name(&app), down, up, destinations }
        })
        .collect();
    build_flow(source, &entries, false, limits)
}

/// Plain names for macOS's own background services, which `nettop` lists by process name.
/// Apps keep their own names.
pub fn display_name(raw: &str) -> String {
    let friendly = match raw {
        "nsurlsessiond" => "Background downloads",
        "apsd" => "Push notifications",
        "cloudd" => "iCloud",
        "bird" => "iCloud Drive",
        "cloudphotod" | "photoanalysisd" => "iCloud Photos",
        "mDNSResponder" => "Bonjour and DNS",
        "trustd" => "Certificate checks",
        "softwareupdated" | "com.apple.Mobile" | "mobileassetd" => "macOS updates",
        "remoted" | "remotepairingd" => "Paired devices",
        "rapportd" => "Nearby Apple devices",
        "identityservice" | "identityservicesd" | "imagent" => "iMessage and FaceTime",
        "akd" | "accountsd" => "Apple Account",
        "assistantd" | "siriknowledged" => "Siri",
        "parsecd" | "corespotlightd" | "mds" => "Spotlight",
        "symptomsd" | "networkservicep" => "Network diagnostics",
        "configd" | "airportd" => "Network setup",
        "timed" => "Time sync",
        "sharingd" => "AirDrop and sharing",
        "netbiosd" | "smbd" => "File sharing",
        "storedownloadd" | "appstoreagent" | "appstorecompone" => "App Store",
        "nehelper" | "neagent" | "nesessionmanage" => "VPN",
        "kernel_task" | "launchd" => "macOS",
        "WebKit" => "Web content",
        "ControlCenter" => "Control Center",
        "locationd" => "Location Services",
        "adprivacyd" | "searchpartyd" => "Find My",
        _ => return raw.to_string(),
    };
    friendly.to_string()
}

/// Every app for a period, as rows of name and traffic. `sort` indexes `APP_SORTS`: by total,
/// by download, by upload, or only the apps grouped into the flow's "Other".
pub fn app_rows(apps: &[(String, f64, f64)], sort: usize, in_flow: &[String], live: bool) -> Vec<crate::view::Row> {
    let format = |value: f64| if live { format_rate(value) } else { format_bytes(value) };
    let mut merged: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (name, down, up) in apps {
        let entry = merged.entry(display_name(name)).or_default();
        entry.0 += down;
        entry.1 += up;
    }
    let mut rows: Vec<(String, f64, f64)> = merged
        .into_iter()
        .filter(|(_, (d, u))| d + u > 0.0)
        .filter(|(name, _)| sort != 3 || !in_flow.contains(name))
        .map(|(name, (d, u))| (name, d, u))
        .collect();
    let key = |row: &(String, f64, f64)| match sort {
        1 => row.1,
        2 => row.2,
        _ => row.1 + row.2,
    };
    rows.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.0.cmp(&b.0)));
    if rows.is_empty() {
        let empty = if sort == 3 { "Every app is shown in the flow" } else { "No traffic for this period yet" };
        return vec![crate::view::Row { key: empty.into(), value: String::new(), header: false }];
    }
    rows.into_iter()
        .map(|(name, d, u)| crate::view::Row {
            key: crate::view::shorten(&name, LIST_NAME_CHARS),
            value: format!("↓{}  ↑{}", format(d), format(u)),
            header: false,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathStepView {
    pub label: String,
    pub value: String,
    pub health: Health,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathView {
    pub steps: Vec<PathStepView>,
    pub message: String,
    pub loss: String,
    pub checks: u64,
}

fn step_view(label: &str, step: &Step) -> PathStepView {
    let value = match (step.health, step.latest_ms) {
        (Health::Off, _) => "Off".to_string(),
        (_, None) => "No reply".to_string(),
        (_, Some(ms)) if ms < 10.0 => format!("{ms:.1} ms"),
        (_, Some(ms)) => format!("{ms:.0} ms"),
    };
    PathStepView { label: label.to_string(), value, health: step.health }
}

pub fn path_view(latest: &Latest, has_check_host: bool) -> PathView {
    let source = source_label(latest);
    let link = match (&latest.wifi, source.as_str()) {
        (_, "Offline") => PathStepView { label: "Network".into(), value: "Offline".into(), health: Health::Down },
        (Some(wifi), "Wi-Fi") => PathStepView {
            label: "Wi-Fi".into(),
            value: format!("{} dBm", wifi.rssi_dbm),
            health: if wifi.rssi_dbm < WEAK_WIFI_DBM { Health::Slow } else { Health::Good },
        },
        _ => PathStepView { label: source.clone(), value: "Connected".into(), health: Health::Good },
    };
    let off = Step::off();
    let path = latest.path.as_ref();
    let mut router = step_view("Router", path.map_or(&off, |p| &p.router));
    let mut dns = step_view("DNS", path.map_or(&off, |p| &p.dns));
    let mut internet = step_view("Internet", path.map_or(&off, |p| &p.internet));
    if let Some(path) = path {
        for (view, step, slow) in [(&mut router, &path.router, ROUTER_SLOW_MS), (&mut dns, &path.dns, DNS_SLOW_MS), (&mut internet, &path.internet, INTERNET_SLOW_MS)] {
            if step.health == Health::Good && step.latest_ms.is_some_and(|ms| ms > slow) {
                view.health = Health::Slow;
            }
        }
    }
    let mac = PathStepView { label: "Mac".into(), value: String::new(), health: Health::Good };
    let steps = vec![mac, link, router, dns, internet];
    let problem = steps.iter().find(|s| matches!(s.health, Health::Down | Health::Slow));
    let message = match problem {
        Some(s) if s.health == Health::Down && s.label == "Network" => "Not connected".to_string(),
        Some(s) if s.health == Health::Down => format!("{} isn't answering", if s.label == "Router" { "The router" } else { &s.label }),
        Some(s) if s.label == "Wi-Fi" => "Wi-Fi signal is weak".to_string(),
        Some(s) => format!("{} is slower than usual", if s.label == "Router" { "The router" } else { &s.label }),
        None if !has_check_host => "Router and DNS are fine. Set a check address in Settings to test the internet".to_string(),
        None => "Every step is answering normally".to_string(),
    };
    let worst_loss = path.map_or(0.0, |p| [&p.router, &p.dns, &p.internet].iter().map(|s| s.loss_percent).fold(0.0, f32::max));
    PathView { steps, message, loss: format!("{worst_loss:.0}% of checks lost"), checks: path.map_or(0, |p| p.checks) }
}

/// Timeline lengths, and how much time each cell covers.
pub const TIMELINE_RANGES: [(&str, i64, i64); 3] = [("24 h", 86400, 3600), ("7 days", 7 * 86400, 4 * 3600), ("90 days", 90 * 86400, 86400)];

#[derive(Debug, Clone, PartialEq)]
pub struct Timeline {
    /// App name and bytes per cell, oldest first.
    pub rows: Vec<(String, Vec<f64>)>,
    pub busiest: Option<(String, f64)>,
    pub range_label: &'static str,
}

/// Where a timeline starts, how long each cell is, and how many there are. The last cell
/// holds now, and day cells start at local midnight.
pub fn timeline_window(now: i64, range_index: usize) -> (i64, i64, usize, &'static str) {
    let (label, range, cell) = TIMELINE_RANGES[range_index.min(TIMELINE_RANGES.len() - 1)];
    let end = if cell == 86400 {
        gauge_core::history::local_midnight(now) + 86400
    } else {
        now - now.rem_euclid(cell) + cell
    };
    (end - range, cell, (range / cell) as usize, label)
}

/// The busiest apps in each cell; the rest are summed into "Other". `cells` are
/// (cell index, app, bytes), as `History::app_cells` returns them.
pub fn timeline(cells: &[(i64, String, u64)], count: usize, range_label: &'static str) -> Timeline {
    let in_range = |index: i64| index >= 0 && (index as usize) < count;
    let mut totals: BTreeMap<String, f64> = BTreeMap::new();
    let cells: Vec<(i64, String, u64)> = cells.iter().map(|(i, app, b)| (*i, display_name(app), *b)).collect();
    let cells = cells.as_slice();
    for (index, app, bytes) in cells {
        if in_range(*index) {
            *totals.entry(app.clone()).or_default() += *bytes as f64;
        }
    }
    let busiest = totals.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(a, b)| (a.clone(), *b));
    let crowded = totals.len() > TIMELINE_APPS;
    let kept = top_labels(totals, if crowded { TIMELINE_APPS - 1 } else { TIMELINE_APPS });
    let mut grid: Vec<(String, Vec<f64>)> = kept.iter().map(|a| (a.clone(), vec![0.0; count])).collect();
    if crowded {
        grid.push((OTHER.into(), vec![0.0; count]));
    }
    for (index, app, bytes) in cells {
        if !in_range(*index) {
            continue;
        }
        let line = grid.iter().position(|(a, _)| a == app).unwrap_or(grid.len().saturating_sub(1));
        if let Some((_, values)) = grid.get_mut(line) {
            values[*index as usize] += *bytes as f64;
        }
    }
    Timeline { rows: grid, busiest, range_label }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(app: &str, down: f64, destinations: &[(&str, f64)]) -> AppEntry {
        AppEntry {
            app: app.into(),
            down,
            up: 0.0,
            destinations: destinations.iter().map(|(l, d)| (l.to_string(), *d, 0.0)).collect(),
        }
    }

    #[test]
    fn small_apps_and_destinations_go_to_other() {
        let entries = vec![
            entry("Safari", 900.0, &[("example.net", 600.0), ("example.org", 300.0)]),
            entry("Mail", 200.0, &[("example.com", 200.0)]),
            entry("Music", 100.0, &[("example.edu", 100.0)]),
            entry("Photos", 50.0, &[("example.info", 50.0)]),
            entry("Notes", 10.0, &[]),
        ];
        let flow = build_flow("Wi-Fi", &entries, true, PANEL).unwrap();
        let apps: Vec<&str> = flow.apps.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(apps, ["Safari", "Mail", "Music", "Other"]);
        assert_eq!(flow.apps[3].down, 60.0);
        let destinations: Vec<&str> = flow.destinations.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(destinations, ["example.net", "example.org", "Other"]);
        assert!((flow.total() - 1260.0).abs() < 0.01);
        let into_destinations: f64 = flow.destinations.iter().map(FlowNode::total).sum();
        assert!((into_destinations - flow.total()).abs() < 0.01, "every byte reaches a destination");
    }

    #[test]
    fn traffic_without_a_seen_connection_counts_as_other() {
        let flow = build_flow("Wi-Fi", &[entry("Music", 100.0, &[("example.net", 40.0)])], true, PANEL).unwrap();
        assert_eq!(flow.destinations.len(), 2);
        assert_eq!(flow.destinations[1].label, OTHER);
        assert_eq!(flow.destinations[1].down, 60.0);
    }

    #[test]
    fn nothing_moving_is_no_flow() {
        assert!(build_flow("Wi-Fi", &[entry("Idle", 0.0, &[])], true, PANEL).is_none());
    }

    #[test]
    fn destinations_are_local_named_or_numbered() {
        let local: IpAddr = "192.168.1.20".parse().unwrap();
        let remote: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(destination_label(&local, Some("nas.lan"), Some("Germany")), LOCAL);
        assert_eq!(destination_label(&remote, Some("edge-1.cdn.example.net"), None), "example.net");
        assert_eq!(destination_label(&remote, Some("edge-1.cdn.example.net"), Some("Germany")), "Germany");
        assert_eq!(destination_label(&remote, None, None), "203.0.113.9");
    }

    #[test]
    fn timeline_places_cells_and_keeps_the_busiest_apps() {
        let mut cells: Vec<(i64, String, u64)> = (0..8).map(|i| (23, format!("App {i}"), 100 * (i as u64 + 1))).collect();
        cells.push((20, "App 7".into(), 50));
        cells.push((-1, "Gone".into(), 1_000_000));
        let got = timeline(&cells, 24, "24 h");
        assert_eq!(got.rows.len(), TIMELINE_APPS);
        assert_eq!(got.rows[0].0, "App 7");
        assert_eq!(got.rows[0].1.len(), 24);
        assert_eq!(got.rows[0].1[23], 800.0);
        assert_eq!(got.rows[0].1[20], 50.0);
        assert_eq!(got.rows.last().unwrap().0, OTHER);
        assert_eq!(got.rows.last().unwrap().1[23], 600.0, "App 0, 1 and 2 are summed");
        assert_eq!(got.busiest, Some(("App 7".into(), 850.0)), "cells outside the range don't count");
    }

    #[test]
    fn timeline_windows_end_with_the_cell_holding_now() {
        let now = 1_800_000_000;
        let (start, cell, count, _) = timeline_window(now, 0);
        assert_eq!(count, 24);
        assert!(start + cell * 23 <= now && now < start + cell * 24);
        let (start, _, count, _) = timeline_window(now, 2);
        assert_eq!(count, 90);
        assert_eq!(start + 89 * 86400, gauge_core::history::local_midnight(now));
    }

    #[test]
    fn quiet_connections_stay_listed_for_a_while_then_go() {
        let address: IpAddr = "203.0.113.5".parse().unwrap();
        let key = ("Mail".to_string(), address);
        let mut recent = Recent::default();
        let apps = BTreeMap::from([("Mail".to_string(), (1000.0, 0.0))]);
        let places = BTreeMap::from([(key.clone(), (1000.0, 0.0))]);
        recent.update(&apps, &places, &BTreeMap::new(), 2.0);
        let first = recent.snapshot(&|_| None);
        assert!(first.apps[0].down > 0.0 && first.apps[0].down < 1000.0, "rates are smoothed");
        let quiet = BTreeMap::new();
        for _ in 0..20 {
            recent.update(&quiet, &BTreeMap::new(), &BTreeMap::new(), 2.0);
        }
        let held = recent.snapshot(&|_| None);
        let rows = recent_connections(&held, 5);
        assert_eq!(rows.len(), 1, "still listed after 40 quiet seconds");
        assert!(rows[0].2.starts_with("idle 40 s"), "{:?}", rows[0]);
        for _ in 0..20 {
            recent.update(&quiet, &BTreeMap::new(), &BTreeMap::new(), 2.0);
        }
        assert!(recent.snapshot(&|_| None).apps.is_empty());
    }

    #[test]
    fn dns_line_says_what_answers_and_what_it_overrides() {
        let server = |address: &str, provider: &str, interface: &str| DnsServer {
            address: address.into(),
            provider: provider.into(),
            interface: Some(interface.into()),
        };
        let off = Step::off();
        let path = PathNow {
            router: off.clone(),
            dns: off.clone(),
            internet: off,
            checks: 1,
            gateway: None,
            dns_servers: vec![
                server("100.100.100.100", "Tailscale", "utun2"),
                server("fd7a:115c:a1e0::53", "Tailscale", "utun2"),
                server("192.168.1.1", "Your router", "en0"),
            ],
        };
        assert_eq!(
            dns_line(&path).unwrap(),
            "Tailscale (100.100.100.100), set by a VPN on utun2, ahead of your router (192.168.1.1)"
        );
    }

    #[test]
    fn the_path_message_names_the_first_problem() {
        let mut latest = Latest::default();
        let view = path_view(&latest, false);
        assert_eq!(view.steps.len(), 5);
        assert_eq!(view.message, "Not connected");
        latest.interfaces.push(gauge_core::network::InterfaceRate {
            name: "en1".into(),
            up: true,
            is_default: true,
            addresses: vec![],
            bytes_in_total: 0,
            bytes_out_total: 0,
            in_per_sec: 0.0,
            out_per_sec: 0.0,
        });
        let good = Step { latest_ms: Some(3.0), usual_ms: Some(3.0), loss_percent: 0.0, health: Health::Good };
        let slow = Step { latest_ms: Some(400.0), usual_ms: Some(20.0), loss_percent: 0.0, health: Health::Slow };
        latest.path = Some(PathNow { router: good.clone(), dns: slow, internet: good, checks: 4, gateway: None, dns_servers: vec![] });
        let view = path_view(&latest, true);
        assert_eq!(view.steps[1].label, "Ethernet");
        assert_eq!(view.steps[3].value, "400 ms");
        assert_eq!(view.message, "DNS is slower than usual");
    }

    #[test]
    fn the_window_flow_keeps_more_apps_than_the_panel() {
        let entries: Vec<AppEntry> = (0..12).map(|i| entry(&format!("App {i}"), 100.0 + f64::from(i), &[])).collect();
        assert_eq!(build_flow("Wi-Fi", &entries, true, PANEL).unwrap().apps.len(), PANEL.apps);
        let window = build_flow("Wi-Fi", &entries, true, WINDOW).unwrap();
        assert_eq!(window.apps.len(), WINDOW.apps);
        assert_eq!(window.apps.last().unwrap().label, OTHER);
    }

    #[test]
    fn services_get_plain_names_and_merge() {
        assert_eq!(display_name("nsurlsessiond"), "Background downloads");
        assert_eq!(display_name("Safari"), "Safari");
        let rows = app_rows(
            &[("configd".into(), 10.0, 0.0), ("airportd".into(), 5.0, 0.0), ("Mail".into(), 1.0, 100.0)],
            0,
            &[],
            false,
        );
        assert_eq!(rows[0].key, "Mail");
        assert_eq!(rows[1].key, "Network setup", "two services with one plain name are added together");
        assert!(rows[1].value.starts_with("↓15 B"));
    }

    #[test]
    fn app_rows_sort_by_column_and_filter_to_other() {
        let apps = vec![("Big".to_string(), 900.0, 1.0), ("Sender".to_string(), 1.0, 500.0), ("Small".to_string(), 2.0, 2.0)];
        assert_eq!(app_rows(&apps, 2, &[], true)[0].key, "Sender", "sorting by upload puts senders first");
        assert_eq!(app_rows(&apps, 1, &[], true)[0].key, "Big");
        let other = app_rows(&apps, 3, &["Big".into(), "Sender".into()], true);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].key, "Small");
    }
}
