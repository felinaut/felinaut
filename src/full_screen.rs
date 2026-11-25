//! ⌃⌘F: native full screen in a new Space, or filling the current desktop. Each window fills
//! on its own, keyed by its window number.

/// A window or screen rectangle in AppKit's screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A window's fill: its frame from before, and the frame it got once the resize
/// ended (`None` while the resize runs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    restore: Frame,
    filled: Option<Frame>,
}

/// ⌃⌘F while filling: the frame to restore, or `None` while the fill resize still runs, so a
/// quick second press can't save the screen as the frame to restore.
pub fn restore(fill: &mut Option<Fill>) -> Option<Frame> {
    fill.filter(|fill| fill.filled.is_some()).map(|done| {
        *fill = None;
        done.restore
    })
}

/// The window moved or resized. A fill the user moved, resized or made native with the green
/// button is over; true if it was. Frames during the fill resize are ignored.
pub fn bounds_changed(fill: &mut Option<Fill>, now: Frame, native: bool) -> bool {
    let near = |a: Frame| {
        [
            a.x - now.x,
            a.y - now.y,
            a.width - now.width,
            a.height - now.height,
        ]
        .iter()
        .all(|d| d.abs() <= 0.5)
    };
    let over = fill.is_some_and(|fill| native || fill.filled.is_some_and(|f| !near(f)));
    if over {
        *fill = None;
    }
    over
}

/// What to do when WebKit asks a page's full screen window to enter or leave full screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PageCall {
    /// Let AppKit do it: native full screen.
    Native,
    /// Cover the current screen ourselves.
    Fill,
    /// Undo our fill.
    Restore,
}

/// A page enters (`exit` false) or leaves full screen; `ours` is whether the window is filled
/// by us. Decided by the call, not by what we remember, so a missed exit can't turn the next
/// enter into an exit.
pub fn page_call(native: bool, exit: bool, ours: bool, fill_pages: bool) -> PageCall {
    match (native, exit) {
        (false, true) if ours => PageCall::Restore,
        (false, false) if fill_pages => PageCall::Fill,
        _ => PageCall::Native,
    }
}

/// How long the layout takes to slide below macOS's title bar strip, or back.
const SLIDE: std::time::Duration = std::time::Duration::from_millis(200);

/// The layout's offset below macOS's title bar strip in native full screen, as Firefox moves
/// its toolbar down while the strip shows.
#[derive(Debug, Default)]
pub struct Reveal {
    from: f32,
    target: f32,
    changed: Option<std::time::Instant>,
}

impl Reveal {
    /// The strip now covers `overlap` points; true if the layout has to move. While the strip
    /// slides in, smaller readings are ignored, so the layout never steps back.
    pub fn set(&mut self, overlap: f32, now: std::time::Instant, animate: bool) -> bool {
        let overlap = if overlap.is_finite() {
            overlap.max(0.)
        } else {
            0.
        };
        if overlap == self.target || (overlap > 0. && overlap < self.target) {
            return false;
        }
        self.from = if animate { self.offset(now) } else { overlap };
        self.target = overlap;
        self.changed = Some(now);
        true
    }

    /// The offset at `now`, eased out.
    pub fn offset(&self, now: std::time::Instant) -> f32 {
        let t = self.changed.map_or(1., |changed| {
            (now.saturating_duration_since(changed).as_secs_f32() / SLIDE.as_secs_f32()).min(1.)
        });
        let eased = 1. - (1. - t).powi(3);
        self.from + (self.target - self.from) * eased
    }

    pub fn moving(&self, now: std::time::Instant) -> bool {
        self.offset(now) != self.target
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::{Fill, Frame};
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{ClassType, MainThreadMarker, msg_send, rc::Retained, sel};
    use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions as Options, NSWindow};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::cell::RefCell;

