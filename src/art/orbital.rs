//! Shared exploration motifs: cross-wound yarn planets, orbital threads and moons.

use crate::theme::{Palette, Theme};
use gpui::{
    Background, Bounds, PathBuilder, Pixels, Point, linear_color_stop, linear_gradient, point, px,
    rgb,
};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy)]
pub(super) struct Frame(pub Bounds<Pixels>);

impl Frame {
    pub(super) fn at(self, x: f32, y: f32) -> Point<Pixels> {
        point(
            self.0.origin.x + self.0.size.width * x,
            self.0.origin.y + self.0.size.height * y,
        )
    }

    pub(super) fn unit(self) -> f32 {
        f32::from(self.0.size.width).min(f32::from(self.0.size.height))
    }
}

pub(super) fn ink(color: u32, opacity: f32) -> Background {
    gpui::rgba((color << 8) | (opacity.clamp(0., 1.) * 255.).round() as u32).into()
}

pub(super) fn wash(a: u32, b: u32, opacity: f32) -> Background {
    linear_gradient(
        135.,
        linear_color_stop(rgb(a), 0.).opacity(opacity),
        linear_color_stop(rgb(b), 1.).opacity(opacity),
    )
}

#[derive(Clone, Copy)]
struct Sphere {
    center: Point<Pixels>,
    radius: f32,
    angle: f32,
}

impl Sphere {
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
        path.move_to(self.at(scale, 0.));
        for (end, a, b) in [
            ((0., scale), (scale, k), (k, scale)),
            ((-scale, 0.), (-k, scale), (-scale, k)),
            ((0., -scale), (-scale, -k), (-k, -scale)),
            ((scale, 0.), (k, -scale), (scale, -k)),
        ] {
            path.cubic_bezier_to(self.at(end.0, end.1), self.at(a.0, a.1), self.at(b.0, b.1));
        }
        path.close();
        path
    }

    fn orbit(self, front: bool) -> PathBuilder {
        let mut path = PathBuilder::stroke(px(self.radius * 0.025));
        let side = if front { 1. } else { -1. };
        path.move_to(self.at(-1.65, 0.));
        path.cubic_bezier_to(
            self.at(1.65, 0.),
            self.at(-1.65, side * 0.82),
            self.at(1.65, side * 0.82),
        );
        path
    }
}

fn yarn_planet(
    ball: Sphere,
    p: Palette,
    colors: [u32; 2],
    strength: f32,
    paint: &mut dyn FnMut(PathBuilder, Background),
) {
    let [a, b] = colors;
    let orbit = Sphere {
        angle: -0.28,
        ..ball
    };
    paint(orbit.orbit(false), ink(p.highlight, strength * 0.38));
    for (scale, alpha) in [(1.22, 0.035), (1.10, 0.055)] {
        paint(ball.disc(scale), wash(a, p.background, alpha * strength));
    }
    paint(ball.disc(1.025), ink(p.background, 0.85 * strength));
    paint(ball.disc(1.), wash(a, b, strength));
    // A translucent offset highlight and shaded rim retain round volume beneath
    // the crossing wool bundles; all bowed chords stay inside the silhouette.
    let glint = Sphere {
        center: ball.at(-0.27, -0.30),
        radius: ball.radius * 0.51,
        ..ball
    };
    paint(glint.disc(1.), wash(0xf7f6ff, a, 0.16 * strength));
    for family in 0..2 {
        let winding = Sphere {
            angle: ball.angle + if family == 0 { -0.52 } else { 0.93 },
            ..ball
        };
        let mut threads = PathBuilder::stroke(px(ball.radius * 0.033));
        for offset in [-0.72_f32, -0.43, -0.14, 0.15, 0.44, 0.73] {
            let half = (1. - offset * offset).sqrt() * 0.97;
            let bow = (1. - offset.abs()) * 0.32;
            threads.move_to(winding.at(-half, offset));
            threads.cubic_bezier_to(
                winding.at(half, offset),
                winding.at(-half * 0.42, offset + bow),
                winding.at(half * 0.42, offset + bow),
            );
        }
        paint(
            threads,
            wash(
                if family == 0 { 0xffe5bc } else { 0xf7f6ff },
                b,
                0.68 * strength,
            ),
        );
    }
    paint(orbit.orbit(true), wash(p.highlight, a, 0.70 * strength));
    let mut loose = PathBuilder::stroke(px(ball.radius * 0.025));
    loose.move_to(ball.at(0.68, 0.67));
    loose.cubic_bezier_to(
        ball.at(0.32, 1.78),
        ball.at(1.72, 1.04),
        ball.at(1.12, 2.10),
    );
    loose.cubic_bezier_to(
        ball.at(-0.15, 2.10),
        ball.at(-0.50, 1.37),
        ball.at(-0.68, 2.22),
    );
    paint(loose, wash(a, p.highlight, 0.65 * strength));
}

