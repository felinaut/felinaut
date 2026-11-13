//! Trackpad swipes that go back and forward a page in the current tab: two fingers moving left
//! go back, moving right go forward.
//!
//! Which gestures count:
//! - A two-finger horizontal swipe anywhere in the browser window: over a web page, the
//!   sidebar, the toolbar or the New Tab page. Only when macOS "Swipe between pages" includes
//!   two fingers, the setting that AppKit reports as `isSwipeTrackingFromScrollEventsEnabled`.
//!   WebKit's own back/forward swipe stays off: it goes back for fingers moving right.
//! - A three-finger swipe anywhere in the window. AppKit sends it as `NSEventTypeSwipe` only
//!   when "Swipe between pages" includes three fingers.
//!
//! The direction is the fingers', whatever the scroll direction setting: AppKit reports deltas
//! after applying it, so they are turned back into finger motion first.

/// Horizontal travel, in points, before a two-finger swipe goes back or forward.
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
    /// Natural scrolling is on (`isDirectionInvertedFromDevice`): deltas follow the fingers.
    pub natural: bool,
    /// Whether this gesture may navigate: the user's trackpad settings allow two-finger
    /// swipes. Only read when the gesture begins.
    pub allowed: bool,
}

/// Follows one two-finger gesture at a time and navigates at most once per gesture.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The travel so far of a gesture that may still become a swipe. `None` between gestures,
    /// and once the current one navigated, turned out to be a scroll, or was not allowed.
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
        let step = (x >= RATIO * y).then(|| direction(fingers(*dx, sample.natural)));
        // Settled either way: a diagonal gesture is not retried as it straightens out.
        self.travel = None;
        step
    }
}

fn finite(delta: f64) -> f64 {
    if delta.is_finite() { delta } else { 0. }
}

/// The fingers' horizontal motion, positive to the right, from a reported delta. Natural
/// scrolling reports it as is; the traditional direction inverts it.
fn fingers(dx: f64, natural: bool) -> f64 {
    if natural { dx } else { -dx }
}

/// Fingers moving left go back, moving right go forward.
fn direction(fingers_dx: f64) -> Step {
    if fingers_dx < 0. {
        Step::Back
    } else {
        Step::Forward
    }
}

/// The step of a three-finger `NSEventTypeSwipe`, whose `deltaX` is ±1 for a horizontal swipe
/// and 0 for a vertical one, reported like scroll deltas.
pub fn swipe_step(dx: f64, dy: f64, natural: bool) -> Option<Step> {
    (dx.is_finite() && dx != 0. && dx.abs() > dy.abs()).then(|| direction(fingers(dx, natural)))
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
            natural: true,
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

    #[test]
    fn short_swipes_do_nothing_and_the_threshold_navigates() {
        let mut tracker = Tracker::default();
        assert!(gesture(&mut tracker, &[(-20.), (-20.), (-15.)].map(|x| (x, 0.))).is_empty());
        assert_eq!(
            gesture(&mut tracker, &[(-30., 0.), (-26., 0.)]),
            [Step::Back],
            "exactly {DISTANCE} points navigates"
        );
    }

    #[test]
    fn one_step_per_gesture_while_the_fingers_stay_down() {
        let mut tracker = Tracker::default();
        let moves = [(-40., 0.); 20];
        assert_eq!(gesture(&mut tracker, &moves), [Step::Back]);
        // Coming back does not undo or repeat it either.
        let mut back_and_forth = vec![(-60., 0.)];
        back_and_forth.extend([(60., 0.); 5]);
        assert_eq!(gesture(&mut tracker, &back_and_forth), [Step::Back]);
        // The next gesture navigates again.
        assert_eq!(gesture(&mut tracker, &[(80., 0.)]), [Step::Forward]);
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
        assert_eq!(gesture(&mut tracker, &[(-60., 8.)]), [Step::Back]);
    }

    /// Feeds one swipe of the fingers, `fingers_dx` points to the right, as AppKit reports it:
    /// natural scrolling reports finger motion as is, the traditional direction inverts it.
    fn fingers_swipe(tracker: &mut Tracker, fingers_dx: f64, natural: bool) -> Vec<Step> {
        let reported = if natural { fingers_dx } else { -fingers_dx };
        let event = |phase, dx| Sample {
            natural,
            ..sample(phase, dx, 0.)
        };
        let mut steps = Vec::new();
        steps.extend(tracker.feed(event(Phase::Began, 0.)));
        steps.extend(tracker.feed(event(Phase::Changed, reported)));
        steps.extend(tracker.feed(event(Phase::Ended, 0.)));
        steps
    }

    #[test]
    fn fingers_left_go_back_and_right_forward_whatever_the_scroll_direction() {
        let mut tracker = Tracker::default();
        for natural in [true, false] {
            assert_eq!(
                fingers_swipe(&mut tracker, -70., natural),
                [Step::Back],
                "{natural}"
            );
            assert_eq!(
                fingers_swipe(&mut tracker, 70., natural),
                [Step::Forward],
                "{natural}"
            );
        }
        // Three-finger swipes report the same way.
        assert_eq!(swipe_step(-1., 0., true), Some(Step::Back));
        assert_eq!(swipe_step(1., 0., true), Some(Step::Forward));
        assert_eq!(swipe_step(1., 0., false), Some(Step::Back));
        assert_eq!(swipe_step(-1., 0., false), Some(Step::Forward));
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
            assert_eq!(swipe_step(dx, dy, true), None, "{dx} {dy}");
        }
        assert_eq!(swipe_step(-1., 0.5, true), Some(Step::Back));
    }

    #[test]
    fn momentum_and_ended_events_never_navigate() {
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
        // Mouse wheels have no phase and never navigate.
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
            Some(Step::Back)
        );
        // MayBegin (fingers resting) also clears a gesture whose end was missed.
        tracker.feed(sample(Phase::Began, 0., 0.));
        tracker.feed(sample(Phase::Changed, -50., 0.));
        tracker.feed(sample(Phase::MayBegin, 0., 0.));
        assert_eq!(tracker.feed(sample(Phase::Changed, -50., 0.)), None);
    }

    #[test]
    fn gestures_with_swipes_disabled_are_ignored() {
        let mut tracker = Tracker::default();
        let not_allowed = Sample {
            allowed: false,
            ..sample(Phase::Began, -100., 0.)
        };
        assert_eq!(tracker.feed(not_allowed), None);
        // Later events of that gesture cannot claim it, even if they are flagged allowed.
        assert_eq!(tracker.feed(sample(Phase::Changed, -100., 0.)), None);
        // A gesture that begins already past the threshold still navigates once.
        assert_eq!(
            tracker.feed(sample(Phase::Began, -100., 0.)),
            Some(Step::Back)
        );
        assert_eq!(tracker.feed(sample(Phase::Changed, -100., 0.)), None);
    }

    #[test]
    fn non_finite_deltas_are_ignored() {
        let mut tracker = Tracker::default();
        let moves = [(f64::NAN, 0.), (f64::INFINITY, f64::NAN), (-60., 0.)];
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
