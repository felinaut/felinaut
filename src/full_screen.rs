//! ⌃⌘F: native full screen in a new Space, or filling the current desktop. The pure model
//! keeps, per window number, the frame each filled window returns to, and says when the menu
//! bar and Dock hide; the AppKit glue at the bottom applies its decisions.

/// A window or screen rectangle in AppKit's screen coordinates (points, bottom-left origin).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Edges of a screen that windows should stay out of, such as the camera housing of a
/// notched MacBook: AppKit's `safeAreaInsets`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

impl Frame {
    /// The frame less `insets`, never narrower or lower than nothing. The origin is at the
    /// bottom left, so the top inset only takes height.
    pub fn inset(self, insets: Insets) -> Frame {
        let edge = |inset: f64| if inset.is_finite() { inset.max(0.) } else { 0. };
        let (top, left, bottom, right) = (
            edge(insets.top),
            edge(insets.left),
            edge(insets.bottom),
            edge(insets.right),
        );
        Frame {
            x: self.x + left,
            y: self.y + bottom,
            width: (self.width - left - right).max(0.),
            height: (self.height - top - bottom).max(0.),
        }
    }
}

/// What ⌃⌘F does to a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Toggle {
    /// Enter or leave native full screen.
    Native,
    /// Resize the window to fill the screen.
    Fill { frame: Frame, animate: bool },
    /// Resize the window back to its frame from before it filled the screen.
    Restore { frame: Frame, animate: bool },
}

/// One ⌃⌘F in a window.
#[derive(Clone, Copy, Debug)]
pub struct Request {
    /// The window's number, unique among the app's open windows.
    pub window: isize,
    /// The window is in native full screen.
    pub native: bool,
    /// Settings choose "Fill the current desktop".
    pub fill: bool,
    /// The window's frame now.
    pub frame: Frame,
    /// The window's screen, or `None` without a screen.
    pub screen: Option<Frame>,
    /// The screen's safe area: the window fills the screen less these.
    pub safe_area: Insets,
    pub animate: bool,
}

#[derive(Debug)]
struct Filled {
    window: isize,
    restore: Frame,
    animate: bool,
    /// The frame AppKit gave the window once the fill resize ended; `None` while it runs.
    filled: Option<Frame>,
}

/// AppKit's frames are whole points or halves; anything more is a move or resize.
const SAME_FRAME: f64 = 0.5;

/// How much of a restored window must show on some screen for its title bar to be reachable.
const REACHABLE: f64 = 40.;

impl Frame {
    /// The frame, or one of its size centred on the first of `screens` (the window's own) and
    /// shrunk to fit, if no screen shows enough of it: its display may be gone since the fill.
    pub fn kept_on(self, screens: &[Frame]) -> Frame {
        let reachable = |screen: &Frame| {
            let width = (self.x + self.width).min(screen.x + screen.width) - self.x.max(screen.x);
            let height =
                (self.y + self.height).min(screen.y + screen.height) - self.y.max(screen.y);
            width >= REACHABLE.min(self.width) && height >= REACHABLE.min(self.height)
        };
        match screens.first() {
            Some(home) if !screens.iter().any(reachable) => {
                let (width, height) = (self.width.min(home.width), self.height.min(home.height));
                Frame {
                    x: home.x + (home.width - width) / 2.,
                    y: home.y + (home.height - height) / 2.,
                    width,
                    height,
                }
            }
            _ => self,
        }
    }

    fn near(self, other: Frame) -> bool {
        [
            self.x - other.x,
            self.y - other.y,
            self.width - other.width,
            self.height - other.height,
        ]
        .iter()
        .all(|difference| difference.abs() <= SAME_FRAME)
    }
}

/// The windows that fill their screen without a new Space.
#[derive(Debug, Default)]
pub struct Fills {
    windows: Vec<Filled>,
}

