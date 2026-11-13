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
                });
                Toggle::Fill {
                    frame: frame.inset(request.safe_area),
                    animate: request.animate,
                }
            }
            _ => Toggle::Native,
        }
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
    use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions as Options, NSWindow};
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
            safe_area: screen.map_or_else(Insets::default, |screen| {
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
        let (frame, animate) = match FILLS.with_borrow_mut(|fills| fills.toggle(request)) {
            Toggle::Native => return window.toggle_fullscreen(),
            Toggle::Fill { frame, animate } | Toggle::Restore { frame, animate } => {
                (frame, animate)
            }
        };
        // Resize after this action: AppKit's resize callbacks update the GPUI window.
        cx.foreground_executor()
            .spawn(async move {
                present();
                ns_window.setFrame_display_animate(rect(frame), true, animate);
            })
            .detach();
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
pub use native::{closed, is_filled, toggle};

/// Only native full screen elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn toggle(window: &gpui::Window, _: &gpui::App, _: bool, _: bool) {
    window.toggle_fullscreen()
}
#[cfg(not(target_os = "macos"))]
pub fn closed(_: &gpui::Window) {}
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
