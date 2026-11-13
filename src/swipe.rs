//! Trackpad swipes that switch tabs, like the sidebar swipe of Arc and the three-finger tab
//! swipe of ChromeOS.
//!
//! Which gestures count:
//! - A two-finger horizontal swipe over the browser chrome: the sidebar, the toolbar, and the
//!   New Tab page. Only when macOS "Swipe between pages" includes two fingers, the setting that
//!   AppKit reports as `isSwipeTrackingFromScrollEventsEnabled`.
//! - A three-finger swipe anywhere in the window. AppKit sends it as `NSEventTypeSwipe` only
//!   when "Swipe between pages" includes three fingers; pages have no use for it because
//!   Felinaut leaves WebKit's back/forward swipe navigation off.
//!
//! A two-finger swipe over a web page always stays the page's, for horizontal scrolling (and
//! WebKit's back/forward swipe, should it be turned on). The pointer's position when the
//! fingers start moving decides, so a gesture that wanders onto the page keeps its owner.
//!
//! AppKit applies the scroll direction setting to both kinds of deltas, so the tab strip moves
//! like content that is scrolled: with natural scrolling, fingers moving left bring in the next
//! tab, as turning a page does, and fingers moving right the previous one; with natural
//! scrolling off, both are reversed, matching how that setting scrolls everything else.

use std::time::Duration;

/// Horizontal travel, in points, before a two-finger swipe switches tabs.
const DISTANCE: f64 = 56.;
/// Vertical travel, in points, after which a gesture that is mostly vertical is a scroll.
const VERTICAL_SLOP: f64 = 10.;
/// How many times larger the horizontal travel must be than the vertical one.
const RATIO: f64 = 2.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Next,
    Previous,
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
    /// Whether this gesture may switch tabs: it started over the chrome, and the user's
    /// trackpad settings allow two-finger swipes. Only read when the gesture begins.
    pub allowed: bool,
}

/// Follows one two-finger gesture at a time and switches at most once per gesture.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The travel so far of a gesture that may still become a swipe. `None` between gestures,
    /// and once the current one switched a tab, turned out to be a scroll, or was not allowed.
    travel: Option<(f64, f64)>,
}

impl Tracker {
    /// Feeds one event and returns the tab step it completes, if any.
    pub fn feed(&mut self, sample: Sample) -> Option<Step> {
        if sample.momentum {
            return None;
        }
        match sample.phase {
            Phase::Began => {
                self.travel = sample.allowed.then_some((0., 0.));
                self.track(sample)
            }
            Phase::Changed | Phase::Stationary => self.track(sample),
            Phase::MayBegin | Phase::Ended | Phase::Cancelled => {
                self.travel = None;
                None
            }
            Phase::None => None,
        }
    }

    fn track(&mut self, sample: Sample) -> Option<Step> {
        let (dx, dy) = self.travel.as_mut()?;
        *dx += finite(sample.dx);
        *dy += finite(sample.dy);
        let (x, y) = (dx.abs(), dy.abs());
        if y > VERTICAL_SLOP && y >= x {
            self.travel = None;
            return None;
        }
        if x < DISTANCE {
            return None;
        }
        let step = (x >= RATIO * y).then(|| direction(*dx));
        // Settled either way: a diagonal gesture is not retried as it straightens out.
        self.travel = None;
        step
    }
}

fn finite(delta: f64) -> f64 {
    if delta.is_finite() { delta } else { 0. }
}

/// Positive deltas pull content to the right, revealing what comes before.
fn direction(dx: f64) -> Step {
    if dx > 0. { Step::Previous } else { Step::Next }
}

/// The step of a three-finger `NSEventTypeSwipe`, whose `deltaX` is ±1 for a horizontal swipe
/// and 0 for a vertical one.
pub fn swipe_step(dx: f64, dy: f64) -> Option<Step> {
    (dx.is_finite() && dx != 0. && dx.abs() > dy.abs()).then(|| direction(dx))
}

/// The tab a step moves to from `active` among `count` tabs, wrapping at either end like
/// ⌃Tab. `None` with only one tab, so the gesture has nothing to do or show.
pub fn adjacent(active: usize, count: usize, step: Step) -> Option<usize> {
    if count < 2 || active >= count {
        return None;
    }
    Some(match step {
        Step::Next => (active + 1) % count,
        Step::Previous => (active + count - 1) % count,
    })
}