impl Fills {
    /// Decides ⌃⌘F. A filled window always goes back to its frame, whatever Settings say now;
    /// any other window fills its screen when Settings ask for it, independently of the others.
    pub fn toggle(&mut self, request: Request) -> Toggle {
        if let Some(index) = self.index(request.window) {
            let filled = self.windows.remove(index);
            return Toggle::Restore {
                frame: filled.restore,
                animate: filled.animate,
            };
        }
        match request.screen {
            Some(frame) if request.fill && !request.native => {
                self.windows.push(Filled {
                    window: request.window,
                    restore: request.frame,
                    animate: request.animate,
                    filled: None,
                });
                Toggle::Fill {
                    frame: frame.inset(request.safe_area),
                    animate: request.animate,
                }
            }
            _ => Toggle::Native,
        }
    }

    /// Records the frame a window has once its fill resize ends.
    pub fn settled(&mut self, window: isize, frame: Frame) {
        if let Some(index) = self.index(window) {
            self.windows[index].filled = Some(frame);
        }
    }

    /// Forgets a filled window that the user moved, resized or put in native full screen with
    /// the green button, so the menu bar and Dock come back and the next ⌃⌘F fills again
    /// rather than jumping to an old frame. True if it forgot the window. Changes during the
    /// fill resize itself are ignored.
    pub fn bounds_changed(&mut self, window: isize, frame: Frame, native: bool) -> bool {
        let Some(index) = self.index(window) else {
            return false;
        };
        let stale = match self.windows[index].filled {
            _ if native => true,
            Some(filled) => !filled.near(frame),
            None => false,
        };
        if stale {
            self.windows.remove(index);
        }
        stale
    }

    /// Forgets a window that closes; true if it filled its screen.
    pub fn closed(&mut self, window: isize) -> bool {
        self.index(window)
            .map(|index| self.windows.remove(index))
            .is_some()
    }

    pub fn is_filled(&self, window: isize) -> bool {
        self.index(window).is_some()
    }

    /// The menu bar and Dock hide while any window fills its screen.
    pub fn hides_menu_bar(&self) -> bool {
        !self.windows.is_empty()
    }