    thread_local! {
        static FILLS: RefCell<Vec<(isize, Fill)>> = const { RefCell::new(Vec::new()) };
        /// A page fills the screen, in WebKit's window; Escape leaves it.
        static PAGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn get(window: isize) -> Option<Fill> {
        FILLS.with_borrow(|fills| fills.iter().find(|(w, _)| *w == window).map(|(_, f)| *f))
    }

    fn set(window: isize, fill: Option<Fill>) {
        FILLS.with_borrow_mut(|fills| {
            fills.retain(|(w, _)| *w != window);
            fills.extend(fill.map(|fill| (window, fill)));
        });
    }

    /// ⌃⌘F: leaves the fill or native full screen, or enters one, filling when `fill`.
    pub fn toggle(window: &gpui::Window, cx: &gpui::App, fill: bool, animate: bool) {
        let ns_window = ns_window(window);
        let number = ns_window.as_ref().map_or(0, |w| w.windowNumber());
        let mut state = get(number);
        let target = match (&ns_window, state) {
            (Some(_), Some(_)) => match super::restore(&mut state) {
                Some(frame) => frame,
                None => return,
            },
            (Some(ns_window), None) if fill && !window.is_fullscreen() => {
                let Some(screen) = ns_window.screen() else {
                    return window.toggle_fullscreen();
                };
                // Below a MacBook's camera housing.
                let mut target = frame(screen.frame());
                target.height -= screen.safeAreaInsets().top.max(0.);
                state = Some(Fill {
                    restore: frame(ns_window.frame()),
                    filled: None,
                });
                target
            }
            _ => return window.toggle_fullscreen(),
        };
        set(number, state);
        let Some(ns_window) = ns_window else { return };
        // After this action: AppKit's resize callbacks update the GPUI window. An animated
        // resize returns when it ends.
        cx.foreground_executor()
            .spawn(async move {
                present();
                ns_window.setFrame_display_animate(rect(target), true, animate);
                if let Some(fill) = get(number) {
                    let filled = Some(frame(ns_window.frame()));
                    set(number, Some(Fill { filled, ..fill }));
                }
            })
            .detach();
    }

    static FILL_PAGES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Makes a page's own full screen (a video's button, `requestFullscreen`) fill the current
    /// desktop too when `fill`.
    pub fn fill_pages(fill: bool) {
        FILL_PAGES.store(fill, std::sync::atomic::Ordering::Relaxed);
    }

    /// WebKit moves a full screen page into its `WebCoreFullScreenWindow` and calls
    /// `enterFullScreenMode:` and `exitFullScreenMode:` on it; that class gets its own versions,
    /// which fill the screen when Settings ask for it and reports the transition to WebKit as AppKit would. WebKit loads
    /// the class with its first page; call this on the main thread until then, it's cheap.
    pub fn hook_pages() {
        use objc2::runtime::{AnyClass, Imp};
        thread_local!(static HOOKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        if HOOKED.get() {
            return;
        }
        let Some(class) = AnyClass::get(c"WebCoreFullScreenWindow") else {
            return;
        };
        HOOKED.set(true);
        // SAFETY: the function matches `v@:@`, the encoding of both methods. Adding only
        // succeeds where the class doesn't define the method itself, so WebKit's code is never
        // replaced; it then overrides the inherited NSWindow method.
        unsafe {
            let class = class as *const AnyClass as *mut AnyClass;
            let toggle = std::mem::transmute::<Toggle, Imp>(toggle_page);
            for name in [sel!(enterFullScreenMode:), sel!(exitFullScreenMode:)] {
                objc2::ffi::class_addMethod(class, name, toggle, c"v@:@".as_ptr());
            }
        }
    }

    type Toggle = unsafe extern "C-unwind" fn(&NSWindow, Sel, *mut AnyObject);

    unsafe extern "C-unwind" fn toggle_page(window: &NSWindow, cmd: Sel, sender: *mut AnyObject) {
        use objc2::runtime::Imp;
        use std::sync::atomic::Ordering;
        let number = window.windowNumber();
        let native = window
            .styleMask()
            .contains(objc2_app_kit::NSWindowStyleMask::FullScreen);
        let exit = cmd == sel!(exitFullScreenMode:);
        let call = super::page_call(
            native,
            exit,
            get(number).is_some(),
            FILL_PAGES.load(Ordering::Relaxed),
        );
        if call == super::PageCall::Native {
            // SAFETY: NSWindow's own implementation, for the same selector and arguments.
            unsafe {
                let Some(method) = NSWindow::class().instance_method(cmd) else {
                    return;
                };
                let imp = std::mem::transmute::<Imp, Toggle>(method.implementation());
                return imp(window, cmd, sender);
            }
        }
        PAGE.set(!exit);
        let done = if exit {
            set(number, None);
            sel!(windowDidExitFullScreen:)
        } else {
            let Some(screen) = window.screen() else {
                return;
            };
            let target = frame(screen.frame());
            let restore = frame(window.frame());
            set(
                number,
                Some(Fill {
                    restore,
                    filled: Some(target),
                }),
            );
            // No title bar over the page.
            window.setStyleMask(objc2_app_kit::NSWindowStyleMask::Borderless);
            window.setFrame_display(rect(target), true);
            sel!(windowDidEnterFullScreen:)
        };
        present();
        // AppKit tells the delegate (WebKit's controller) once the transition ends, after this
        // call returns; so do we.
        if let Some(delegate) = window.delegate() {
            let nothing: Option<&AnyObject> = None;
            // SAFETY: WebKit's controller implements both selectors; a zero delay runs them on
            // the main run loop after this call.
            let () = unsafe {
                msg_send![&*delegate, performSelector: done, withObject: nothing, afterDelay: 0f64]
            };
        }
    }

    /// ⌃⌘F while a page's full screen window is key, where GPUI doesn't see the key: leaves
    /// it, as Escape does.
    pub fn leave_page() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if let Some(window) = NSApplication::sharedApplication(mtm).keyWindow()
            && window.class().name().to_bytes() == b"WebCoreFullScreenWindow"
        {
            let nothing: Option<&AnyObject> = None;
            // SAFETY: WebKit's window implements `cancelOperation:`, its Escape.
            let () = unsafe { msg_send![&*window, cancelOperation: nothing] };
        }
    }

