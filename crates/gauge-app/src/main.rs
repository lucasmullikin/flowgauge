//! Gauge menu bar app.

mod bar_image;
mod controller;
mod curve_view;
mod fan_control;
mod flow_view;
mod graph_math;
mod graph_view;
mod motion;
mod net_flow_view;
mod net_view;
mod network_window;
mod order_view;
mod panel;
mod path_view;
mod power_window;
mod render;
mod settings_form;
mod sensors_window;
mod settings_window;
mod timeline_view;
mod ui;
mod view;
mod worker;

use controller::Controller;
use gauge_core::history::History;
use gauge_core::settings::{Settings, support_dir};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{MainThreadMarker, NSRunLoop, NSRunLoopCommonModes, NSString, NSTimer, NSUserDefaults};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, mpsc};
use worker::{NO_MODULE, Shared};

const REFRESH_SECONDS: f64 = 1.0;

/// macOS pads every status item, which is the gap between them. These standard keys normally
/// apply system-wide, but set here they change only this app's items.
fn tighten_status_items() {
    let defaults = NSUserDefaults::standardUserDefaults();
    for key in ["NSStatusItemSpacing", "NSStatusItemSelectionPadding"] {
        defaults.setInteger_forKey(0, &NSString::from_str(key));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mtm = MainThreadMarker::new().expect("must start on the main thread");
    tighten_status_items();
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    if args.first().is_some_and(|a| a == "--render") {
        if let Err(message) = render::run(&args[1..]) {
            eprintln!("{message}");
            std::process::exit(1);
        }
        return;
    }

    let directory = support_dir();
    let settings = Settings::load(&directory.join("settings.json"));
    let history = match History::open(&directory.join("history.sqlite")) {
        Ok(history) => history,
        Err(error) => {
            // History is a convenience, so keep monitoring even if the file can't be opened.
            eprintln!("gauge: history won't be saved: {error}");
            History::open_in_memory().expect("in-memory database")
        }
    };
    let shared = Arc::new(Shared {
        history: Arc::new(Mutex::new(history)),
        settings: Arc::new(Mutex::new(settings)),
        open_module: AtomicUsize::new(NO_MODULE),
        install_message: Mutex::new(None),
        sensors_open: std::sync::atomic::AtomicBool::new(false),
        network_window_open: std::sync::atomic::AtomicBool::new(false),
        names: gauge_core::names::Names::start(),
        geo: Mutex::new(None),
        geo_refresh: std::sync::atomic::AtomicBool::new(false),
    });
    let (sender, receiver) = mpsc::channel();
    let worker_shared = shared.clone();
    std::thread::spawn(move || worker::run(sender, worker_shared));

    let controller = Controller::new(mtm, receiver, shared);
    controller.rebuild_items();
    app.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*controller)));

    if args.first().is_some_and(|a| a == "--layout-test") {
        std::process::exit(if controller.layout_test() { 0 } else { 1 });
    }
    if args.first().is_some_and(|a| a == "--self-test") {
        std::process::exit(if controller.self_test() { 0 } else { 1 });
    }

    // SAFETY: tick: exists on the target, which stays alive for the whole run loop.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            REFRESH_SECONDS,
            &controller,
            sel!(tick:),
            None,
            true,
        )
    };
    // SAFETY: the mode constant is a valid run loop mode. Common modes keep it firing while a
    // control is being dragged or a menu is open.
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    app.run();
}
