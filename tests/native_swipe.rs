//! Opt-in native test of the tab swipe monitor: synthetic trackpad scroll gestures, posted to
//! this process's own event queue, over GPUI chrome and over a native page.
//! Run: FELINAUT_NATIVE_TEST=1 cargo test --test native_swipe
//!
//! Three-finger swipes (`NSEventTypeSwipe`) cannot be synthesized and are covered by the
//! model's unit tests only.

#[allow(dead_code, unused_imports)]
#[path = "../src/audio.rs"]
mod audio;
#[allow(dead_code, unused_imports)]
#[path = "../src/downloads.rs"]
mod downloads;
#[allow(dead_code, unused_imports)]
#[path = "../src/icons.rs"]
mod icons;
#[allow(dead_code, unused_imports)]
#[path = "../src/load_error.rs"]
mod load_error;
#[allow(dead_code)]
#[path = "../src/swipe.rs"]
mod swipe;
#[allow(dead_code)]
#[path = "../src/webview.rs"]
mod webview;

use gpui::{prelude::*, *};
use objc2::{
    Encoding, Message, RefEncode, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_app_kit::{NSEvent, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect};
use std::{cell::RefCell, ffi::c_void, rc::Rc, time::Duration};
use swipe::Step;
use webview::NativePage;
use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// The chrome is this wide; the page fills the rest.
const CHROME: f32 = 300.;

/// `CGEventRef`, opaque. The encoding lets objc2 check `eventWithCGEvent:` in debug builds.
#[repr(C)]
struct CGEvent {
    _private: [u8; 0],
}
unsafe impl RefEncode for CGEvent {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateScrollWheelEvent2(
        source: *const c_void,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> *mut CGEvent;
    fn CGEventSetIntegerValueField(event: *mut CGEvent, field: u32, value: i64);
    fn CGEventSetLocation(event: *mut CGEvent, location: NSPoint);
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> NSRect;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: *const c_void);
}

const PIXEL_UNITS: u32 = 0;
const FIELD_CONTINUOUS: u32 = 88;
const FIELD_WINDOW: u32 = 91;
const FIELD_WINDOW_THAT_CAN_HANDLE: u32 = 92;
const FIELD_SCROLL_PHASE: u32 = 99;
const FIELD_MOMENTUM_PHASE: u32 = 123;
// CGScrollPhase and CGMomentumScrollPhase values, which AppKit turns into NSEventPhase.
const BEGAN: i64 = 1;
const CHANGED: i64 = 2;
const ENDED: i64 = 4;
const CANCELLED: i64 = 8;
const MOMENTUM_BEGIN: i64 = 1;
const MOMENTUM_CONTINUE: i64 = 2;
const MOMENTUM_END: i64 = 3;

struct Target {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
}

impl Target {
    fn new(window: &Window) -> Self {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)
            .expect("window handle")
            .as_raw()
        else {
            panic!("not an AppKit window");
        };
        // SAFETY: GPUI hands out its live content view, and we are on the main thread.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let window = view.window().expect("NSWindow");
        // Other apps' windows must not cover the points the gestures are posted to.
        window.setLevel(objc2_app_kit::NSFloatingWindowLevel);
        Self {
            window,
            view: view.retain(),
        }
    }

    /// A point given from the GPUI view's top-left corner, in global display coordinates.
    fn global(&self, x: f64, y: f64) -> NSPoint {
        let height = self.view.bounds().size.height;
        let local = NSPoint::new(x, if self.view.isFlipped() { y } else { height - y });
        let in_window = self.view.convertPoint_toView(local, None);
        let screen = self.window.convertPointToScreen(in_window);
        let mtm = objc2::MainThreadMarker::new().expect("main thread");
        assert_eq!(
            NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(screen, 0, mtm),
            self.window.windowNumber(),
            "another window covers the test window at {x}, {y}"
        );
        // SAFETY: plain CoreGraphics queries.
        let main = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        NSPoint::new(screen.x, main.size.height - screen.y)
    }

    /// Posts one scroll event to this app's queue, where the event monitor sees it.
    fn post(&self, at: NSPoint, dx: i32, dy: i32, phase: i64, momentum: i64) {
        let number = self.window.windowNumber() as i64;
        // SAFETY: the event is created, configured and released here; AppKit copies it.
        unsafe {
            let event = CGEventCreateScrollWheelEvent2(std::ptr::null(), PIXEL_UNITS, 2, dy, dx, 0);
            assert!(!event.is_null(), "scroll event");
            CGEventSetLocation(event, at);
            CGEventSetIntegerValueField(event, FIELD_CONTINUOUS, 1);
            CGEventSetIntegerValueField(event, FIELD_SCROLL_PHASE, phase);
            CGEventSetIntegerValueField(event, FIELD_MOMENTUM_PHASE, momentum);
            CGEventSetIntegerValueField(event, FIELD_WINDOW, number);
            CGEventSetIntegerValueField(event, FIELD_WINDOW_THAT_CAN_HANDLE, number);
            let ns: Option<Retained<NSEvent>> =
                msg_send![AnyClass::get(c"NSEvent").unwrap(), eventWithCGEvent: event];
            CFRelease(event.cast());
            let ns = ns.expect("NSEvent from CGEvent");
            let app: *mut AnyObject =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let _: () = msg_send![app, postEvent: &*ns, atStart: false];
        }
    }

    /// Fingers down, `moves` movements, fingers up.
    fn gesture(&self, at: NSPoint, moves: &[(i32, i32)]) {
        self.post(at, 0, 0, BEGAN, 0);
        for &(dx, dy) in moves {
            self.post(at, dx, dy, CHANGED, 0);
        }
        self.post(at, 0, 0, ENDED, 0);
    }
}

