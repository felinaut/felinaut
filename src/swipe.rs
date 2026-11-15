//! Trackpad swipes that go back and forward a page in the current tab, as in Chrome.
//!
//! Which gestures count:
//! - A two-finger horizontal swipe anywhere in the browser window: over a web page, the
//!   sidebar, the toolbar or the New Tab page. Only when macOS "Swipe between pages" includes
//!   two fingers, the setting that AppKit reports as `isSwipeTrackingFromScrollEventsEnabled`.
//! - A three-finger swipe anywhere in the window. AppKit sends it as `NSEventTypeSwipe` only
//!   when "Swipe between pages" includes three fingers.
//!
//! How a two-finger swipe behaves:
//! - Direction follows the scrolled content: deltas that pull content to the right go back.
//!   AppKit applies the scroll direction setting to the deltas, so with natural scrolling
//!   fingers moving right go back and fingers moving left go forward; with natural scrolling
//!   off, both are reversed.
//! - It navigates when the fingers lift, from where they ended up: coming back toward the
//!   start cancels it.
//! - The page comes first: a swipe that scrolled something sideways in the page, or that the
//!   page handled itself (a map, a carousel), does not navigate. Only a swipe the page had no
//!   use for does, such as one at the edge of what scrolls.
//!
//! WebKit's own back/forward swipe stays off: it knows nothing of Home, which is not in its
//! history, and would take a second step for the same swipe.

/// Horizontal travel, in points, that a two-finger swipe needs to go back or forward.
const DISTANCE: f64 = 56.;
/// Vertical travel, in points, after which a gesture that is mostly vertical is a scroll.
const VERTICAL_SLOP: f64 = 10.;
/// How many times larger the horizontal travel must be than the vertical one.
const RATIO: f64 = 2.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Back,
    Forward,
}

/// The scroll phase of a trackpad event, from `NSEvent.phase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Mouse wheels, which have no gesture to follow.
    None,
    MayBegin,
    Began,
    Changed,
    Stationary,
    Ended,
    Cancelled,
}

impl Phase {
    /// Reads `NSEventPhase` bits; AppKit sets exactly one, or none for a mouse wheel.
    pub fn from_appkit(bits: usize) -> Self {
        match bits {
            0x01 => Self::Began,
            0x02 => Self::Stationary,
            0x04 => Self::Changed,
            0x08 => Self::Ended,
            0x10 => Self::Cancelled,
            0x20 => Self::MayBegin,
            _ => Self::None,
        }
    }
}

/// One scroll event, deltas as AppKit reports them (`scrollingDeltaX/Y`).
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub dx: f64,
    pub dy: f64,
    pub phase: Phase,
    /// `momentumPhase` was set: the coasting after the fingers lifted.
    pub momentum: bool,
    /// Whether this gesture may navigate: the user's trackpad settings allow two-finger
    /// swipes. Only read when the gesture begins.
    pub allowed: bool,
}

/// Follows one two-finger gesture at a time and navigates at most once, when it ends.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The travel so far of a gesture that may still become a swipe. `None` between gestures,
    /// and once the current one turned out to be a scroll, moved the page, or was not allowed.
    travel: Option<(f64, f64)>,
}

impl Tracker {
    /// Feeds one event and returns the step it completes, if any.
    pub fn feed(&mut self, sample: Sample) -> Option<Step> {
        if sample.momentum {
            return None;
        }
        match sample.phase {
            Phase::Began => {
                self.travel = sample.allowed.then_some((0., 0.));
                self.track(sample);
                None
            }
            Phase::Changed | Phase::Stationary => {
                self.track(sample);
                None
            }
            // The fingers lifted: where they ended up decides.
            Phase::Ended => self.travel.take().and_then(settle),
            Phase::MayBegin | Phase::Cancelled => {
                self.travel = None;
                None
            }
            Phase::None => None,
        }
    }

    /// The page scrolled sideways or handled the scroll itself, so this gesture is the page's.
    pub fn page_scrolled(&mut self) {
        self.travel = None;
    }

    fn track(&mut self, sample: Sample) {
        let Some((dx, dy)) = self.travel.as_mut() else {
            return;
        };
        *dx += finite(sample.dx);
        *dy += finite(sample.dy);
        // A gesture that turns mostly vertical is a scroll for the rest of it.
        if dy.abs() > VERTICAL_SLOP && dy.abs() >= dx.abs() {
            self.travel = None;
        }
    }
}