/// How long the newly shown tab's row slides into place after a swipe.
pub const NUDGE: Duration = Duration::from_millis(180);
/// How far, in points, the row starts from its place.
const NUDGE_DISTANCE: f32 = 6.;

/// The row's horizontal offset `elapsed` after a swipe, or `None` once it has settled. The next
/// tab's row comes in from the right and the previous one's from the left, following the
/// fingers with natural scrolling like the page they would turn.
pub fn nudge(step: Step, elapsed: Duration) -> Option<f32> {
    let t = elapsed.as_secs_f32() / NUDGE.as_secs_f32();
    if t >= 1. {
        return None;
    }
    let remaining = (1. - t).powi(3);
    let side = match step {
        Step::Next => 1.,
        Step::Previous => -1.,
    };
    Some(side * NUDGE_DISTANCE * remaining)
}

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

    /// The deltas AppKit reports for fingers moving `fingers_dx` points to the right: natural
    /// scrolling reports finger motion as is, the traditional direction inverts it.
    fn reported(fingers_dx: f64, natural: bool) -> f64 {
        if natural { fingers_dx } else { -fingers_dx }
    }

    #[test]
    fn short_swipes_do_nothing_and_the_threshold_switches() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(-20.), (-20.), (-15.)].map(|x| (x, 0.))).is_empty());
        assert_eq!(
            gesture(&mut tracker, &[(-30., 0.), (-26., 0.)]),
            [Step::Next],
            "exactly {DISTANCE} points switches"
        );
    }

    #[test]
    fn one_switch_per_gesture_while_the_fingers_stay_down() {
        let mut tracker = Tracker::default();
        let moves = [(-40., 0.); 20];
        assert_eq!(gesture(&mut tracker, &moves), [Step::Next]);
        // Coming back does not undo or repeat it either.
        let mut back_and_forth = vec![(-60., 0.)];
        back_and_forth.extend([(60., 0.); 5]);
        assert_eq!(gesture(&mut tracker, &back_and_forth), [Step::Next]);
        // The next gesture switches again.
        assert_eq!(gesture(&mut tracker, &[(80., 0.)]), [Step::Previous]);
    }

    #[test]
    fn vertical_and_diagonal_scrolls_are_rejected() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(0., -40.), (-5., -40.)]).is_empty());
        // A scroll that drifts sideways after it started vertically stays a scroll.
        assert!(gesture(&mut tracker, &[(-2., 12.), (-100., 0.)]).is_empty());
        // About 40°: far enough sideways, but not mostly horizontal.
        assert!(gesture(&mut tracker, &[(-30., 25.), (-30., 25.)]).is_empty());
        assert!(gesture(&mut tracker, &[(-60., 31.)]).is_empty());
        // A diagonal that straightens out later is not retried.
        assert!(gesture(&mut tracker, &[(-60., 31.), (-200., 0.)]).is_empty());
        // A slight wobble is fine.
        assert_eq!(gesture(&mut tracker, &[(-60., 8.)]), [Step::Next]);
    }

    #[test]
    fn direction_follows_natural_scrolling() {
        let mut tracker = Tracker::default();
        let left = |natural| [(reported(-70., natural), 0.)];
        let right = |natural| [(reported(70., natural), 0.)];
        assert_eq!(gesture(&mut tracker, &left(true)), [Step::Next]);
        assert_eq!(gesture(&mut tracker, &right(true)), [Step::Previous]);
        assert_eq!(gesture(&mut tracker, &left(false)), [Step::Previous]);
        assert_eq!(gesture(&mut tracker, &right(false)), [Step::Next]);
        // Three-finger swipes report the same way.
        assert_eq!(swipe_step(reported(-1., true), 0.), Some(Step::Next));
        assert_eq!(swipe_step(reported(1., true), 0.), Some(Step::Previous));
        assert_eq!(swipe_step(reported(-1., false), 0.), Some(Step::Previous));
        assert_eq!(swipe_step(reported(1., false), 0.), Some(Step::Next));
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
        assert_eq!(swipe_step(-1., 0.5), Some(Step::Next));
    }

    #[test]
    fn momentum_and_ended_events_never_switch() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, -20., 0.));
        assert_eq!(tracker.feed(sample(Phase::Ended, -200., 0.)), None);
        for phase in [Phase::None, Phase::Changed, Phase::Ended] {
            let coasting = Sample {
                momentum: true,
                ..sample(phase, -200., 0.)
            };
            assert_eq!(tracker.feed(coasting), None);
        }
        // After lifting the fingers, nothing continues the old gesture.
        assert_eq!(tracker.feed(sample(Phase::Changed, -200., 0.)), None);
        // Mouse wheels have no phase and never switch.
        assert_eq!(tracker.feed(sample(Phase::None, -500., 0.)), None);
    }

    #[test]
    fn cancelled_gestures_start_over() {
        let mut tracker = Tracker::default();
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, -50., 0.));
        assert_eq!(tracker.feed(sample(Phase::Cancelled, 0., 0.)), None);
        assert_eq!(tracker.feed(sample(Phase::Changed, -50., 0.)), None);
        // The travel of a cancelled gesture does not count toward the next.
        tracker.feed(sample(Phase::Began, 0., 0.));
        assert_eq!(tracker.feed(sample(Phase::Changed, -10., 0.)), None);
        assert_eq!(
            tracker.feed(sample(Phase::Changed, -46., 0.)),
            Some(Step::Next)
        );
        // MayBegin (fingers resting) also clears a gesture whose end was missed.
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, -50., 0.));
        tracker.feed(sample(Phase::MayBegin, 0., 0.));
        assert_eq!(tracker.feed(sample(Phase::Changed, -50., 0.)), None);
    }

    #[test]
    fn gestures_over_pages_or_with_swipes_disabled_are_ignored() {
        let mut tracker = Tracker::default();
        let not_allowed = Sample {
            allowed: false,
            ..sample(Phase::Began, -100., 0.)
        };
        assert_eq!(tracker.feed(not_allowed), None);
        // Later events of that gesture cannot claim it, even if they are flagged allowed.
        assert_eq!(tracker.feed(sample(Phase::Changed, -100., 0.)), None);
        // A gesture that begins already past the threshold still switches once.
        assert_eq!(
            tracker.feed(sample(Phase::Began, -100., 0.)),
            Some(Step::Next)
        );
        assert_eq!(tracker.feed(sample(Phase::Changed, -100., 0.)), None);
    }

    #[test]
    fn non_finite_deltas_are_ignored() {
        let mut tracker = Tracker::default();
        let moves = [(f64::NAN, 0.), (f64::INFINITY, f64::NAN), (-60., 0.)];
        assert_eq!(gesture(&mut tracker, &moves), [Step::Next]);
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

    #[test]
    fn steps_wrap_at_the_first_and_last_tab() {
        assert_eq!(adjacent(0, 3, Step::Next), Some(1));
        assert_eq!(adjacent(2, 3, Step::Next), Some(0));
        assert_eq!(adjacent(0, 3, Step::Previous), Some(2));
        assert_eq!(adjacent(2, 3, Step::Previous), Some(1));
        assert_eq!(adjacent(1, 2, Step::Next), Some(0));
    }

    #[test]
    fn a_single_tab_or_no_tab_has_nowhere_to_go() {
        for step in [Step::Next, Step::Previous] {
            assert_eq!(adjacent(0, 1, step), None);
            assert_eq!(adjacent(0, 0, step), None);
            assert_eq!(adjacent(3, 3, step), None, "out of range");
        }
    }

    #[test]
    fn the_row_slides_in_from_the_steps_side_and_settles() {
        let start = nudge(Step::Next, Duration::ZERO).unwrap();
        assert!(start > 5.9, "{start}");
        assert!(nudge(Step::Previous, Duration::ZERO).unwrap() < -5.9);
        let middle = nudge(Step::Next, NUDGE / 2).unwrap();
        assert!(0. < middle && middle < start, "{middle}");
        assert_eq!(nudge(Step::Next, NUDGE), None);
        assert_eq!(nudge(Step::Previous, NUDGE * 3), None);
    }
}
