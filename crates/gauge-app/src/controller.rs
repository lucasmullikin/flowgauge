//! Owns the menu bar items, panels and settings window, and handles their actions.

use crate::bar_image;
use crate::graph_math::{Unit, fraction, log_floor, nice_max};
use crate::order_view::PreviewCallbacks;
use crate::panel::Panel;
use crate::network_window::NetworkWindow;
use crate::power_window::PowerWindow;
use crate::settings_form::{self, Input};
use crate::sensors_window::SensorsWindow;
use crate::settings_window::SettingsWindow;
use crate::view::{self, Latest};
use crate::worker::{self, NO_MODULE, Shared, Update};
use gauge_core::curve::CurvePoint;
use gauge_core::history::{bridge_gaps, bucketize};
use gauge_core::sensors::is_m1_mac;
use gauge_core::settings::{BarStyle, GraphScale, MODULES, Settings, normalized_order, support_dir};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSButton, NSCellImagePosition, NSColor, NSColorSpace, NSColorWell, NSControl,
    NSControlStateValueOn, NSFont, NSFontAttributeName, NSPopUpButton, NSPopoverDelegate,
    NSSegmentedControl, NSSlider, NSStatusBar, NSStatusItem, NSStringDrawing, NSTextField,
};
use objc2_core_location::CLLocationManager;
use objc2_foundation::{MainThreadMarker, NSDictionary, NSNotification, NSRectEdge, NSRunLoop, NSString, NSTimer};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TITLE_FONT_SIZE: f64 = 12.0;
const BAR_GRAPH_POINTS: usize = 18;
const BAR_GRAPH_SECONDS: i64 = 180;
/// A click on the bar closes an open panel on mouse down, then arrives here on mouse up.
/// Without this window, clicking an open panel's item would close and instantly reopen it.
/// macOS redraws a snapshot of an item for every title change, which is the app's largest idle
/// cost. Network stays live at one second; everything else updates less often.
const SLOW_TITLE_SECONDS: i64 = 5;
const ITEM_PADDING: f64 = 4.0;
const WIDTH_STEP: f64 = 2.0;
/// An item with no text still needs something to click.
const MIN_ITEM_WIDTH: f64 = 18.0;
const BAR_GRAPH_WIDTH: f64 = 40.0;
const REOPEN_GUARD: Duration = Duration::from_millis(300);

struct Module {
    name: &'static str,
    index: usize,
    item: Retained<NSStatusItem>,
    panel: Panel,
    title: RefCell<String>,
    last_title_at: Cell<i64>,
    last_image_at: Cell<i64>,
    has_image: Cell<bool>,
}

pub struct Ivars {
    updates: Receiver<Update>,
    shared: Arc<Shared>,
    modules: RefCell<Vec<Module>>,
    latest: RefCell<Latest>,
    settings_window: RefCell<Option<SettingsWindow>>,
    sensors_window: RefCell<Option<SensorsWindow>>,
    power_window: RefCell<Option<PowerWindow>>,
    network_window: RefCell<Option<NetworkWindow>>,
    /// Kept alive so macOS keeps reporting clicks outside the app.
    click_monitor: RefCell<Option<Retained<AnyObject>>>,
    location: RefCell<Option<Retained<CLLocationManager>>>,
    last_closed: Cell<Option<(usize, Instant)>>,
}

fn now_seconds() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn settings_path() -> std::path::PathBuf {
    support_dir().join("settings.json")
}

fn login_agent_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join("Library/LaunchAgents/com.flowgauge.app.plist"))
}

pub fn login_agent_plist(executable: &str) -> String {
    let escaped = executable.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>com.flowgauge.app</string>\n  <key>ProgramArguments</key><array><string>{escaped}</string></array>\n  <key>RunAtLoad</key><true/>\n</dict>\n</plist>\n"
    )
}

/// Removes the login item from when the app was called Gauge, and rewrites the current one so
/// it opens this copy of the app.
pub fn refresh_login_agent(enabled: bool) {
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::fs::remove_file(std::path::PathBuf::from(home).join("Library/LaunchAgents/com.gauge.app.plist"));
    }
    if enabled {
        sync_login_agent(true);
    }
}

