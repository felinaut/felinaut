//! egui inside a GPUI element, so that an egui widget can live in a GPUI window. Felinaut
//! uses it for the cookie graph, which `egui_graphs` lays out and draws.
//!
//! egui only computes here. Each frame, `Host::pass` turns the pointer input queued since
//! the last frame into shapes, and `Host::element` paints those shapes with GPUI and queues
//! the next input. egui gets no fonts and draws no text: a widget hands its text to `label`,
//! and GPUI draws it with the system font, like the rest of the window.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use egui::{Color32, Pos2, Shape, epaint};
use gpui::{
    App, BorderStyle, Bounds, Canvas, DispatchPhase, EntityId, Hitbox, HitboxBehavior, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, Rgba, ScrollDelta,
    ScrollWheelEvent, SharedString, Size, TextRun, Window, canvas, font, point, px, quad, size,
};

thread_local! {
    /// The text of the pass in progress. egui runs on the UI thread only.
    static LABELS: RefCell<Vec<Label>> = const { RefCell::new(Vec::new()) };
}

/// One line of text for GPUI to draw at `anchor` (egui coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub anchor: Pos2,
    /// Which way the text extends from the anchor, each axis from -1 to 1: `(0, 1)` hangs
    /// centered below it, `(1, 0)` starts at it and runs right, `(-1, 0)` ends at it.
    pub toward: egui::Vec2,
    pub text: String,
    pub size: f32,
    pub color: Color32,
    pub strong: bool,
    /// Painted behind the text, so that it stays readable over lines and other labels.
    pub plate: Color32,
}

/// Called by a widget while it draws, in place of egui's own text shapes.
pub fn label(label: Label) {
    LABELS.with_borrow_mut(|labels| labels.push(label));
}

/// What one egui pass wants on screen.
#[derive(Default)]
pub struct Frame {
    pub shapes: Vec<Shape>,
    pub labels: Vec<Label>,
}

#[derive(Default)]
struct Pointer {
    events: Vec<egui::Event>,
    /// The element's size at its last paint; egui's screen is exactly the element.
    size: Option<Size<Pixels>>,
    /// A button went down inside the element and has not come up yet.
    pressed: bool,
    inside: bool,
}

pub struct Host {
    ctx: egui::Context,
    pointer: Rc<RefCell<Pointer>>,
    started: Instant,
}

impl Default for Host {
    fn default() -> Self {
        let ctx = egui::Context::default();
        // Without fonts egui must not look any up, and it preloads glyphs for every text
        // style at the start of a pass.
        ctx.all_styles_mut(|style| style.text_styles.clear());
        Self {
            ctx,
            pointer: Rc::default(),
            started: Instant::now(),
        }
    }
}

impl Host {
    /// Whether the pointer has done anything since the last pass.
    pub fn has_input(&self) -> bool {
        !self.pointer.borrow().events.is_empty()
    }

    /// Whether the user scrolled since the last pass: the gesture that zooms.
    pub fn has_zoom_input(&self) -> bool {
        self.pointer
            .borrow()
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Zoom(_)))
    }

    /// Runs `ui` once with the input queued since the last pass.
    pub fn pass(&self, focused: bool, ui: impl FnMut(&mut egui::Ui)) -> Frame {
        let (events, extent) = {
            let mut pointer = self.pointer.borrow_mut();
            (std::mem::take(&mut pointer.events), pointer.size)
        };
        // Before the first paint there is no size yet; any plausible one will do.
        let extent = extent.unwrap_or(size(px(600.), px(400.)));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(f32::from(extent.width), f32::from(extent.height)),
            )),
            time: Some(self.started.elapsed().as_secs_f64()),
            events,
            focused,
            ..Default::default()
        };
        LABELS.with_borrow_mut(Vec::clear);
        let mut output = self.ctx.run_ui(input, ui);
        // Nothing here uploads textures, and the delta must not be dropped unapplied.
        output.textures_delta.clear();
        Frame {
            shapes: std::mem::take(&mut output.shapes)
                .into_iter()
                .map(|clipped| clipped.shape)
                .collect(),
            labels: LABELS.take(),
        }
    }

    /// The element that shows `frame` and listens for the pointer. `view` is re-rendered
    /// whenever input arrives, so that its next `pass` sees it.
    pub fn element(&self, frame: Frame, view: EntityId) -> Canvas<Hitbox> {
        let pointer = self.pointer.clone();
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, cx| {
                let resized = pointer.borrow_mut().size.replace(bounds.size) != Some(bounds.size);
                if resized {
                    cx.notify(view);
                }
                window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                    for shape in &frame.shapes {
                        paint_shape(shape, bounds.origin, window);
                    }
                    for label in &frame.labels {
                        paint_label(label, bounds.origin, window, cx);
                    }
                });
                listen(pointer, bounds, hitbox, view, window);
            },
        )
    }
}

