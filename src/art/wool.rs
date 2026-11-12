//! Neon yarn: lit, cross-wound spheres and loose, flowing footer threads.

use crate::theme::Palette;
use gpui::{
    Background, Bounds, PathBuilder, Pixels, Point, linear_color_stop, linear_gradient, point, px,
    rgb,
};

fn ink(a: u32, alpha: f32, b: u32, end_alpha: f32) -> Background {
    linear_gradient(
        135.0,
        linear_color_stop(rgb(a), 0.0).opacity(alpha),
        linear_color_stop(rgb(b), 1.0).opacity(end_alpha),
    )
}

#[derive(Clone, Copy)]
struct Ball {
    center: Point<Pixels>,
    radius: f32,
    angle: f32,
}

impl Ball {
    fn at(self, x: f32, y: f32) -> Point<Pixels> {
        let (s, c) = self.angle.sin_cos();
        point(
            self.center.x + px(self.radius * (x * c - y * s)),
            self.center.y + px(self.radius * (x * s + y * c)),
        )
    }

    fn disc(self, scale: f32) -> PathBuilder {
        let mut path = PathBuilder::fill();
        let k = scale * 0.552_284_8;
        path.move_to(self.at(scale, 0.0));
        for (end, a, b) in [
            ((0.0, scale), (scale, k), (k, scale)),
            ((-scale, 0.0), (-k, scale), (-scale, k)),
            ((0.0, -scale), (-scale, -k), (-k, -scale)),
            ((scale, 0.0), (k, -scale), (scale, -k)),
        ] {
            path.cubic_bezier_to(self.at(end.0, end.1), self.at(a.0, a.1), self.at(b.0, b.1));
        }
        path.close();
        path
    }

    fn strand(self, offset: f32, width: f32) -> PathBuilder {
        // Parallel bowed chords remain inside the sphere's convex silhouette.
        let half = (1.0 - offset * offset).sqrt() * 0.975;
        let bow = (1.0 - offset.abs()) * 0.48;
        let mut path = PathBuilder::stroke(px(width));
        path.move_to(self.at(-half, offset));
        path.cubic_bezier_to(
            self.at(half, offset),
            self.at(-half * 0.45, offset + bow),
            self.at(half * 0.45, offset + bow),
        );
        path
    }
}