fn sync_login_agent(enabled: bool) {
    let Some(path) = login_agent_path() else { return };
    if enabled {
        if let Ok(executable) = std::env::current_exe() {
            let _ = std::fs::write(path, login_agent_plist(&executable.to_string_lossy()));
        }
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// Items hug their text, with the spacing setting as the only extra room. Rounded up a little
/// so a digit's small width change doesn't re-lay out the whole bar.
pub fn fit_width(text_width: f64, style: BarStyle, spacing: f64) -> f64 {
    let content = match style {
        BarStyle::Text => text_width,
        BarStyle::Graph => BAR_GRAPH_WIDTH,
        BarStyle::Both => text_width + BAR_GRAPH_WIDTH,
    };
    let width = content + ITEM_PADDING + spacing;
    (width / WIDTH_STEP).ceil() * WIDTH_STEP
}

/// Network values change every second, so its item is sized for the widest value it could
/// show. That keeps the items beside it from shifting.
fn bar_width(name: &str, title: &str, style: BarStyle, spacing: f64) -> f64 {
    if name == "network" {
        item_width(&view::widest_title(title), style, spacing)
    } else {
        item_width(title, style, spacing)
    }
}

fn item_width(title: &str, style: BarStyle, spacing: f64) -> f64 {
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(TITLE_FONT_SIZE, 0.0);
    // SAFETY: the attribute name static is valid, and measuring has no other requirements.
    let text = unsafe {
        let attributes = NSDictionary::from_slices(&[NSFontAttributeName], &[font.as_ref() as &AnyObject]);
        NSString::from_str(title).sizeWithAttributes(Some(&attributes)).width
    };
    fit_width(text, style, spacing).max(MIN_ITEM_WIDTH)
}

fn color_to_hex(color: &NSColor) -> Option<String> {
    let srgb = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(format!(
        "#{:02X}{:02X}{:02X}",
        channel(srgb.redComponent()),
        channel(srgb.greenComponent()),
        channel(srgb.blueComponent())
    ))
}

/// Finds the installer next to the app, or in the source tree when running a development build.
fn helper_paths() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let executable = std::env::current_exe().ok()?;
    let directory = executable.parent()?;
    let helper = directory.join("gauge-fand");
    let candidates = [
        directory.join("../Resources/install-helper.sh"),
        directory.join("../../scripts/install-helper.sh"),
    ];
    let script = candidates.into_iter().find(|p| p.exists())?;
    helper.exists().then_some((script, helper))
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GaugeController"]
    #[ivars = Ivars]
    pub struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &NSNotification) {
            self.save_history();
        }
    }

    unsafe impl NSPopoverDelegate for Controller {
        #[unsafe(method(popoverDidClose:))]
        fn popover_did_close(&self, _notification: &NSNotification) {
            let open = self.ivars().shared.open_module.swap(NO_MODULE, Ordering::Relaxed);
            self.ivars().last_closed.set(Some((open, Instant::now())));
        }
    }

    impl Controller {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.on_tick();
        }

        #[unsafe(method(toggle:))]
        fn toggle(&self, sender: &NSButton) {
            self.on_toggle(sender);
        }

        #[unsafe(method(rangeChanged:))]
        fn range_changed(&self, sender: &NSSegmentedControl) {
            let name = MODULES[sender.tag() as usize];
            let selected = sender.selectedSegment().max(0) as usize;
            self.change_settings(|settings| {
                if let Some(module) = settings.module_mut(name) {
                    module.range_index = selected;
                }
            });
            self.refresh_open(true);
            self.refresh_power_window(true);
            self.refresh_network_window(true);
        }

        #[unsafe(method(presetChanged:))]
        fn preset_changed(&self, sender: &NSSegmentedControl) {
            let index = sender.selectedSegment().max(0) as usize;
            self.change_settings(|settings| {
                if let Some(name) = settings.presets.get(index).map(|p| p.name.clone()) {
                    settings.active_preset = name;
                }
            });
            self.refresh_open(true);
        }

        #[unsafe(method(fixedChanged:))]
        fn fixed_changed(&self, sender: &NSSlider) {
            let percent = sender.doubleValue() as f32;
            self.change_settings(|settings| settings_form::set_active_fixed_percent(settings, percent));
        }

        #[unsafe(method(installHelper:))]
        fn install_helper(&self, _sender: &NSButton) {
            self.on_install_helper();
        }

        #[unsafe(method(openSensors:))]
        fn open_sensors(&self, _sender: &NSButton) {
            self.show_sensors();
        }

        #[unsafe(method(openPower:))]
        fn open_power(&self, _sender: &NSButton) {
            self.show_power_window();
        }

        #[unsafe(method(openNetwork:))]
        fn open_network(&self, _sender: &NSButton) {
            self.show_network_window();
        }

        #[unsafe(method(flowModeChanged:))]
        fn flow_mode_changed(&self, sender: &NSSegmentedControl) {
            if let Some(window) = &*self.ivars().network_window.borrow() {
                window.set_flow_mode(sender.selectedSegment().max(0) as usize);
            }
            self.refresh_network_window(true);
        }

        #[unsafe(method(appSortChanged:))]
        fn app_sort_changed(&self, sender: &NSSegmentedControl) {
            if let Some(window) = &*self.ivars().network_window.borrow() {
                window.set_app_sort(sender.selectedSegment().max(0) as usize);
            }
            self.refresh_network_window(true);
        }

        #[unsafe(method(showOtherApps:))]
        fn show_other_apps(&self, _sender: &AnyObject) {
            if let Some(window) = &*self.ivars().network_window.borrow() {
                window.show_other();
            }
            self.refresh_network_window(true);
        }

        #[unsafe(method(timelineRangeChanged:))]
        fn timeline_range_changed(&self, sender: &NSSegmentedControl) {
            if let Some(window) = &*self.ivars().network_window.borrow() {
                window.set_timeline_range(sender.selectedSegment().max(0) as usize);
            }
            self.refresh_network_window(true);
        }

        #[unsafe(method(chargeLimitChanged:))]
        fn charge_limit_changed(&self, sender: &NSSlider) {
            let limit = sender.doubleValue().round() as u8;
            self.change_settings(|settings| {
                settings.charge_limit = Some(limit);
                settings.top_up = false;
            });
            self.refresh_open(false);
        }

        #[unsafe(method(topUp:))]
        fn top_up(&self, _sender: &NSButton) {
            let latest_limit = self.ivars().latest.borrow().charge_limit.flatten();
            self.change_settings(|settings| {
                settings.top_up = !settings.top_up;
                // Remember the limit that's in force, so finishing can put it back.
                if settings.charge_limit.is_none() {
                    settings.charge_limit = latest_limit;
                }
            });
            self.refresh_open(false);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: &NSButton) {
            self.show_settings();
        }

        #[unsafe(method(sidebarClicked:))]
        fn sidebar_clicked(&self, sender: &NSButton) {
            self.show_settings_page(sender.tag() as i64);
        }

        #[unsafe(method(settingChanged:))]
        fn setting_changed(&self, sender: &NSControl) {
            self.on_setting_changed(sender);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Controller {
    pub fn new(mtm: MainThreadMarker, updates: Receiver<Update>, shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            updates,
            shared,
            modules: RefCell::new(Vec::new()),
            latest: RefCell::new(Latest::default()),
            settings_window: RefCell::new(None),
            sensors_window: RefCell::new(None),
            power_window: RefCell::new(None),
            network_window: RefCell::new(None),
            location: RefCell::new(None),
            last_closed: Cell::new(None),
            click_monitor: RefCell::new(None),
        });
        // SAFETY: NSObject's init takes no arguments and the ivars are set.
        let controller: Retained<Self> = unsafe { msg_send![super(this), init] };
        controller.watch_outside_clicks();
        controller
    }

    /// Closes an open panel when you click in another app or on the desktop. macOS only reports
    /// clicks outside FlowGauge here, so clicking another FlowGauge item still switches panels.
    fn watch_outside_clicks(&self) {
        let this = self.retain();
        let handler = block2::RcBlock::new(move |_event: std::ptr::NonNull<objc2_app_kit::NSEvent>| {
            let Ok(modules) = this.ivars().modules.try_borrow() else { return };
            for module in modules.iter() {
                if module.panel.popover.isShown() {
                    module.panel.popover.close();
                }
            }
        });
        let mask = objc2_app_kit::NSEventMask::LeftMouseDown
            | objc2_app_kit::NSEventMask::RightMouseDown
            | objc2_app_kit::NSEventMask::OtherMouseDown;
        let monitor = objc2_app_kit::NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &handler);
        *self.ivars().click_monitor.borrow_mut() = monitor;
    }

    /// macOS shows its own prompt the first time. The manager must stay alive for that.
    fn request_location(&self) {
        // SAFETY: documented calls on a live manager, on the main thread.
        let manager = unsafe {
            let manager = CLLocationManager::new();
            manager.requestWhenInUseAuthorization();
            manager
        };
        *self.ivars().location.borrow_mut() = Some(manager);
    }

    fn save_history(&self) {
        if let Ok(mut history) = self.ivars().shared.history.lock()
            && let Err(error) = history.flush_all(now_seconds())
        {
            eprintln!("gauge: could not save history: {error}");
        }
    }

    fn target(&self) -> &AnyObject {
        self.as_ref()
    }

    fn settings(&self) -> Settings {
        self.ivars().shared.settings.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn change_settings(&self, change: impl FnOnce(&mut Settings)) {
        if let Ok(mut settings) = self.ivars().shared.settings.lock() {
            change(&mut settings);
            if let Err(error) = settings.save(&settings_path()) {
                eprintln!("gauge: could not save settings: {error}");
            }
        }
    }

    /// Rebuilds menu bar items to match settings. Created in reverse so Network sits leftmost.
    pub fn rebuild_items(&self) {
        let mtm = self.mtm();
        let settings = self.settings();
        let bar = NSStatusBar::systemStatusBar();
        for module in self.ivars().modules.borrow_mut().drain(..) {
            bar.removeStatusItem(&module.item);
        }
        let mut built = Vec::new();
        for wanted in settings.order.iter().rev() {
            let Some(index) = MODULES.iter().position(|m| m == wanted) else { continue };
            let name = MODULES[index];
            if !settings.module(name).is_some_and(|m| m.shown) {
                continue;
            }
            let style = settings.module(name).map_or(BarStyle::Text, |m| m.bar_style);
            let item = bar.statusItemWithLength(item_width("", style, settings.item_spacing));
            if let Some(button) = item.button(mtm) {
                button.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(TITLE_FONT_SIZE, 0.0)));
                // SAFETY: this controller lives for the whole app and implements toggle:.
                unsafe {
                    button.setTarget(Some(self.target()));
                    button.setAction(Some(sel!(toggle:)));
                }
                button.setTag(index as isize);
            }
            let panel = Panel::new(mtm, name, index, &settings, self.target());
            panel.popover.setDelegate(Some(ProtocolObject::from_ref(self)));
            built.push(Module {
                name,
                index,
                item,
                panel,
                title: RefCell::new(String::new()),
                last_title_at: Cell::new(0),
                last_image_at: Cell::new(0),
                has_image: Cell::new(false),
            });
        }
        *self.ivars().modules.borrow_mut() = built;
        self.refresh_bar();
    }

    fn on_tick(&self) {
        let mut changed = false;
        {
            let mut latest = self.ivars().latest.borrow_mut();
            while let Ok(update) = self.ivars().updates.try_recv() {
                if matches!(update, Update::TopUpFinished) {
                    self.change_settings(|settings| settings.top_up = false);
                }
                worker::absorb(&mut latest, update);
                changed = true;
            }
            if let Ok(mut slot) = self.ivars().shared.install_message.lock()
                && let Some(message) = slot.take()
            {
                latest.install_message = Some(message);
            }
        }
        if changed {
            self.refresh_bar();
        }
        self.refresh_open(false);
        self.refresh_preview();
        self.refresh_sensors();
        self.refresh_power_window(false);
        self.refresh_network_window(false);
    }

    fn refresh_network_window(&self, force: bool) {
        let window = self.ivars().network_window.borrow();
        let open = window.as_ref().is_some_and(|w| w.window.isVisible());
        self.ivars().shared.network_window_open.store(open, Ordering::Relaxed);
        let Some(window) = window.as_ref().filter(|_| open) else { return };
        let settings = self.settings();
        let latest = self.ivars().latest.borrow();
        if let Ok(history) = self.ivars().shared.history.lock() {
            let geo = if settings.show_countries { self.ivars().shared.geo.lock().ok().and_then(|g| g.clone()) } else { None };
            window.refresh_graphs(&history, &settings, &self.ivars().shared.names, geo.as_deref(), &latest, now_seconds(), force);
        }
        window.refresh_text(&latest, &settings);
    }

    fn show_network_window(&self) {
        let mtm = self.mtm();
        if self.ivars().network_window.borrow().is_none() {
            let index = MODULES.iter().position(|m| *m == "network").unwrap_or(0) as isize;
            let window = NetworkWindow::new(mtm, self.target(), index);
            *self.ivars().network_window.borrow_mut() = Some(window);
        }
        for module in self.ivars().modules.borrow().iter() {
            module.panel.popover.close();
        }
        NSApplication::sharedApplication(mtm).activate();
        if let Some(window) = &*self.ivars().network_window.borrow() {
            window.window.makeKeyAndOrderFront(None);
        }
        self.refresh_network_window(true);
    }

    fn refresh_power_window(&self, force: bool) {
        let window = self.ivars().power_window.borrow();
        let Some(window) = window.as_ref().filter(|w| w.window.isVisible()) else { return };
        let settings = self.settings();
        window.refresh_text(&self.ivars().latest.borrow(), &settings);
        if let Ok(history) = self.ivars().shared.history.lock() {
            window.refresh_graphs(&history, &settings, now_seconds(), force);
        }
    }

    fn show_power_window(&self) {
        let mtm = self.mtm();
        if self.ivars().power_window.borrow().is_none() {
            let index = MODULES.iter().position(|m| *m == "battery").unwrap_or(0) as isize;
            let window = PowerWindow::new(mtm, self.target(), index);
            *self.ivars().power_window.borrow_mut() = Some(window);
        }
        for module in self.ivars().modules.borrow().iter() {
            module.panel.popover.close();
        }
        NSApplication::sharedApplication(mtm).activate();
        if let Some(window) = &*self.ivars().power_window.borrow() {
            window.window.makeKeyAndOrderFront(None);
        }
        self.refresh_power_window(true);
    }

    fn refresh_sensors(&self) {
        let window = self.ivars().sensors_window.borrow();
        let open = window.as_ref().is_some_and(|w| w.window.isVisible());
        self.ivars().shared.sensors_open.store(open, Ordering::Relaxed);
        if let Some(window) = window.as_ref().filter(|_| open) {
            let latest = self.ivars().latest.borrow();
            window.set_rows(&view::sensor_rows(&latest.sensors, is_m1_mac(), &self.settings()));
        }
    }

    fn show_sensors(&self) {
        let mtm = self.mtm();
        if self.ivars().sensors_window.borrow().is_none() {
            *self.ivars().sensors_window.borrow_mut() = Some(SensorsWindow::new(mtm));
        }
        for module in self.ivars().modules.borrow().iter() {
            module.panel.popover.close();
        }
        NSApplication::sharedApplication(mtm).activate();
        if let Some(window) = &*self.ivars().sensors_window.borrow() {
            window.window.makeKeyAndOrderFront(None);
        }
        self.ivars().shared.sensors_open.store(true, Ordering::Relaxed);
    }

    fn refresh_bar(&self) {
        let mtm = self.mtm();
        let settings = self.settings();
        let latest = self.ivars().latest.borrow();
        let now = now_seconds();
        for module in self.ivars().modules.borrow().iter() {
            let Some(button) = module.item.button(mtm) else { continue };
            let style = settings.module(module.name).map_or(BarStyle::Text, |m| m.bar_style);
            let title = if style == BarStyle::Graph {
                String::new()
            } else {
                view::bar_title(module.name, &latest, &settings)
            };
            let due = module.name == "network" || now - module.last_title_at.get() >= SLOW_TITLE_SECONDS;
            if due && *module.title.borrow() != title {
                module.last_title_at.set(now);
                button.setTitle(&NSString::from_str(&title));
                let width = bar_width(module.name, &title, style, settings.item_spacing);
                if module.item.length() != width {
                    module.item.setLength(width);
                }
                *module.title.borrow_mut() = title;
            }
            if style == BarStyle::Text {
                // Setting even an empty image redraws the item, so only do it when needed.
                if module.has_image.replace(false) {
                    button.setImage(None);
                }
            } else if now - module.last_image_at.get() >= 2 {
                module.last_image_at.set(now);
                module.has_image.set(true);
                button.setImage(Some(&bar_image::image(&self.bar_shares(module.name, now, &settings))));
                button.setImagePosition(if style == BarStyle::Graph {
                    NSCellImagePosition::ImageOnly
                } else {
                    NSCellImagePosition::ImageLeft
                });
            }
        }
    }

    /// Applies new text or spacing right away, including to titles that normally update slowly.
    fn apply_title_settings(&self) {
        let settings = self.settings();
        for module in self.ivars().modules.borrow().iter() {
            let style = settings.module(module.name).map_or(BarStyle::Text, |m| m.bar_style);
            module.item.setLength(bar_width(module.name, &module.title.borrow(), style, settings.item_spacing));
            module.last_title_at.set(0);
        }
        self.refresh_bar();
    }

    /// Recent values for a menu bar graph: one list per series, on one shared scale.
    fn bar_shares(&self, name: &str, now: i64, settings: &Settings) -> Vec<Vec<f32>> {
        let specs = view::graphs(name, settings);
        let (Some(spec), Ok(history)) = (specs.first(), self.ivars().shared.history.lock()) else {
            return Vec::new();
        };
        let buckets: Vec<_> = spec
            .series
            .iter()
            .map(|(series, _)| {
                let rows = history.rows(series, BAR_GRAPH_SECONDS, now).unwrap_or_default();
                let mut buckets = bucketize(&rows, now - BAR_GRAPH_SECONDS, now, BAR_GRAPH_POINTS);
                bridge_gaps(&mut buckets, BAR_GRAPH_SECONDS as f64 / BAR_GRAPH_POINTS as f64, 60.0);
                buckets
            })
            .collect();
        let values = || buckets.iter().flatten().flatten().map(|b| b.avg);
        let max = spec.unit.fixed_max().unwrap_or_else(|| nice_max(values().fold(0.0, f32::max)));
        let scale = if spec.unit == Unit::Rate { GraphScale::Log } else { GraphScale::Linear };
        let floor = log_floor(values().filter(|v| *v > 0.0).min_by(f32::total_cmp), max, spec.unit);
        buckets
            .iter()
            .map(|series| series.iter().map(|b| b.map_or(0.0, |b| fraction(b.avg, max, floor, scale))).collect())
            .collect()
    }

    fn refresh_open(&self, force: bool) {
        let open = self.ivars().shared.open_module.load(Ordering::Relaxed);
        if open == NO_MODULE {
            return;
        }
        let settings = self.settings();
        let latest = self.ivars().latest.borrow();
        for module in self.ivars().modules.borrow().iter() {
            if module.index == open {
                module.panel.refresh_text(&latest, &settings);
                if let Ok(history) = self.ivars().shared.history.lock() {
                    module.panel.refresh_graphs(&history, &settings, now_seconds(), force);
                }
            }
        }
    }

    fn on_toggle(&self, sender: &NSButton) {
        let mtm = self.mtm();
        let index = sender.tag() as usize;
        if self.ivars().last_closed.get().is_some_and(|(module, at)| module == index && at.elapsed() < REOPEN_GUARD) {
            return;
        }
        let modules = self.ivars().modules.borrow();
        let mut opening = false;
        for module in modules.iter() {
            if module.panel.popover.isShown() {
                module.panel.popover.close();
            } else if module.index == index {
                opening = true;
            }
        }
        if !opening {
            return;
        }
        let Some(module) = modules.iter().find(|m| m.index == index) else { return };
        self.ivars().shared.open_module.store(index, Ordering::Relaxed);
        let settings = self.settings();
        module.panel.refresh_text(&self.ivars().latest.borrow(), &settings);
        if let Ok(history) = self.ivars().shared.history.lock() {
            module.panel.refresh_graphs(&history, &settings, now_seconds(), true);
        }
        module.panel.fit();
        NSApplication::sharedApplication(mtm).activate();
        if let Some(button) = module.item.button(mtm) {
            module.panel.popover.showRelativeToRect_ofView_preferredEdge(button.bounds(), &button, NSRectEdge::MinY);
        }
    }

    fn on_install_helper(&self) {
        let Some((script, helper)) = helper_paths() else {
            if let Ok(mut slot) = self.ivars().shared.install_message.lock() {
                *slot = Some("Install failed: helper files are missing".to_string());
            }
            return;
        };
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        // macOS shows its own password prompt. Paths go in as arguments so they can't be
        // misread as AppleScript.
        let run = "do shell script (quoted form of item 1 of argv) & \" \" & item 2 of argv & \" \" & (quoted form of item 3 of argv) with administrator privileges";
        let result = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", "on run argv", "-e", run, "-e", "end run"])
            .arg(&script)
            .arg(uid.to_string())
            .arg(&helper)
            .stderr(std::process::Stdio::piped())
            .spawn();
        let shared = self.ivars().shared.clone();
        let report = move |message: String| {
            if let Ok(mut slot) = shared.install_message.lock() {
                *slot = Some(message);
            }
        };
        match result {
            Ok(child) => {
                std::thread::spawn(move || match child.wait_with_output() {
                    Ok(output) => report(crate::fan_control::install_result(
                        output.status.success(),
                        &String::from_utf8_lossy(&output.stderr),
                    )),
                    Err(error) => report(format!("Install failed: {error}")),
                });
            }
            Err(error) => report(format!("Install failed: {error}")),
        }
    }

    /// Keeps the preview at the top of Settings in step with the real menu bar.
    fn refresh_preview(&self) {
        let window = self.ivars().settings_window.borrow();
        let Some(window) = window.as_ref().filter(|w| w.window.isVisible() || !w.has_preview_items()) else {
            return;
        };
        let settings = self.settings();
        let latest = self.ivars().latest.borrow();
        window.set_preview(view::preview_items(&latest, &settings), settings.item_spacing);
    }

    fn show_settings(&self) {
        let mtm = self.mtm();
        if self.ivars().settings_window.borrow().is_none() {
            let for_order = self.retain();
            let for_select = self.retain();
            let callbacks = PreviewCallbacks {
                on_order: Box::new(move |order: Vec<String>| {
                    for_order.change_settings(|settings| settings.order = normalized_order(&order));
                    for_order.rebuild_items();
                }),
                on_select: Box::new(move |name: String| {
                    if let Some(index) = MODULES.iter().position(|m| *m == name) {
                        for_select.show_settings_page(index as i64 + 1);
                    }
                }),
            };
            let window = SettingsWindow::new(mtm, self.target(), callbacks);
            *self.ivars().settings_window.borrow_mut() = Some(window);
            self.show_settings_page(0);
            self.refresh_preview();
        }
        for module in self.ivars().modules.borrow().iter() {
            module.panel.popover.close();
        }
        NSApplication::sharedApplication(mtm).activate();
        if let Some(window) = &*self.ivars().settings_window.borrow() {
            window.window.makeKeyAndOrderFront(None);
        }
    }

    fn show_settings_page(&self, page: i64) {
        let this = self.retain();
        let on_curve: Rc<dyn Fn(usize, Vec<CurvePoint>)> = Rc::new(move |index, points| {
            this.change_settings(|settings| settings_form::set_preset_curve(settings, index, points));
        });
        if let Some(window) = &*self.ivars().settings_window.borrow() {
            window.show_page(self.mtm(), page, &self.settings(), self.target(), on_curve);
        }
    }

    fn on_setting_changed(&self, sender: &NSControl) {
        let tag = sender.tag() as i64;
        if tag == settings_form::UPDATE_GEO_TAG {
            self.ivars().shared.geo_refresh.store(true, Ordering::Relaxed);
            if !self.settings().show_countries {
                self.change_settings(|settings| settings.show_countries = true);
                self.show_settings_page(1);
            }
            return;
        }
        if tag == settings_form::CLEAR_NETWORK_TAG {
            if let Ok(mut history) = self.ivars().shared.history.lock()
                && let Err(error) = history.clear_network()
            {
                eprintln!("gauge: could not clear network history: {error}");
            }
            self.ivars().shared.names.clear();
            self.refresh_network_window(true);
            return;
        }
        if let Some(page) = settings_form::page_opened_by(tag) {
            self.show_settings_page(page);
            return;
        }
        let any: &AnyObject = sender.as_ref();
        let input = if let Some(menu) = any.downcast_ref::<NSPopUpButton>() {
            Input::Index(menu.indexOfSelectedItem().max(0) as usize)
        } else if let Some(segments) = any.downcast_ref::<NSSegmentedControl>() {
            Input::Index(segments.selectedSegment().max(0) as usize)
        } else if let Some(button) = any.downcast_ref::<NSButton>() {
            settings_form::input_for_button(tag, button.state() == NSControlStateValueOn)
        } else if let Some(well) = any.downcast_ref::<NSColorWell>() {
            match color_to_hex(&well.color()) {
                Some(hex) => Input::Color(hex),
                None => return,
            }
        } else if let Some(slider) = any.downcast_ref::<NSSlider>() {
            if let Some(window) = &*self.ivars().settings_window.borrow() {
                window.update_readout(tag, slider.doubleValue());
            }
            Input::Number(slider.doubleValue())
        } else if let Some(field) = any.downcast_ref::<NSTextField>() {
            Input::Text(field.stringValue().to_string())
        } else {
            return;
        };
        let before = self.settings();
        self.change_settings(|settings| {
            settings_form::apply(settings, tag, input);
        });
        let after = self.settings();
        if after.show_wifi_name && !before.show_wifi_name {
            self.request_location();
        }
        if before.launch_at_login != after.launch_at_login {
            sync_login_agent(after.launch_at_login);
        }
        let layout = |s: &Settings| {
            (
                MODULES.map(|m| s.module(m).map(|x| (x.shown, x.bar_style))),
                s.presets.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            )
        };
        if layout(&before) != layout(&after) {
            self.rebuild_items();
        } else {
            self.apply_title_settings();
        }
        self.refresh_preview();
        if settings_form::needs_page_refresh(tag) || before.module_colors() != after.module_colors() || layout(&before) != layout(&after) {
            let page = self.ivars().settings_window.borrow().as_ref().map(|w| w.page());
            if let Some(page) = page {
                self.show_settings_page(page);
            }
        }
    }
}