fn listen(
    pointer: Rc<RefCell<Pointer>>,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
    view: EntityId,
    window: &mut Window,
) {
    let local = move |position: Point<Pixels>| {
        Pos2::new(
            f32::from(position.x - bounds.origin.x),
            f32::from(position.y - bounds.origin.y),
        )
    };
    let button = |button: MouseButton| match button {
        MouseButton::Left => Some(egui::PointerButton::Primary),
        MouseButton::Right => Some(egui::PointerButton::Secondary),
        MouseButton::Middle => Some(egui::PointerButton::Middle),
        MouseButton::Navigate(_) => None,
    };

    window.on_mouse_event({
        let (pointer, hitbox) = (pointer.clone(), hitbox.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            let Some(button) = button(event.button) else {
                return;
            };
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            let pos = local(event.position);
            let mut pointer = pointer.borrow_mut();
            pointer.pressed = true;
            pointer.events.push(egui::Event::PointerMoved(pos));
            pointer.events.push(egui::Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            cx.notify(view);
        }
    });
    window.on_mouse_event({
        let pointer = pointer.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            let Some(button) = button(event.button) else {
                return;
            };
            let mut pointer = pointer.borrow_mut();
            // A drag that leaves the element still ends when the button comes up.
            if phase != DispatchPhase::Bubble || !pointer.pressed {
                return;
            }
            pointer.pressed = false;
            pointer.events.push(egui::Event::PointerButton {
                pos: local(event.position),
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            cx.notify(view);
        }
    });
    window.on_mouse_event({
        let (pointer, hitbox) = (pointer.clone(), hitbox.clone());
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let mut pointer = pointer.borrow_mut();
            let inside = hitbox.is_hovered(window);
            if inside || pointer.pressed {
                pointer
                    .events
                    .push(egui::Event::PointerMoved(local(event.position)));
            } else if pointer.inside {
                pointer.events.push(egui::Event::PointerGone);
            } else {
                return;
            }
            pointer.inside = inside;
            cx.notify(view);
        }
    });
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
            return;
        }
        let amount = match event.delta {
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / 240.,
            ScrollDelta::Lines(delta) => delta.y / 8.,
        };
        let mut pointer = pointer.borrow_mut();
        pointer
            .events
            .push(egui::Event::PointerMoved(local(event.position)));
        // Scrolling zooms, as in a map. One event is bounded: a flick must not fling the view.
        pointer
            .events
            .push(egui::Event::Zoom(amount.clamp(-0.5, 0.5).exp()));
        cx.notify(view);
    });
}

fn color(color: Color32) -> Rgba {
    let [red, green, blue, alpha] = color.to_srgba_unmultiplied();
    Rgba {
        r: f32::from(red) / 255.,
        g: f32::from(green) / 255.,
        b: f32::from(blue) / 255.,
        a: f32::from(alpha) / 255.,
    }
}

