//! Theme-specific procedural artwork. Owners advance seconds only while active.

#[path = "art/alpine.rs"]
mod alpine;
#[path = "art/forest.rs"]
mod forest;
#[path = "art/midnight.rs"]
mod midnight;
#[path = "art/milky_way.rs"]
mod milky_way;
#[path = "art/orbital.rs"]
mod orbital;
#[path = "art/sunset.rs"]
mod sunset;
#[path = "art/wool.rs"]
mod wool;

use crate::theme::{Palette, Theme};
use gpui::{Background, Bounds, Canvas, PathBuilder, Pixels, Styled, canvas};

/// Responsive native vector artwork, with a complete static frame at phase zero.
pub fn backdrop(palette: Palette, phase: f32) -> Canvas<()> {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            compose(bounds, palette, phase, &mut |builder, ink| {
                if let Ok(path) = builder.build() {
                    window.paint_path(path, ink);
                }
            });
        },
    )
    .size_full()
}

fn compose(
    bounds: Bounds<Pixels>,
    p: Palette,
    phase: f32,
    paint: &mut dyn FnMut(PathBuilder, Background),
) {
    let w = f32::from(bounds.size.width);
    let h = f32::from(bounds.size.height);
    let x = f32::from(bounds.origin.x);
    let y = f32::from(bounds.origin.y);
    // Reject invalid origins too, and leave arithmetic headroom for off-canvas
    // Bézier controls and stroke expansion before handing geometry to lyon.
    if w <= 0. || h <= 0. || !(w * 4. + x.abs()).is_finite() || !(h * 4. + y.abs()).is_finite() {
        return;
    }
    let phase = if phase.is_finite() {
        phase.rem_euclid(3600.)
    } else {
        0.
    };
    match p.theme {
        Theme::MidnightParty => midnight::compose(bounds, p, phase, paint),
        Theme::ConfettiSunset => sunset::compose(bounds, p, phase, paint),
        Theme::NeonWool => wool::compose(bounds, p, phase, paint),
        Theme::AlpinePop => alpine::compose(bounds, p, phase, paint),
        Theme::ForestCarnival => forest::compose(bounds, p, phase, paint),
        Theme::MilkyWay => milky_way::compose(bounds, p, phase, paint),
    }
    orbital::compose(bounds, p, phase, paint);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    #[test]
    fn every_theme_has_distinct_animated_geometry() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(1200.), px(800.)));
        let mut signatures = Vec::new();
        for theme in Theme::ALL {
            let frame = |phase| {
                let mut signature = String::new();
                compose(bounds, theme.palette(), phase, &mut |builder, _| {
                    let path = builder.build().expect("valid theme geometry");
                    signature.push_str(&format!("{path:?}"));
                });
                signature
            };
            let still = frame(0.);
            assert!(!still.is_empty());
            assert_ne!(
                still,
                frame(1.),
                "{} must move within one second",
                theme.name()
            );
            assert_eq!(still, frame(f32::NAN));
            assert!(
                !signatures.contains(&still),
                "themes must have distinct shapes"
            );
            signatures.push(still);
        }
    }

    fn geometry(bounds: Bounds<Pixels>, theme: Theme, phase: f32) -> Vec<String> {
        let mut result = Vec::new();
        compose(bounds, theme.palette(), phase, &mut |builder, _| {
            let path = format!(
                "{:?}",
                builder
                    .build()
                    .expect("responsive geometry must tessellate")
            );
            assert!(
                !path.contains("NaN") && !path.contains("inf"),
                "{theme:?}: nonfinite geometry"
            );
            result.push(path);
        });
        result
    }

    #[test]
    fn all_scenes_tessellate_at_small_wide_tall_and_offset_bounds() {
        for theme in Theme::ALL {
            for (w, h) in [(1., 1.), (320., 800.), (2400., 320.), (1440., 900.)] {
                let bounds = Bounds::new(point(px(-17.), px(31.)), size(px(w), px(h)));
                for phase in [0., 1., -1., 3599., f32::MAX, f32::MIN] {
                    let paths = geometry(bounds, theme, phase);
                    assert!(!paths.is_empty());
                    assert!(paths.len() <= 140, "bounded per-frame path budget");
                }
            }
        }
    }

    #[test]
    fn nonfinite_phases_use_the_complete_still_frame_and_finite_phases_wrap() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(600.), px(500.)));
        for theme in Theme::ALL {
            let still = geometry(bounds, theme, 0.);
            for phase in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 3600.] {
                assert_eq!(still, geometry(bounds, theme, phase));
            }
            assert_eq!(geometry(bounds, theme, -1.), geometry(bounds, theme, 3599.));
            assert_eq!(geometry(bounds, theme, 1.), geometry(bounds, theme, 3601.));
        }
    }

    #[test]
    fn empty_nonfinite_and_overflowing_bounds_do_not_paint() {
        for theme in Theme::ALL {
            for (x, y, w, h) in [
                (0., 0., 0., 0.),
                (0., 0., 0., 800.),
                (0., 0., 1200., 0.),
                (0., 0., -1., 800.),
                (0., 0., 1200., -1.),
                (f32::NAN, 0., 1200., 800.),
                (0., f32::INFINITY, 1200., 800.),
                (f32::NEG_INFINITY, 0., 1200., 800.),
                (0., 0., f32::NAN, 800.),
                (0., 0., 1200., f32::NAN),
                (0., 0., f32::INFINITY, 800.),
                (0., 0., 1200., f32::NEG_INFINITY),
                (0., 0., f32::MAX, f32::MAX),
            ] {
                let bounds = Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
                compose(bounds, theme.palette(), 0., &mut |_, _| {
                    panic!("invalid canvas painted")
                });
            }
        }
    }
}