    /// How far macOS's title bar strip covers the top of `window`, in points: in native full
    /// screen it slides in over the window while the pointer is at the top. 0 while hidden.
    pub fn title_bar_overlap(window: &gpui::Window) -> f32 {
        let (Some(ns_window), Some(mtm)) = (ns_window(window), MainThreadMarker::new()) else {
            return 0.;
        };
        if !window.is_fullscreen() {
            return 0.;
        }
        let top = ns_window.frame().origin.y + ns_window.frame().size.height;
        // The strip's window is taller than what shows: only a title bar's height of it is
        // drawn, the rest is its shadow.
        let content = NSRect::new(NSPoint::new(0., 0.), NSSize::new(100., 100.));
        let title_bar = NSWindow::frameRectForContentRect_styleMask(
            content,
            objc2_app_kit::NSWindowStyleMask::Titled,
            mtm,
        )
        .size
        .height
            - 100.;
        NSApplication::sharedApplication(mtm)
            .windows()
            .iter()
            .filter(|strip| {
                // `ends_with`: AppKit may observe it through a KVO subclass.
                strip
                    .class()
                    .name()
                    .to_bytes()
                    .ends_with(b"NSToolbarFullScreenWindow")
                    && strip.isVisible()
                    && strip.alphaValue() > 0.5
            })
            .map(|strip| {
                let frame = strip.frame();
                let shadow = (frame.size.height - title_bar).max(0.);
                (top - frame.origin.y - shadow).clamp(0., 120.) as f32
            })
            .fold(0., f32::max)
    }

    /// Call when the window moves or resizes; ends a fill the user undid.
    pub fn window_moved(window: &gpui::Window, cx: &mut gpui::App) {
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        let number = ns_window.windowNumber();
        let mut state = get(number);
        if super::bounds_changed(&mut state, frame(ns_window.frame()), window.is_fullscreen()) {
            set(number, state);
            cx.defer(|_| present());
        }
    }

    /// Call when a browser window closes: brings back the menu bar and Dock if it filled.
    pub fn closed(window: &gpui::Window, cx: &mut gpui::App) {
        if let Some(ns_window) = ns_window(window) {
            set(ns_window.windowNumber(), None);
            cx.defer(|_| present());
        }
    }

    pub fn is_filled(window: &gpui::Window) -> bool {
        ns_window(window).is_some_and(|w| get(w.windowNumber()).is_some())
    }

    /// The menu bar and Dock hide while any window fills its screen, and come back when the
    /// pointer reaches their edge, so the menus stay in reach. A page's full screen hides them
    /// outright: Escape leaves it.
    fn present() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let options = if PAGE.get() {
            Options::HideDock | Options::HideMenuBar
        } else if FILLS.with_borrow(Vec::is_empty) {
            Options::Default
        } else {
            Options::AutoHideDock | Options::AutoHideMenuBar
        };
        NSApplication::sharedApplication(mtm).setPresentationOptions(options);
    }

    fn ns_window(window: &gpui::Window) -> Option<Retained<NSWindow>> {
        use objc2_app_kit::NSView;
        use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
        else {
            return None;
        };
        // SAFETY: GPUI hands out its live content view, and we are on the main thread.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        view.window()
    }

    fn frame(rect: NSRect) -> Frame {
        let (origin, size) = (rect.origin, rect.size);
        Frame {
            x: origin.x,
            y: origin.y,
            width: size.width,
            height: size.height,
        }
    }

    fn rect(frame: Frame) -> NSRect {
        NSRect::new(
            NSPoint::new(frame.x, frame.y),
            NSSize::new(frame.width, frame.height),
        )
    }
}
#[cfg(target_os = "macos")]
pub use native::{
    closed, fill_pages, hook_pages, is_filled, leave_page, title_bar_overlap, toggle, window_moved,
};