fn solid(stroke: &epaint::PathStroke) -> Color32 {
    match &stroke.color {
        epaint::ColorMode::Solid(color) => *color,
        // A color per position has no GPUI counterpart.
        epaint::ColorMode::UV(_) => Color32::TRANSPARENT,
    }
}

fn stroke_path(
    window: &mut Window,
    start: Point<Pixels>,
    width: f32,
    ink: Color32,
    draw: &dyn Fn(&mut PathBuilder),
) {
    if width <= 0. || ink == Color32::TRANSPARENT {
        return;
    }
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(start);
    draw(&mut path);
    if let Ok(path) = path.build() {
        window.paint_path(path, color(ink));
    }
}

/// Paints the shapes a graph is made of. Text, images and meshes are not supported: egui
/// draws those from a texture atlas that GPUI cannot sample.
fn paint_shape(shape: &Shape, origin: Point<Pixels>, window: &mut Window) {
    let at = |pos: Pos2| point(origin.x + px(pos.x), origin.y + px(pos.y));
    match shape {
        Shape::Vec(shapes) => {
            for shape in shapes {
                paint_shape(shape, origin, window);
            }
        }
        Shape::Circle(circle) => {
            let radius = px(circle.radius);
            let center = at(circle.center);
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - radius, center.y - radius),
                    size(radius * 2., radius * 2.),
                ),
                radius,
                color(circle.fill),
                px(circle.stroke.width),
                color(circle.stroke.color),
                BorderStyle::default(),
            ));
        }
        Shape::Rect(rect) => {
            window.paint_quad(quad(
                Bounds::new(
                    at(rect.rect.min),
                    size(px(rect.rect.width()), px(rect.rect.height())),
                ),
                px(f32::from(rect.corner_radius.nw)),
                color(rect.fill),
                px(rect.stroke.width),
                color(rect.stroke.color),
                BorderStyle::default(),
            ));
        }
        Shape::LineSegment { points, stroke } => {
            stroke_path(window, at(points[0]), stroke.width, stroke.color, &|path| {
                path.line_to(at(points[1]));
            });
        }
        Shape::Path(path) => {
            let Some((first, rest)) = path.points.split_first() else {
                return;
            };
            if path.closed && path.fill != Color32::TRANSPARENT {
                let mut fill = PathBuilder::fill();
                fill.move_to(at(*first));
                for point in rest {
                    fill.line_to(at(*point));
                }
                fill.close();
                if let Ok(fill) = fill.build() {
                    window.paint_path(fill, color(path.fill));
                }
            }
            stroke_path(
                window,
                at(*first),
                path.stroke.width,
                solid(&path.stroke),
                &|line| {
                    for point in rest {
                        line.line_to(at(*point));
                    }
                    if path.closed {
                        line.close();
                    }
                },
            );
        }
        Shape::QuadraticBezier(curve) => {
            let [start, control, end] = curve.points;
            stroke_path(
                window,
                at(start),
                curve.stroke.width,
                solid(&curve.stroke),
                &|path| {
                    path.curve_to(at(end), at(control));
                },
            );
        }
        Shape::CubicBezier(curve) => {
            let [start, first, second, end] = curve.points;
            stroke_path(
                window,
                at(start),
                curve.stroke.width,
                solid(&curve.stroke),
                &|path| {
                    path.cubic_bezier_to(at(end), at(first), at(second));
                },
            );
        }
        Shape::Noop | Shape::Ellipse(_) | Shape::Text(_) | Shape::Mesh(_) | Shape::Callback(_) => {}
    }
}