/// The step of a finished gesture: far enough sideways, and mostly horizontal.
fn settle((dx, dy): (f64, f64)) -> Option<Step> {
    (dx.abs() >= DISTANCE && dx.abs() >= RATIO * dy.abs()).then(|| direction(dx))
}

fn finite(delta: f64) -> f64 {
    if delta.is_finite() { delta } else { 0. }
}

/// Positive deltas pull content to the right, revealing the page that came before.
fn direction(dx: f64) -> Step {
    if dx > 0. { Step::Back } else { Step::Forward }
}

/// The step of a three-finger `NSEventTypeSwipe`, whose `deltaX` is ±1 for a horizontal swipe
/// and 0 for a vertical one. Positive goes back, as in Chrome.
pub fn swipe_step(dx: f64, dy: f64) -> Option<Step> {
    (dx.is_finite() && dx != 0. && dx.abs() > dy.abs()).then(|| direction(dx))
}

/// The only message `swipe_scroll.js` sends: a sideways scroll was the page's.
pub const PAGE_SCROLLED: &str = "felifox-swipe:scrolled";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(phase: Phase, dx: f64, dy: f64) -> Sample {
        Sample {
            dx,
            dy,
            phase,
            momentum: false,
            allowed: true,
        }
    }

    /// Feeds a whole gesture: Began, the given moves, then Ended. Returns every step it made.
    fn gesture(tracker: &mut Tracker, moves: &[(f64, f64)]) -> Vec<Step> {
        let mut steps = Vec::new();
        steps.extend(tracker.feed(sample(Phase::Began, 0., 0.)));
        for &(dx, dy) in moves {
            steps.extend(tracker.feed(sample(Phase::Changed, dx, dy)));
        }
        steps.extend(tracker.feed(sample(Phase::Ended, 0., 0.)));
        steps
    }

    /// The delta AppKit reports for fingers moving `fingers_dx` points to the right: natural
    /// scrolling reports finger motion as is, the traditional direction inverts it.
    fn reported(fingers_dx: f64, natural: bool) -> f64 {
        if natural { fingers_dx } else { -fingers_dx }
    }

    #[test]
    fn short_swipes_do_nothing_and_the_threshold_navigates() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(20., 0.), (20., 0.), (15., 0.)]).is_empty());
        assert_eq!(
            gesture(&mut tracker, &[(30., 0.), (26., 0.)]),
            [Step::Back],
            "exactly {DISTANCE} points navigates"
        );
    }

    #[test]
    fn it_navigates_once_when_the_fingers_lift() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        for _ in 0..20 {
            assert_eq!(tracker.feed(sample(Phase::Changed, 40., 0.)), None);
        }
        assert_eq!(tracker.feed(sample(Phase::Stationary, 0., 0.)), None);
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), Some(Step::Back));
        // A second end, or stray movement after it, does nothing more.
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
        assert_eq!(tracker.feed(sample(Phase::Changed, 200., 0.)), None);
        // The next gesture navigates again.
        assert_eq!(gesture(&mut tracker, &[(-80., 0.)]), [Step::Forward]);
    }

    #[test]
    fn coming_back_toward_the_start_cancels() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(120., 0.), (-100., 0.)]).is_empty());
        // Past the start and far enough the other way is a swipe that way.
        assert_eq!(
            gesture(&mut tracker, &[(60., 0.), (-130., 0.)]),
            [Step::Forward]
        );
    }

    #[test]
    fn vertical_and_diagonal_scrolls_are_rejected() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(0., -40.), (5., -40.)]).is_empty());
        // A scroll that drifts sideways after it started vertically stays a scroll.
        assert!(gesture(&mut tracker, &[(2., 12.), (100., 0.)]).is_empty());
        // About 40°: far enough sideways, but not mostly horizontal.
        assert!(gesture(&mut tracker, &[(30., 25.), (30., 25.)]).is_empty());
        assert!(gesture(&mut tracker, &[(60., 31.)]).is_empty());
        // A slight wobble is fine.
        assert_eq!(gesture(&mut tracker, &[(60., 8.)]), [Step::Back]);
    }

    #[test]
    fn direction_follows_the_scroll_direction_setting_like_chrome() {
        let mut tracker = Tracker::default();
        let right = |natural| [(reported(70., natural), 0.)];
        let left = |natural| [(reported(-70., natural), 0.)];
        // Natural scrolling: fingers moving right go back.
        assert_eq!(gesture(&mut tracker, &right(true)), [Step::Back]);
        assert_eq!(gesture(&mut tracker, &left(true)), [Step::Forward]);
        // Traditional scrolling reverses both.
        assert_eq!(gesture(&mut tracker, &right(false)), [Step::Forward]);
        assert_eq!(gesture(&mut tracker, &left(false)), [Step::Back]);
        // Three-finger swipes report the same way.
        assert_eq!(swipe_step(1., 0.), Some(Step::Back));
        assert_eq!(swipe_step(-1., 0.), Some(Step::Forward));
    }

    #[test]
    fn three_finger_swipes_need_a_horizontal_delta() {
        for (dx, dy) in [
            (0., 1.),
            (0., -1.),
            (0., 0.),
            (f64::NAN, 0.),
            (f64::INFINITY, 0.),
        ] {
            assert_eq!(swipe_step(dx, dy), None, "{dx} {dy}");
        }
        assert_eq!(swipe_step(1., 0.5), Some(Step::Back));
    }

    #[test]
    fn a_swipe_that_scrolled_the_page_does_not_navigate() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, 80., 0.));
        tracker.page_scrolled();
        // Nor does more travel bring it back.
        tracker.feed(sample(Phase::Changed, 200., 0.));
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
        // A report with no gesture under way, or after one ended, changes nothing.
        tracker.page_scrolled();
        assert_eq!(gesture(&mut tracker, &[(80., 0.)]), [Step::Back]);
        tracker.page_scrolled();
        assert_eq!(gesture(&mut tracker, &[(80., 0.)]), [Step::Back]);
    }

    #[test]
    fn momentum_events_never_navigate() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, 20., 0.));
        // The deltas of the ending event itself do not count.
        assert_eq!(tracker.feed(sample(Phase::Ended, 200., 0.)), None);
        for phase in [Phase::None, Phase::Changed, Phase::Ended] {
            let coasting = Sample {
                momentum: true,
                ..sample(phase, 200., 0.)
            };
            assert_eq!(tracker.feed(coasting), None);
        }
        // Momentum does not end or add to a gesture under way either.
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, 30., 0.));
        let coasting = Sample {
            momentum: true,
            ..sample(Phase::Ended, 500., 0.)
        };
        assert_eq!(tracker.feed(coasting), None);
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
        // Mouse wheels have no phase and never navigate.
        assert_eq!(tracker.feed(sample(Phase::None, 500., 0.)), None);
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
    }

    #[test]
    fn cancelled_gestures_start_over() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, 150., 0.));
        assert_eq!(tracker.feed(sample(Phase::Cancelled, 0., 0.)), None);
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
        // The travel of a cancelled gesture does not count toward the next.
        assert!(gesture(&mut tracker, &[(10., 0.), (40., 0.)]).is_empty());
        // MayBegin (fingers resting) also clears a gesture whose end was missed.
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, 150., 0.));
        tracker.feed(sample(Phase::MayBegin, 0., 0.));
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
    }

    #[test]
    fn gestures_with_swipes_disabled_are_ignored() {
        let mut tracker = Tracker::default();
        let not_allowed = Sample {
            allowed: false,
            ..sample(Phase::Began, 100., 0.)
        };
        assert_eq!(tracker.feed(not_allowed), None);
        // Later events of that gesture cannot claim it, even if they are flagged allowed.
        tracker.feed(sample(Phase::Changed, 100., 0.));
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), None);
        // A gesture that begins already past the threshold still navigates once.
        tracker.feed(sample(Phase::Began, 100., 0.));
        assert_eq!(tracker.feed(sample(Phase::Ended, 0., 0.)), Some(Step::Back));
    }

    #[test]
    fn non_finite_deltas_are_ignored() {
        let mut tracker = Tracker::default();
        let moves = [(f64::NAN, 0.), (f64::INFINITY, f64::NAN), (60., 0.)];
        assert_eq!(gesture(&mut tracker, &moves), [Step::Back]);
    }

    #[test]
    fn phases_map_from_appkit_bits() {
        assert_eq!(Phase::from_appkit(0), Phase::None);
        assert_eq!(Phase::from_appkit(1), Phase::Began);
        assert_eq!(Phase::from_appkit(2), Phase::Stationary);
        assert_eq!(Phase::from_appkit(4), Phase::Changed);
        assert_eq!(Phase::from_appkit(8), Phase::Ended);
        assert_eq!(Phase::from_appkit(16), Phase::Cancelled);
        assert_eq!(Phase::from_appkit(32), Phase::MayBegin);
        assert_eq!(Phase::from_appkit(3), Phase::None);
        assert_eq!(Phase::from_appkit(64), Phase::None);
    }
}