    fn index(&self, window: isize) -> Option<usize> {
        self.windows
            .iter()
            .position(|filled| filled.window == window)
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::{Fills, Frame, Insets, Request, Toggle};
    use objc2::{MainThreadMarker, rc::Retained};
    use objc2_app_kit::{
        NSApplication, NSApplicationPresentationOptions as Options, NSScreen, NSWindow,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use std::cell::RefCell;

    thread_local! {
        /// AppKit windows live on the main thread, so the state does too.
        static FILLS: RefCell<Fills> = RefCell::default();
    }

    /// ⌃⌘F in `window`: native full screen, or filling the current desktop when `fill`.
    pub fn toggle(window: &gpui::Window, cx: &gpui::App, fill: bool, animate: bool) {
        let Some(ns_window) = ns_window(window) else {
            return window.toggle_fullscreen();
        };
        let screen = ns_window.screen();
        let request = Request {
            window: ns_window.windowNumber(),
            native: window.is_fullscreen(),
            fill,
            frame: frame(ns_window.frame()),
            screen: screen.as_ref().map(|screen| frame(screen.frame())),
            // macOS 12 and later, the deployment target. With the menu bar hidden, the top
            // inset keeps the toolbar and traffic lights below a MacBook's camera housing.
            safe_area: screen.as_ref().map_or_else(Insets::default, |screen| {
                let insets = screen.safeAreaInsets();
                Insets {
                    top: insets.top,
                    left: insets.left,
                    bottom: insets.bottom,
                    right: insets.right,
                }
            }),
            animate,
        };
        let (target, animate, fill) = match FILLS.with_borrow_mut(|fills| fills.toggle(request)) {
            Toggle::Native => return window.toggle_fullscreen(),
            Toggle::Fill { frame, animate } => (frame, animate, true),
            Toggle::Restore {
                frame: restore,
                animate,
            } => {
                let mut screens = Vec::from_iter(screen.as_ref().map(|s| frame(s.visibleFrame())));
                if let Some(mtm) = MainThreadMarker::new() {
                    let all = NSScreen::screens(mtm).to_vec();
                    screens.extend(all.iter().map(|screen| frame(screen.visibleFrame())));
                }
                (restore.kept_on(&screens), animate, false)
            }
        };
        let number = request.window;
        // Resize after this action: AppKit's resize callbacks update the GPUI window. An
        // animated resize returns when the animation ends; no borrow is held across it, since
        // its callbacks reach `bounds_changed`.
        cx.foreground_executor()
            .spawn(async move {
                present();
                ns_window.setFrame_display_animate(rect(target), true, animate);
                if fill {
                    let settled = frame(ns_window.frame());
                    FILLS.with_borrow_mut(|fills| fills.settled(number, settled));
                }
            })
            .detach();
    }

    /// Call when the window moves or resizes: a filled window the user moved, resized or put
    /// in native full screen no longer fills the screen.
    pub fn bounds_changed(window: &gpui::Window) {
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        let (number, now) = (ns_window.windowNumber(), frame(ns_window.frame()));
        let native = window.is_fullscreen();
        if FILLS.with_borrow_mut(|fills| fills.bounds_changed(number, now, native)) {
            present();
        }
    }

    /// Forgets a closing window, bringing back the menu bar and Dock if it filled the screen.
    pub fn closed(window: &gpui::Window) {
        if let Some(ns_window) = ns_window(window)
            && FILLS.with_borrow_mut(|fills| fills.closed(ns_window.windowNumber()))
        {
            present();
        }
    }

    /// Whether `window` fills its screen without a new Space.
    pub fn is_filled(window: &gpui::Window) -> bool {
        ns_window(window).is_some_and(|ns_window| {
            FILLS.with_borrow(|fills| fills.is_filled(ns_window.windowNumber()))
        })
    }

    /// Hides the menu bar and Dock only while a window fills its screen.
    fn present() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let options = if FILLS.with_borrow(Fills::hides_menu_bar) {
            Options::HideDock | Options::HideMenuBar
        } else {
            Options::Default
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
        Frame {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
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
pub use native::{bounds_changed, closed, is_filled, toggle};

/// Only native full screen elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn toggle(window: &gpui::Window, _: &gpui::App, _: bool, _: bool) {
    window.toggle_fullscreen()
}
#[cfg(not(target_os = "macos"))]
pub fn closed(_: &gpui::Window) {}
#[cfg(not(target_os = "macos"))]
pub fn bounds_changed(_: &gpui::Window) {}
#[cfg(not(target_os = "macos"))]
pub fn is_filled(_: &gpui::Window) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{Fills, Frame, Insets, Request, Toggle};

    const SCREEN: Frame = Frame {
        x: 0.,
        y: 0.,
        width: 1512.,
        height: 982.,
    };

    fn window(x: f64) -> Frame {
        Frame {
            x,
            y: 100.,
            width: 900.,
            height: 600.,
        }
    }

    fn press(window: isize, frame: Frame) -> Request {
        Request {
            window,
            native: false,
            fill: true,
            frame,
            screen: Some(SCREEN),
            safe_area: Insets::default(),
            animate: true,
        }
    }

    #[test]
    fn a_move_or_resize_after_the_fill_forgets_it() {
        let mut fills = Fills::default();
        fills.toggle(press(1, window(40.)));
        // The animated fill resize passes through other frames; they aren't the user's.
        assert!(!fills.bounds_changed(1, window(200.), false));
        assert!(fills.is_filled(1));
        // AppKit may round the frame it gives.
        let given = Frame {
            height: 981.5,
            ..SCREEN
        };
        fills.settled(1, given);
        assert!(!fills.bounds_changed(1, given, false));
        assert!(!fills.bounds_changed(1, SCREEN, false));
        // GPUI also reports bounds when another window becomes key; the frame is the same.
        assert!(!fills.bounds_changed(2, window(500.), false));
        assert!(fills.is_filled(1) && fills.hides_menu_bar());
        // A drag by the title bar.
        let dragged = Frame { x: 80., ..given };
        assert!(fills.bounds_changed(1, dragged, false));
        assert!(!fills.is_filled(1) && !fills.hides_menu_bar());
        assert!(!fills.bounds_changed(1, window(40.), false));
        // The next ⌃⌘F fills again instead of jumping to the frame from before the drag.
        assert!(matches!(
            fills.toggle(press(1, dragged)),
            Toggle::Fill { .. }
        ));
        fills.settled(1, SCREEN);
        let smaller = Frame {
            width: 1200.,
            ..SCREEN
        };
        assert!(fills.bounds_changed(1, smaller, false));
        assert!(!fills.is_filled(1));
    }

    #[test]
    fn native_full_screen_on_top_of_fill_forgets_it() {
        let mut fills = Fills::default();
        fills.toggle(press(1, window(40.)));
        fills.toggle(press(2, window(500.)));
        fills.settled(1, SCREEN);
        // The green button, before the frame even changes.
        assert!(fills.bounds_changed(1, SCREEN, true));
        assert!(!fills.is_filled(1));
        // Also while the fill resize still runs.
        assert!(fills.bounds_changed(2, window(500.), true));
        assert!(!fills.hides_menu_bar());
        // A settle that comes after the window was forgotten doesn't bring it back.
        fills.settled(1, SCREEN);
        assert!(!fills.is_filled(1));
    }

    #[test]
    fn a_restored_frame_stays_on_a_connected_screen() {
        let side = Frame {
            x: 1512.,
            y: 0.,
            width: 1920.,
            height: 1080.,
        };
        let on_side = Frame {
            x: 1700.,
            y: 300.,
            width: 1200.,
            height: 700.,
        };
        // Both screens are there, or the window's own: the frame stays as it was.
        assert_eq!(on_side.kept_on(&[SCREEN, side]), on_side);
        assert_eq!(window(40.).kept_on(&[SCREEN]), window(40.));
        assert_eq!(on_side.kept_on(&[]), on_side);
        // The side display is gone: the window comes back centred on its own screen.
        assert_eq!(
            on_side.kept_on(&[SCREEN]),
            Frame {
                x: 156.,
                y: 141.,
                width: 1200.,
                height: 700.,
            }
        );
        // A frame bigger than the remaining screen shrinks to it.
        let huge = Frame { x: 1600., ..side };
        assert_eq!(huge.kept_on(&[SCREEN]), SCREEN);
        // A sliver along the edge isn't enough to grab the title bar.
        let sliver = Frame {
            x: SCREEN.width - 10.,
            ..window(0.)
        };
        assert_ne!(sliver.kept_on(&[SCREEN]), sliver);
        let reachable = Frame {
            x: SCREEN.width - 40.,
            ..window(0.)
        };
        assert_eq!(reachable.kept_on(&[SCREEN]), reachable);
    }

    #[test]
    fn fill_stays_below_the_camera_housing() {
        let mut fills = Fills::default();
        let notched = Request {
            safe_area: Insets {
                top: 32.,
                ..Insets::default()
            },
            ..press(1, window(40.))
        };
        assert_eq!(
            fills.toggle(notched),
            Toggle::Fill {
                frame: Frame {
                    height: 950.,
                    ..SCREEN
                },
                animate: true
            }
        );
        // Restoring ignores the insets: the window gets back exactly the frame it had.
        assert_eq!(
            fills.toggle(press(1, SCREEN)),
            Toggle::Restore {
                frame: window(40.),
                animate: true
            }
        );
    }

    #[test]
    fn insets_move_the_origin_and_never_go_negative() {
        let screen = Frame {
            x: -1512.,
            y: 200.,
            width: 1512.,
            height: 982.,
        };
        let insets = Insets {
            top: 32.,
            left: 10.,
            bottom: 5.,
            right: 20.,
        };
        assert_eq!(
            screen.inset(insets),
            Frame {
                x: -1502.,
                y: 205.,
                width: 1482.,
                height: 945.,
            }
        );
        assert_eq!(screen.inset(Insets::default()), screen);
        // Nonsense from the system can't turn the window inside out or move it off the screen.
        let wild = Insets {
            top: f64::NAN,
            left: -40.,
            bottom: f64::INFINITY,
            right: 4000.,
        };
        assert_eq!(
            screen.inset(wild),
            Frame {
                width: 0.,
                ..screen
            }
        );
    }

    #[test]
    fn fill_then_restore_the_same_window() {
        let mut fills = Fills::default();
        assert!(!fills.hides_menu_bar());
        assert_eq!(
            fills.toggle(press(7, window(40.))),
            Toggle::Fill {
                frame: SCREEN,
                animate: true
            }
        );
        assert!(fills.is_filled(7) && fills.hides_menu_bar());
        // The second ⌃⌘F gets the frame from before, not the screen's.
        assert_eq!(
            fills.toggle(press(7, SCREEN)),
            Toggle::Restore {
                frame: window(40.),
                animate: true
            }
        );
        assert!(!fills.is_filled(7) && !fills.hides_menu_bar());
    }

    #[test]
    fn a_fill_in_one_window_never_touches_another() {
        let mut fills = Fills::default();
        fills.toggle(press(1, window(40.)));
        // A window that doesn't fill the screen, such as About, has nothing to restore.
        assert!(!fills.is_filled(2));
        assert!(!fills.closed(2));
        assert!(fills.is_filled(1) && fills.hides_menu_bar());
        // Another browser window fills its screen too, rather than falling back to native.
        assert_eq!(
            fills.toggle(press(2, window(500.))),
            Toggle::Fill {
                frame: SCREEN,
                animate: true
            }
        );
        assert_eq!(
            fills.toggle(press(1, SCREEN)),
            Toggle::Restore {
                frame: window(40.),
                animate: true
            }
        );
        // The menu bar stays hidden for the window that still fills the screen.
        assert!(fills.is_filled(2) && fills.hides_menu_bar());
        assert_eq!(
            fills.toggle(press(2, SCREEN)),
            Toggle::Restore {
                frame: window(500.),
                animate: true
            }
        );
        assert!(!fills.hides_menu_bar());
    }

    #[test]
    fn closing_a_filled_window_brings_back_the_menu_bar() {
        let mut fills = Fills::default();
        fills.toggle(press(1, window(40.)));
        fills.toggle(press(2, window(500.)));
        assert!(fills.closed(1));
        assert!(!fills.is_filled(1) && fills.hides_menu_bar());
        assert!(!fills.closed(1));
        assert!(fills.closed(2));
        assert!(!fills.hides_menu_bar());
        // A new window that gets the closed one's number starts unfilled.
        assert!(matches!(
            fills.toggle(press(1, window(40.))),
            Toggle::Fill { .. }
        ));
    }

    #[test]
    fn switching_the_setting_while_filled_still_restores() {
        let mut fills = Fills::default();
        let unanimated = Request {
            animate: false,
            ..press(1, window(40.))
        };
        fills.toggle(unanimated);
        // Native is chosen now, and motion is allowed again: the window still goes back, as
        // it came, rather than entering native full screen from the filled frame.
        let native = Request {
            fill: false,
            ..press(1, SCREEN)
        };
        assert_eq!(
            fills.toggle(native),
            Toggle::Restore {
                frame: window(40.),
                animate: false
            }
        );
        assert_eq!(fills.toggle(native), Toggle::Native);
        assert!(!fills.hides_menu_bar());
    }

    #[test]
    fn native_full_screen_and_missing_screens_stay_native() {
        let mut fills = Fills::default();
        // Choosing fill while in native full screen leaves native full screen first.
        let in_native = Request {
            native: true,
            ..press(1, SCREEN)
        };
        assert_eq!(fills.toggle(in_native), Toggle::Native);
        let no_screen = Request {
            screen: None,
            ..press(1, window(40.))
        };
        assert_eq!(fills.toggle(no_screen), Toggle::Native);
        assert!(!fills.is_filled(1) && !fills.hides_menu_bar());
    }
}