fn paint_label(label: &Label, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
    if label.text.is_empty() || label.size < 1. {
        return;
    }
    let mut face = font(".AppleSystemUIFont");
    if label.strong {
        face.weight = gpui::FontWeight::SEMIBOLD;
    }
    let text = SharedString::from(label.text.clone());
    let run = TextRun {
        len: text.len(),
        font: face,
        color: color(label.color).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let font_size = px(label.size);
    let line = window
        .text_system()
        .shape_line(text, font_size, &[run], None);
    let line_height = font_size * 1.3;
    let (across, down) = extent(label.toward);
    let left = origin.x + px(label.anchor.x) - line.width * across;
    let top = origin.y + px(label.anchor.y) - line_height * down;
    if label.plate != Color32::TRANSPARENT {
        window.paint_quad(quad(
            Bounds::new(
                point(left - px(4.), top),
                size(line.width + px(8.), line_height),
            ),
            px(4.),
            color(label.plate),
            px(0.),
            color(Color32::TRANSPARENT),
            BorderStyle::default(),
        ));
    }
    let _ = line.paint(point(left, top), line_height, window, cx);
}

/// The share of a label's width and height that lies before its anchor, for a direction.
fn extent(toward: egui::Vec2) -> (f32, f32) {
    let share = |axis: f32| {
        if axis.is_finite() {
            (1. - axis.clamp(-1., 1.)) / 2.
        } else {
            0.5
        }
    };
    (share(toward.x), share(toward.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pass_without_fonts_returns_the_shapes_and_labels_of_the_ui() {
        let host = Host::default();
        for _ in 0..3 {
            let frame = host.pass(true, |ui| {
                ui.painter()
                    .circle_filled(Pos2::new(10., 10.), 4., Color32::RED);
                label(Label {
                    anchor: Pos2::new(10., 16.),
                    toward: egui::Vec2::DOWN,
                    text: "example.com".into(),
                    size: 11.,
                    color: Color32::WHITE,
                    strong: false,
                    plate: Color32::TRANSPARENT,
                });
            });
            assert!(
                frame
                    .shapes
                    .iter()
                    .any(|shape| matches!(shape, Shape::Circle(circle) if circle.radius == 4.))
            );
            assert_eq!(frame.labels.len(), 1, "labels do not pile up across passes");
            assert_eq!(frame.labels[0].text, "example.com");
        }
    }

    #[test]
    fn an_empty_ui_is_an_empty_frame_and_queued_input_is_consumed() {
        let host = Host::default();
        assert!(!host.has_input());
        host.pointer.borrow_mut().events.extend([
            egui::Event::PointerMoved(Pos2::new(5., 5.)),
            egui::Event::Zoom(1.1),
        ]);
        assert!(host.has_input() && host.has_zoom_input());
        let mut zoom = 0.;
        let frame = host.pass(true, |ui| zoom = ui.input(|input| input.zoom_delta()));
        assert!((zoom - 1.1).abs() < 1e-4, "{zoom}");
        assert!(frame.labels.is_empty());
        assert!(!host.has_input());
    }

    #[test]
    fn a_label_extends_from_its_anchor_in_its_direction() {
        use egui::Vec2;
        assert_eq!(extent(Vec2::new(0., 1.)), (0.5, 0.), "centered below");
        assert_eq!(extent(Vec2::new(1., 0.)), (0., 0.5), "to the right");
        assert_eq!(extent(Vec2::new(-1., 0.)), (1., 0.5), "to the left");
        assert_eq!(extent(Vec2::new(0., -1.)), (0.5, 1.), "above");
        assert_eq!(extent(Vec2::new(9., -9.)), (0., 1.), "bounded");
        assert_eq!(extent(Vec2::new(f32::NAN, f32::INFINITY)), (0.5, 0.5));
    }

    #[test]
    fn colors_keep_their_channels_and_alpha() {
        let rgba = color(Color32::from_rgba_unmultiplied(255, 128, 0, 128));
        assert!((rgba.r - 1.).abs() < 0.02 && (rgba.g - 0.5).abs() < 0.02 && rgba.b < 0.02);
        assert!((rgba.a - 0.5).abs() < 0.02);
        assert_eq!(color(Color32::TRANSPARENT).a, 0.);
    }
}