fn moon(ball: Sphere, p: Palette, strength: f32, paint: &mut dyn FnMut(PathBuilder, Background)) {
    paint(ball.disc(1.14), ink(p.highlight, 0.045 * strength));
    paint(ball.disc(1.), wash(0xe1e7ff, p.secondary, 0.68 * strength));
    for (x, y, radius) in [
        (-0.27, -0.32, 0.22),
        (0.31, 0.20, 0.29),
        (-0.28, 0.44, 0.13),
    ] {
        let crater = Sphere {
            center: ball.at(x, y),
            radius: ball.radius * radius,
            ..ball
        };
        paint(
            crater.disc(1.),
            wash(p.secondary, p.background, 0.19 * strength),
        );
    }
}

pub(super) fn star(center: Point<Pixels>, radius: f32) -> PathBuilder {
    let mut path = PathBuilder::fill();
    for vertex in 0..8 {
        let angle = vertex as f32 * PI / 4. - PI / 2.;
        let r = radius * if vertex % 2 == 0 { 1. } else { 0.22 };
        let pt = point(
            center.x + px(angle.cos() * r),
            center.y + px(angle.sin() * r),
        );
        if vertex == 0 {
            path.move_to(pt);
        } else {
            path.line_to(pt);
        }
    }
    path.close();
    path
}

pub(super) fn compose(
    bounds: Bounds<Pixels>,
    p: Palette,
    phase: f32,
    paint: &mut dyn FnMut(PathBuilder, Background),
) {
    let f = Frame(bounds);
    let rich = p.theme == Theme::MilkyWay;
    let strength = if rich {
        0.93
    } else if p.dark {
        0.56
    } else {
        0.43
    };
    let t = phase as f64;
    for (right, y, radius, colors) in [
        (
            false,
            0.23,
            if rich { 0.066 } else { 0.043 },
            [0xffb45e, 0xff72b6],
        ),
        (true, 0.78, 0.073, [0xb3a0ff, 0x78d9ef]),
    ] {
        if right && !rich {
            continue;
        }
        let offset = if right { 2.1 } else { 0. };
        let sway = (t * 0.38 + offset).sin() as f32;
        yarn_planet(
            Sphere {
                center: f.at(if right { 0.88 } else { 0.11 }, y + sway * 0.008),
                radius: f.unit() * radius,
                angle: (t * 0.055 + offset).rem_euclid(std::f64::consts::TAU) as f32,
            },
            p,
            colors,
            strength,
            paint,
        );
    }
    moon(
        Sphere {
            center: f.at(0.88, 0.22 + (t * 0.31).cos() as f32 * 0.006),
            radius: f.unit() * if rich { 0.032 } else { 0.022 },
            angle: 0.,
        },
        p,
        strength,
        paint,
    );
    // Two small asymmetric constellations hug the upper corners. Neither their
    // nodes nor their connecting orbital thread enter the hero's clearing.
    for right in [false, true] {
        let nodes = [(0.035, 0.09), (0.080, 0.06), (0.145, 0.095), (0.185, 0.045)];
        let at = |x: f32, y: f32| f.at(if right { 1. - x } else { x }, y);
        let mut thread = PathBuilder::stroke(px(f.unit() * 0.0009));
        for (index, (x, y)) in nodes.into_iter().enumerate() {
            if index == 0 {
                thread.move_to(at(x, y));
            } else {
                thread.line_to(at(x, y));
            }
        }
        paint(thread, ink(p.primary, 0.25 * strength));
        for (index, (x, y)) in nodes.into_iter().enumerate() {
            let pulse = (phase * 0.42 + index as f32 * TAU / 4.).sin();
            paint(
                star(at(x, y), f.unit() * (0.0032 + pulse * 0.0004)),
                ink(p.primary, 0.66 * strength),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn orbital_details_are_bounded_deterministic_and_animated_in_all_palettes() {
        let bounds = Bounds::new(point(px(13.), px(29.)), size(px(1200.), px(800.)));
        for theme in Theme::ALL {
            let frame = |phase| {
                let mut paths = Vec::new();
                compose(bounds, theme.palette(), phase, &mut |builder, _| {
                    paths.push(format!(
                        "{:?}",
                        builder.build().expect("valid orbital motif")
                    ));
                });
                paths
            };
            let still = frame(0.);
            assert_eq!(still, frame(0.));
            assert_ne!(still, frame(1.));
            assert_eq!(still.len(), if theme == Theme::MilkyWay { 35 } else { 25 });
        }
    }

    #[test]
    fn planet_envelopes_leave_the_hero_clearing_untouched() {
        // Yarn and its strand fit within 2.5 radii regardless of rotation.
        // Using min(width, height) keeps the same guarantee at every aspect ratio.
        for rich in [false, true] {
            for (x, radius) in [(0.11, if rich { 0.066 } else { 0.043 }), (0.88, 0.073)] {
                assert!(x + radius * 2.5 < 0.30 || x - radius * 2.5 > 0.69);
            }
        }
    }
}