pub(super) fn compose(
    bounds: Bounds<Pixels>,
    p: Palette,
    phase: f32,
    paint: &mut dyn FnMut(PathBuilder, Background),
) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let at = |x: f32, y: f32| point(bounds.origin.x + px(w * x), bounds.origin.y + px(h * y));
    // Use f64 for time arithmetic so even very large finite phases stay finite.
    let t = phase as f64;
    let strength = if p.dark { 1.0 } else { 0.72 };
    let radius = (w * 0.12).min(h * 0.135);
    let line = (radius / 72.0).clamp(0.08, 1.6);

    for side in 0..2 {
        let right = side == 1;
        let mirror = |x: f32| if right { 1.0 - x } else { x };
        let shift = side as f64 * 2.1;
        let flow = (t * 1.15 + shift).sin() as f32;
        let curl = (t * 0.85 + shift).cos() as f32;
        let color = if right { p.secondary } else { p.primary };
        let other = if right { p.accent } else { p.secondary };
        let ball = Ball {
            center: at(mirror(0.108 + 0.004 * flow), 0.863 + 0.004 * curl),
            radius: radius * if right { 0.88 } else { 1.0 },
            angle: (t * 0.09).rem_euclid(std::f64::consts::TAU) as f32
                + 0.19 * (t * 0.85 + shift).sin() as f32,
        };

        // Broad translucent shells give the bright threads a soft neon bloom.
        for (scale, alpha) in [(1.34, 0.025), (1.19, 0.045), (1.08, 0.085)] {
            paint(ball.disc(scale), ink(color, alpha * strength, other, 0.0));
        }
        paint(ball.disc(1.0), ink(p.elevated, 0.96, p.background, 0.97));
        paint(ball.disc(0.98), ink(color, 0.21 * strength, other, 0.06));

        // Two crossing windings, not latitude stripes: the diagonal bundle
        // overlays the first just as a loosely cross-wound skein would.
        for family in 0..2 {
            let winding = Ball {
                angle: ball.angle + if family == 0 { -0.48 } else { 1.02 },
                ..ball
            };
            for strand in 0..7 {
                let offset = -0.78 + strand as f32 * 0.25;
                let thread_color = if family == 0 { color } else { other };
                paint(
                    winding.strand(offset, 4.2 * line),
                    ink(thread_color, 0.13 * strength, thread_color, 0.035),
                );
                paint(
                    winding.strand(offset, 1.15 * line),
                    ink(thread_color, 0.92 * strength, p.highlight, 0.48 * strength),
                );
            }
        }

        // A single connected strand unwinds from each ball into a footer curl.
        // Only its loose end moves; the attachment tracks the rolling sphere.
        for (width, alpha) in [(6.5, 0.06), (2.8, 0.16), (1.05, 0.85)] {
            let mut yarn = PathBuilder::stroke(px(width * line));
            yarn.move_to(ball.at(0.62, 0.70));
            yarn.cubic_bezier_to(
                at(mirror(0.28), 0.959),
                at(mirror(0.17), 0.993),
                at(mirror(0.23), 0.925 + flow * 0.014),
            );
            yarn.cubic_bezier_to(
                at(mirror(0.435), 0.948 + flow * 0.016),
                at(mirror(0.36), 0.997 - flow * 0.018),
                at(mirror(0.37), 0.893 + curl * 0.018),
            );
            yarn.cubic_bezier_to(
                at(mirror(0.372 + curl * 0.015), 0.953 + flow * 0.013),
                at(mirror(0.49), 0.991 + flow * 0.009),
                at(mirror(0.33), 1.012 - curl * 0.016),
            );
            paint(
                yarn,
                ink(color, alpha * strength, other, alpha * strength * 0.65),
            );
        }

        // Tiny rising starlights connect this yarn system to the orbital scenes.
        for ember in 0..5 {
            let cycle = (t * 0.24 + ember as f64 * 0.2 + shift).rem_euclid(1.0) as f32;
            let x = mirror(0.045 + ember as f32 * 0.032 + 0.009 * flow);
            let y = 0.82 - cycle * 0.23;
            let center = at(x, y);
            let r = line * (1.3 + (1.0 - cycle) * 1.4);
            let spark = super::orbital::star(center, r);
            paint(
                spark,
                ink(p.highlight, (1.0 - cycle) * 0.6 * strength, color, 0.15),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::size;

    #[test]
    fn yarn_tessellates_at_responsive_sizes_and_phases() {
        for (w, h) in [(1440.0, 900.0), (320.0, 800.0), (2400.0, 320.0), (1.0, 1.0)] {
            for phase in [0.0, 0.5, 2.0, 37.0, 600.0, f32::MAX] {
                let mut count = 0;
                compose(
                    Bounds::new(point(px(17.0), px(31.0)), size(px(w), px(h))),
                    Theme::NeonWool.palette(),
                    phase,
                    &mut |path, _| {
                        path.build().expect("neon yarn must tessellate");
                        count += 1;
                    },
                );
                assert_eq!(count, 82);
            }
        }
    }

    #[test]
    fn rolling_windings_and_loose_ends_change_geometry() {
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(1200.0), px(800.0)));
        let geometry = |phase| {
            let mut paths = Vec::new();
            compose(bounds, Theme::NeonWool.palette(), phase, &mut |path, _| {
                paths.push(format!("{:?}", path.build().expect("valid yarn")));
            });
            paths
        };
        let still = geometry(0.0);
        assert_eq!(still, geometry(0.0));
        let moving = geometry(2.0);
        assert_ne!(still[6], moving[6], "wound strands must visibly roll");
        assert_ne!(still[35], moving[35], "loose footer thread must flow");
    }
}