/// Only native full screen elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn toggle(window: &gpui::Window, _: &gpui::App, _: bool, _: bool) {
    window.toggle_fullscreen()
}
#[cfg(not(target_os = "macos"))]
pub fn window_moved(_: &gpui::Window, _: &mut gpui::App) {}
#[cfg(not(target_os = "macos"))]
pub fn fill_pages(_: bool) {}
#[cfg(not(target_os = "macos"))]
pub fn hook_pages() {}
#[cfg(not(target_os = "macos"))]
pub fn title_bar_overlap(_: &gpui::Window) -> f32 {
    0.
}
#[cfg(not(target_os = "macos"))]
pub fn leave_page() {}
#[cfg(not(target_os = "macos"))]
pub fn closed(_: &gpui::Window, _: &mut gpui::App) {}
#[cfg(not(target_os = "macos"))]
pub fn is_filled(_: &gpui::Window) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{Fill, Frame, PageCall, Reveal, SLIDE, bounds_changed, page_call, restore};
    use std::time::Instant;

    #[test]
    fn a_page_enters_and_leaves_by_the_call_not_by_what_we_remember() {
        // Fill the current desktop: enter fills, exit restores.
        assert_eq!(page_call(false, false, false, true), PageCall::Fill);
        assert_eq!(page_call(false, true, true, true), PageCall::Restore);
        // A missed exit left the window marked as ours: the next enter still fills.
        assert_eq!(page_call(false, false, true, true), PageCall::Fill);
        // An exit for a window we never filled is AppKit's.
        assert_eq!(page_call(false, true, false, true), PageCall::Native);
        // Native full screen setting: AppKit enters; our fill from before the switch restores.
        assert_eq!(page_call(false, false, false, false), PageCall::Native);
        assert_eq!(page_call(false, true, true, false), PageCall::Restore);
        // Already in native full screen: always AppKit.
        assert_eq!(page_call(true, false, false, true), PageCall::Native);
        assert_eq!(page_call(true, true, true, true), PageCall::Native);
    }

    #[test]
    fn the_layout_slides_below_the_title_bar_strip_and_back() {
        let start = Instant::now();
        let mut reveal = Reveal::default();
        assert!(!reveal.set(0., start, true), "nothing shows");
        assert!(reveal.set(44., start, true));
        assert_eq!(reveal.offset(start), 0.);
        let half = reveal.offset(start + SLIDE / 2);
        assert!(half > 22. && half < 44., "{half}");
        assert!(reveal.moving(start + SLIDE / 2));
        assert_eq!(reveal.offset(start + SLIDE), 44.);
        // The strip wobbles while it slides in: the layout doesn't step back.
        assert!(!reveal.set(37., start + SLIDE, true));
        assert!(!reveal.set(44., start + SLIDE, true));
        // Hidden again: back up, from wherever the layout is.
        let later = start + SLIDE * 2;
        assert!(reveal.set(0., later, true));
        assert_eq!(reveal.offset(later), 44.);
        assert_eq!(reveal.offset(later + SLIDE), 0.);
        assert!(!reveal.moving(later + SLIDE));
        // Reduce Motion: no slide; bad readings count as hidden.
        assert!(reveal.set(44., later, false));
        assert_eq!(reveal.offset(later), 44.);
        assert!(reveal.set(f32::NAN, later, false));
        assert_eq!(reveal.offset(later), 0.);
    }

    const WINDOW: Frame = Frame {
        x: 40.,
        y: 100.,
        width: 900.,
        height: 600.,
    };
    const SCREEN: Frame = Frame {
        x: 0.,
        y: 0.,
        width: 1512.,
        height: 950.,
    };

    #[test]
    fn a_second_press_during_the_fill_resize_is_ignored() {
        let resizing = Fill {
            restore: WINDOW,
            filled: None,
        };
        let mut fill = Some(resizing);
        assert_eq!(restore(&mut fill), None);
        assert_eq!(fill, Some(resizing));
        // The resize's own frames aren't the user's.
        assert!(!bounds_changed(&mut fill, WINDOW, false));
        let mut fill = Some(Fill {
            filled: Some(SCREEN),
            ..resizing
        });
        assert_eq!(restore(&mut fill), Some(WINDOW));
        assert_eq!(fill, None);
        assert_eq!(restore(&mut fill), None);
    }

    #[test]
    fn a_move_resize_or_green_button_ends_the_fill() {
        let filled = Some(Fill {
            restore: WINDOW,
            filled: Some(SCREEN),
        });
        // AppKit may round the frame, and other windows becoming key report the same bounds.
        let rounded = Frame {
            height: 949.5,
            ..SCREEN
        };
        let mut fill = filled;
        assert!(!bounds_changed(&mut fill, rounded, false));
        assert_eq!(fill, filled);
        assert!(bounds_changed(&mut fill, Frame { x: 80., ..SCREEN }, false));
        assert_eq!(fill, None);
        assert!(!bounds_changed(&mut fill, WINDOW, false));
        let mut fill = filled;
        assert!(bounds_changed(&mut fill, SCREEN, true));
        assert_eq!(fill, None);
    }
}
