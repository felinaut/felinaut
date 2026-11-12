//! Deep-space clearing framed by layered nebula ribbons and delicate starlight.

use super::orbital::{Frame, ink, star, wash};
use crate::theme::Palette;
use gpui::{Background, Bounds, PathBuilder, Pixels};

// Stable integer hashing: no random state, wall clock, or frame-dependent seeds.
fn seed(index: u32) -> f32 {
    let n = index.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let n = ((n >> ((n >> 28) + 4)) ^ n).wrapping_mul(277_803_737);
    ((n ^ (n >> 22)) >> 8) as f32 / 16_777_216.
}

fn star_pose(index: u32, phase: f32) -> (f32, f32, f32) {
    let sway = (phase as f64 * 0.24 + index as f64 * 1.7).sin() as f32;
    let lane = 0.018 + seed(index + 31) * 0.185 + sway * 0.003;
    let (x, y) = if index < 52 {
        (
            if index.is_multiple_of(2) {
                lane
            } else {
                1. - lane
            },
            0.13 + seed(index + 71) * 0.76,
        )
    } else {
        (
            0.025 + seed(index + 93) * 0.95,
            if index.is_multiple_of(2) { 0.025 } else { 0.96 } + seed(index + 5) * 0.02,
        )
    };
    (x, y, 0.0009 + seed(index + 11) * 0.0022)
}

pub(super) fn compose(
    bounds: Bounds<Pixels>,
    p: Palette,
    phase: f32,
    paint: &mut dyn FnMut(PathBuilder, Background),
) {
    let f = Frame(bounds);
    let t = phase as f64;
    let mut base = PathBuilder::fill();
    base.move_to(f.at(0., 0.));
    base.line_to(f.at(1., 0.));
    base.line_to(f.at(1., 1.));
    base.line_to(f.at(0., 1.));
    base.close();
    paint(base, wash(p.background, p.surface, 1.));

    // Nested, closed nebula bands provide depth without full-screen glow layers.
    // All Bézier control points stay in the side lanes, leaving x=.30..=.69 quiet.
    for right in [false, true] {
        let x = |x: f32| if right { 1. - x } else { x };
        for layer in 0..7 {
            let n = layer as f32;
            let drift =
                (t * 0.19 + n as f64 * 0.53 + if right { 1.6 } else { 0. }).sin() as f32 * 0.009;
            let inset = n * 0.014;
            let at = |a: f32, b: f32| f.at(x(a + drift), b);
            let mut ribbon = PathBuilder::fill();
            ribbon.move_to(at(-0.08, -0.04));
            ribbon.cubic_bezier_to(
                at(0.11 + inset, 0.30),
                at(0.19 + inset, 0.04),
                at(0.015, 0.13),
            );
            ribbon.cubic_bezier_to(
                at(0.075 + inset * 0.6, 0.65),
                at(0.23 + inset * 0.35, 0.44),
                at(-0.01, 0.52),
            );
            ribbon.cubic_bezier_to(at(-0.05, 1.04), at(0.17 + inset, 0.81), at(0.02, 0.96));
            ribbon.line_to(at(-0.12, 1.04));
            ribbon.cubic_bezier_to(at(-0.08, -0.04), at(-0.025, 0.71), at(-0.15, 0.22));
            ribbon.close();
            let (a, b) = match layer % 3 {
                0 => (0x6856b5, 0x283567),
                1 => (0xa05799, 0x405888),
                _ => (0x477b9d, 0x51458b),
            };
            paint(ribbon, wash(a, b, 0.11 - n * 0.008));
        }
    }

    // Three long, low nebula currents bind the wings below the hero. The upper
    // hull never rises above y=.90, even at the extremes of its gentle motion.
    for layer in 0..3 {
        let n = layer as f32;
        let y = 0.925 + n * 0.026;
        let flow = (t * 0.22 + n as f64).sin() as f32 * 0.008;
        let mut ribbon = PathBuilder::fill();
        ribbon.move_to(f.at(-0.03, y));
        ribbon.cubic_bezier_to(
            f.at(0.52, y + 0.022),
            f.at(0.14, y - 0.018 + flow),
            f.at(0.30, y + 0.06),
        );
        ribbon.cubic_bezier_to(
            f.at(1.03, y - 0.008),
            f.at(0.73, y - 0.014),
            f.at(0.85, y + flow),
        );
        ribbon.line_to(f.at(1.03, 1.04));
        ribbon.line_to(f.at(-0.03, 1.04));
        ribbon.close();
        paint(
            ribbon,
            wash([0x6c4d9e, 0x386d87, 0x9b5a8e][layer], p.background, 0.14),
        );
    }

    for index in 0..68 {
        let (x, y, radius) = star_pose(index, phase);
        let breath = (t * 0.33 + index as f64 * 1.4).sin() as f32;
        let color = [0xf7f6ff, p.primary, p.highlight, 0xffb45e][index as usize % 4];
        // A stable halo, not blinking opacity or a flashing starfield.
        if index.is_multiple_of(9) {
            paint(star(f.at(x, y), f.unit() * radius * 2.2), ink(color, 0.06));
        }
        paint(
            star(f.at(x, y), f.unit() * radius * (1. + breath * 0.09)),
            ink(color, 0.40 + breath * 0.06),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::{point, px, size};

    #[test]
    fn galaxy_layers_tessellate_responsively_and_pause_at_a_fixed_phase() {
        for (w, h) in [(1440., 900.), (320., 800.), (2400., 320.), (1., 1.)] {
            let frame = |phase| {
                let mut paths = Vec::new();
                compose(
                    Bounds::new(point(px(17.), px(31.)), size(px(w), px(h))),
                    Theme::MilkyWay.palette(),
                    phase,
                    &mut |builder, _| {
                        let path = format!("{:?}", builder.build().expect("galaxy tessellation"));
                        assert!(!path.contains("NaN") && !path.contains("inf"));
                        paths.push(path);
                    },
                );
                paths
            };
            let still = frame(0.);
            assert_eq!(still.len(), 94);
            assert_eq!(
                still,
                frame(0.),
                "a paused caller produces an identical still frame"
            );
            for phase in [0.5, 1., 13., 3599., f32::MAX] {
                let moving = frame(phase);
                assert_eq!(still[0], moving[0], "the clearing stays still");
                assert_ne!(still, moving, "the nebula and stars drift gently");
            }
        }
    }

    #[test]
    fn stars_never_enter_the_hero_clearing() {
        for index in 0..68 {
            for phase in [0., 1., 13., 3599., f32::MAX] {
                let (x, y, radius) = star_pose(index, phase);
                assert!((0. ..1.).contains(&x) && (0. ..1.).contains(&y));
                let r = radius * 2.2;
                assert!(x + r < 0.25 || x - r > 0.75 || y + r < 0.13 || y - r > 0.90);
            }
        }
    }
}