struct TestWindow {
    page: Rc<NativePage>,
}
impl Render for TestWindow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let page = self.page.clone();
        div()
            .flex()
            .size_full()
            .child(div().w(px(CHROME)).h_full().bg(rgb(0x202030)))
            .child(
                canvas(
                    move |bounds, _, _| {
                        page.set_bounds(bounds).expect("native bounds");
                        page.set_visible(true).expect("native visibility");
                    },
                    |_, _, _, _| {},
                )
                .flex_1()
                .h_full(),
            )
    }
}

fn main() {
    if std::env::var_os("FELINAUT_NATIVE_TEST")
        .or_else(|| std::env::var_os("FELIFOX_NATIVE_TEST"))
        .as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        println!("Native swipe test skipped; set FELINAUT_NATIVE_TEST=1 to run.");
        return;
    }
    Application::new().run(move |cx| {
        let handle = cx
            .open_window(
                WindowOptions {
                    // Top left, clear of alerts that macOS centres on the screen.
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(40.), px(60.)),
                        size(px(800.), px(500.)),
                    ))),
                    focus: true,
                    show: true,
                    ..Default::default()
                },
                |window, cx| {
                    let page = NativePage::new(window, "about:blank").expect("native page");
                    cx.new(|_| TestWindow {
                        page: Rc::new(page),
                    })
                },
            )
            .expect("window");
        cx.activate(true);
        let steps: Rc<RefCell<Vec<Step>>> = Rc::default();
        let swipes = handle
            .update(cx, |_, window, _| webview::watch_swipes(window))
            .expect("update")
            .expect("swipe monitor");
        let seen = steps.clone();
        cx.spawn(async move |_| {
            while let Some(step) = swipes.next().await {
                seen.borrow_mut().push(step);
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            Timer::after(Duration::from_millis(800)).await;
            let target = handle
                .update(cx, |_, window, _| Target::new(window))
                .expect("target");
            let chrome = target.global(f64::from(CHROME) / 2., 250.);
            let page = target.global(f64::from(CHROME) + 250., 250.);
            let enabled = NSEvent::isSwipeTrackingFromScrollEventsEnabled();
            // A real swipe, then its opposite: one step each, however long the fingers move.
            target.gesture(chrome, &[(-20, 0); 12]);
            target.gesture(chrome, &[(20, 0); 12]);
            // None of these may switch.
            target.gesture(chrome, &[(0, -20); 12]);
            target.gesture(chrome, &[(-20, -20); 12]);
            target.gesture(chrome, &[(-10, 0); 3]);
            target.post(chrome, -40, 0, 0, MOMENTUM_BEGIN);
            for _ in 0..10 {
                target.post(chrome, -40, 0, 0, MOMENTUM_CONTINUE);
            }
            target.post(chrome, 0, 0, 0, MOMENTUM_END);
            target.gesture(page, &[(-20, 0); 12]);
            target.post(chrome, 0, 0, BEGAN, 0);
            target.post(chrome, -20, 0, CHANGED, 0);
            target.post(chrome, 0, 0, CANCELLED, 0);
            target.post(chrome, -60, 0, CHANGED, 0);
            // Proves the queue was read to here.
            target.gesture(chrome, &[(-20, 0); 12]);
            Timer::after(Duration::from_millis(800)).await;
            let steps = steps.borrow().clone();
            if enabled {
                assert!(
                    matches!(steps.as_slice(), [first, second, last] if first != second && first == last),
                    "expected one step per real swipe, got {steps:?}"
                );
                println!("Native swipe test passed.");
            } else {
                assert!(steps.is_empty(), "swipes are off in System Settings, got {steps:?}");
                println!("Native swipe test passed; two-finger swipes are off in System Settings, so none switched.");
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    });
    // GPUI exits the process when it quits; getting here means the checks never finished.
    panic!("native swipe test did not finish");
}