/// Drives the real controls in the real app, for checks that unit tests can't reach.
impl Controller {
    fn spin(&self, seconds: f64) {
        for _ in 0..(seconds * 4.0) as u32 {
            let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.25);
            NSRunLoop::mainRunLoop().runUntilDate(&until);
            self.on_tick();
        }
    }

    /// Where macOS actually put each menu bar item, leftmost first.
    fn print_item_gaps(&self) {
        let mtm = self.mtm();
        let mut frames: Vec<(&str, f64, f64)> = self
            .ivars()
            .modules
            .borrow()
            .iter()
            .filter_map(|m| {
                let frame = m.item.button(mtm)?.window()?.frame();
                Some((m.name, frame.origin.x, frame.size.width))
            })
            .collect();
        frames.sort_by(|a, b| a.1.total_cmp(&b.1));
        for pair in frames.windows(2) {
            let gap = pair[1].1 - pair[0].1 - pair[0].2;
            println!("GAP {} to {}: {gap:.1} points ({} is {:.1} wide)", pair[0].0, pair[1].0, pair[0].0, pair[0].2);
        }
    }

    /// Opens every panel for the first time and measures how close content sits to the edges.
    pub fn layout_test(&self) -> bool {
        let mtm = self.mtm();
        self.spin(3.0);
        self.print_item_gaps();
        let mut passed = true;
        let count = self.ivars().modules.borrow().len();
        for position in 0..count {
            let (name, button) = {
                let modules = self.ivars().modules.borrow();
                (modules[position].name, modules[position].item.button(mtm))
            };
            let Some(button) = button else { continue };
            self.on_toggle(&button);
            let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.3);
            NSRunLoop::mainRunLoop().runUntilDate(&until);
            let modules = self.ivars().modules.borrow();
            let panel = &modules[position].panel;
            let (left, right) = panel.margins();
            let width = panel.root.bounds().size.width;
            // Controls' frames extend about 2 points past what they draw.
            let ok = left >= crate::panel::INSET - 2.5 && right >= crate::panel::INSET - 2.5;
            println!("{} {name}: left {left:.1} right {right:.1} panel width {width:.1}", if ok { "PASS" } else { "FAIL" });
            passed &= ok;
            panel.popover.close();
        }
        passed
    }

    pub fn self_test(&self) -> bool {
        let mtm = self.mtm();
        let mut passed = true;
        let mut check = |name: &str, ok: bool| {
            println!("{} {name}", if ok { "PASS" } else { "FAIL" });
            passed &= ok;
        };
        self.spin(4.0);
        let fans_index = MODULES.iter().position(|m| *m == "fans").unwrap_or(0);
        let button = {
            let modules = self.ivars().modules.borrow();
            modules.iter().find(|m| m.index == fans_index).and_then(|m| m.item.button(mtm))
        };
        check("fans item has a button", button.is_some());
        let Some(button) = button else { return false };
        self.on_toggle(&button);
        self.spin(1.0);
        let shown = |c: &Controller| {
            c.ivars().modules.borrow().iter().find(|m| m.index == fans_index).is_some_and(|m| m.panel.popover.isShown())
        };
        check("clicking the item opens the panel", shown(self));
        check("open panel is tracked", self.ivars().shared.open_module.load(Ordering::Relaxed) == fans_index);
        let send = |control: &NSControl| {
            // SAFETY: re-sends the control's own action to its own target, as a click does.
            unsafe {
                let action = control.action();
                let target = control.target();
                control.sendAction_to(action, target.as_deref())
            }
        };
        {
            let modules = self.ivars().modules.borrow();
            let module = modules.iter().find(|m| m.index == fans_index);
            if let Some(controls) = module.and_then(|m| m.panel.fan_controls()) {
                controls.presets.setSelectedSegment(1);
                check("preset button has a working action", send(&controls.presets));
            }
            if let Some(module) = module {
                module.panel.range_control().setSelectedSegment(0);
                check("range button has a working action", send(module.panel.range_control()));
            }
        }
        check("preset choice reached settings", self.settings().active_preset == "Cool");
        check("range choice reached settings", self.settings().fans.range_index == 0);
        check("settings were saved", settings_path().exists());
        self.spin(5.0);
        let state = self.ivars().latest.borrow().fan_state.clone();
        println!("     fan state: {}", state.label());
        {
            let modules = self.ivars().modules.borrow();
            let module = modules.iter().find(|m| m.index == fans_index);
            if let Some(controls) = module.and_then(|m| m.panel.fan_controls()) {
                let missing = state == crate::fan_control::FanState::HelperMissing;
                check("install button matches helper state", controls.install.isHidden() != missing);
                controls.presets.setSelectedSegment(0);
                send(&controls.presets);
            }
        }
        check("back on Auto", self.settings().active_preset == "Auto");
        self.show_settings();
        self.spin(1.0);
        let visible = self.ivars().settings_window.borrow().as_ref().is_some_and(|w| w.window.isVisible());
        check("settings window opens", visible);
        self.show_power_window();
        self.spin(1.0);
        let power_open = self.ivars().power_window.borrow().as_ref().is_some_and(|w| w.window.isVisible());
        check("power window opens", power_open);
        let flow_ready = view::power_flow(&self.ivars().latest.borrow()).is_some();
        check("power flow has readings", flow_ready);
        self.show_network_window();
        // The sampler may be mid-way through its slow pace, and its first sample only sets a start.
        self.spin(14.0);
        let network_open = self.ivars().network_window.borrow().as_ref().is_some_and(|w| w.window.isVisible());
        check("network window opens", network_open);
        let has_path = self.ivars().latest.borrow().path.is_some();
        check("path check reports", has_path);
        let apps = self.ivars().latest.borrow().traffic.as_ref().map_or(0, |t| t.apps.len());
        println!("     apps with traffic: {apps}");
        check("app traffic arrives", self.ivars().latest.borrow().traffic.is_some());
        let (placed, remote) = self.ivars().latest.borrow().traffic.as_ref().map_or((0, 0), |t| {
            let remote: Vec<_> = t.apps.iter().flat_map(|a| &a.destinations).filter(|d| !gauge_core::traffic::is_local(&d.address)).collect();
            (remote.iter().filter(|d| d.country.is_some()).count(), remote.len())
        });
        println!("     remote connections with a country: {placed} of {remote} (countries are {})", if self.settings().show_countries { "on" } else { "off" });
        let dots = self.ivars().network_window.borrow().as_ref().map_or(0, |w| w.moving_dots());
        println!("     moving dots: {dots} (none is right when idle or with Reduce Motion on)");
        self.show_sensors();
        self.spin(4.5);
        let sensor_count = self.ivars().latest.borrow().sensors.len();
        println!("     sensors listed: {sensor_count}");
        check("sensor window receives readings", sensor_count > 0);
        for page in 0..settings_form::PAGES.len() as i64 {
            self.show_settings_page(page);
        }
        check("every settings page builds", true);
        check("panel closes when settings open", !shown(self));
        passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_hug_their_text_plus_the_spacing_setting() {
        assert_eq!(fit_width(100.0, BarStyle::Text, 0.0), 104.0);
        assert_eq!(fit_width(100.0, BarStyle::Text, 20.0), 124.0);
        assert_eq!(fit_width(101.0, BarStyle::Text, 0.0), 106.0);
        assert_eq!(fit_width(100.0, BarStyle::Graph, 0.0), BAR_GRAPH_WIDTH + 4.0);
        assert_eq!(fit_width(100.0, BarStyle::Both, 0.0), 100.0 + BAR_GRAPH_WIDTH + 4.0);
    }

    #[test]
    fn login_agent_escapes_the_executable_path() {
        let plist = login_agent_plist("/Apps/A&B/gauge");
        assert!(plist.contains("<string>/Apps/A&amp;B/gauge</string>"));
        assert!(plist.contains("com.flowgauge.app"));
    }
}
