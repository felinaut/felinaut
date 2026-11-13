//! The cookie tree as an interactive graph: `egui_graphs` lays it out with a force-directed
//! simulation, hit-tests it and handles dragging, panning and zooming; `egui_host` paints
//! the result with GPUI.
//!
//! The graph mirrors the visible rows of `cookies::Tree`: one graph node per row, one edge
//! to its parent. Opening a branch in the model therefore grows the graph, and the model
//! stays the only owner of selection, expansion and deletion.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use egui::{Color32, Pos2, Shape, Stroke, Vec2, epaint::CircleShape};
use egui_graphs::{
    DisplayEdge, DisplayNode, DrawContext, EdgeProps, FruchtermanReingoldWithCenterGravity,
    FruchtermanReingoldWithCenterGravityState, GraphChange, GraphView, LayoutForceDirected,
    NodeProps, SettingsInteraction, SettingsNavigation,
};
use gpui::{Canvas, EntityId, Hitbox};
use petgraph::{
    Directed,
    stable_graph::{DefaultIx, NodeIndex},
};

use crate::{
    cookies::{Node, Row},
    egui_host::{self, Frame, Host, Label},
    theme::Palette,
};

type Layout = LayoutForceDirected<FruchtermanReingoldWithCenterGravity>;
type LayoutState = FruchtermanReingoldWithCenterGravityState;
type Graph = egui_graphs::Graph<Payload, (), Directed, DefaultIx, NodeShape, EdgeShape>;

// ponytail: egui_graphs' Fruchterman-Reingold compares every pair of nodes each step, so a
// frame costs O(n²) while the layout moves. `cookies::MAX_SHOWN` bounds the nodes the model
// lets the graph show, and large stores start closed (see `Tree::opened_within`); the layout
// still stops at `MAX_STEPS`. If a larger graph ever matters, give the layout a spatial grid.

/// Nodes move less than this per step (graph units) once the layout has settled.
const SETTLED: f32 = 0.06;
/// A simulation that never settles (two nodes pushing each other) still stops.
const MAX_STEPS: u64 = 900;
/// Extra simulation steps per frame, so that a few hundred nodes settle in about a second.
const STEPS_PER_FRAME: u32 = 3;
/// The most time one frame gives to those extra steps.
const STEP_BUDGET_MS: u64 = 5;
/// Without animation: the most steps, and time, that one frame gives to settling. A layout
/// that needs more goes on in the next frame, until it settles or reaches `MAX_STEPS`.
const SETTLE_STEPS_PER_FRAME: u32 = 400;
const SETTLE_BUDGET_MS: u64 = 40;

/// What a graph node stands for and how it is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Payload {
    pub node: Node,
    /// 0 for the root, 1 site, 2 host, 3 cookie.
    pub depth: usize,
    /// Cookies at or below the node.
    pub count: usize,
    /// A branch whose children are not shown.
    pub closed: bool,
}

/// The theme's colors for the pass in progress. The shapes are built by `egui_graphs`, which
/// knows nothing about Felinaut's themes, so they read the colors from here.
#[derive(Clone, Copy, Default)]
struct Style {
    levels: [Color32; 4],
    text: Color32,
    muted: Color32,
    line: Color32,
    background: Color32,
    /// Where the root is (graph units): labels point away from it.
    root: Pos2,
}

thread_local! {
    static STYLE: Cell<Style> = Cell::default();
}

fn ink(color: u32) -> Color32 {
    Color32::from_rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

impl Style {
    fn new(p: Palette, root: Pos2) -> Self {
        Self {
            root,
            levels: [
                ink(p.accent),
                ink(p.primary),
                ink(p.secondary),
                ink(p.highlight),
            ],
            text: ink(p.text),
            muted: ink(p.muted),
            line: ink(p.border),
            background: ink(p.background),
        }
    }
}

/// A node's radius in graph units: the root largest, then sites and hosts, growing slowly
/// with the cookies they hold, and cookies as small dots.
pub fn radius(depth: usize, count: usize) -> f32 {
    let growth = (count.max(1) as f32).ln();
    match depth {
        0 => 19.,
        1 => (10. + 2.4 * growth).min(18.),
        2 => (7.5 + 1.8 * growth).min(13.),
        _ => 5.,
    }
}

/// How much nodes grow or shrink on screen at a zoom level. Much less than the distances
/// between them do: a small graph fitted to the window must not turn into a few huge discs,
/// and a large one must keep nodes that can be seen and hit.
pub fn node_scale(zoom: f32) -> f32 {
    zoom.clamp(0.85, 1.25)
}

#[derive(Clone)]
pub struct NodeShape {
    pos: Pos2,
    payload: Payload,
    label: String,
    selected: bool,
    hovered: bool,
    dragged: bool,
    /// The view's zoom when the node was last drawn.
    zoom: f32,
}

impl NodeShape {
    /// The radius in graph units that gives the node its on-screen size at this zoom, so
    /// that hit-testing and the ends of the edges agree with what is drawn.
    fn radius(&self) -> f32 {
        radius(self.payload.depth, self.payload.count) * node_scale(self.zoom) / self.zoom
    }
}

impl From<NodeProps<Payload>> for NodeShape {
    fn from(props: NodeProps<Payload>) -> Self {
        Self {
            pos: props.location(),
            label: props.label.clone(),
            selected: props.selected,
            hovered: props.hovered,
            dragged: props.dragged,
            payload: props.payload,
            zoom: 1.,
        }
    }
}

impl DisplayNode<Payload, (), Directed, DefaultIx> for NodeShape {
    fn closest_boundary_point(&self, dir: Vec2) -> Pos2 {
        self.pos + dir.normalized() * self.radius()
    }

    fn is_inside(&self, pos: Pos2) -> bool {
        // A little slack: the cookie dots are small targets.
        (pos - self.pos).length() <= self.radius() + 3.
    }

    fn update(&mut self, props: &NodeProps<Payload>) {
        self.pos = props.location();
        self.payload.clone_from(&props.payload);
        self.label.clone_from(&props.label);
        self.selected = props.selected;
        self.hovered = props.hovered;
        self.dragged = props.dragged;
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        let style = STYLE.get();
        let center = ctx.meta.canvas_to_screen_pos(self.pos);
        let zoom = ctx.meta.canvas_to_screen_size(1.);
        if zoom.is_finite() && zoom > 0. {
            self.zoom = zoom;
        }
        let radius = self.radius() * self.zoom;
        let fill = style.levels[self.payload.depth.min(3)];
        let active = self.selected || self.hovered || self.dragged;
        let mut shapes = Vec::with_capacity(4);

        // A soft glow under every branch, stronger under the selected node.
        if self.selected {
            shapes.push(Shape::circle_filled(
                center,
                radius + 10.,
                fill.gamma_multiply(0.3),
            ));
        } else if self.payload.depth < 3 {
            shapes.push(Shape::circle_filled(
                center,
                radius + 4.,
                fill.gamma_multiply(0.14),
            ));
        }
        if self.payload.closed && self.payload.count > 0 {
            // A ring around a closed branch: there is more inside.
            shapes.push(Shape::circle_stroke(
                center,
                radius + 4.,
                Stroke::new(1.2, fill.gamma_multiply(0.7)),
            ));
        }
        shapes.push(Shape::Circle(CircleShape {
            center,
            radius,
            fill,
            // The background color keeps the edges from touching the disc. Thin on the
            // small cookie dots, which it would otherwise swallow.
            stroke: Stroke::new(
                f32::min(if active { 2. } else { 1.5 }, radius * 0.3),
                if active { style.text } else { style.background },
            ),
        }));

        let branch = !matches!(self.payload.node, Node::Cookie(_));
        // Zoomed out, names would cover each other: first the hosts and cookies lose theirs,
        // then the sites. The root and the node under the pointer always keep it.
        let named = active
            || self.payload.depth == 0
            || zoom >= 0.55
            || (self.payload.depth == 1 && zoom >= 0.35);
        if named {
            let toward = label_direction(
                self.pos - style.root,
                self.payload.depth,
                branch && !self.payload.closed,
            );
            egui_host::label(Label {
                anchor: center + toward * (radius + 6.),
                toward,
                text: if branch {
                    format!("{}  {}", self.label, self.payload.count)
                } else {
                    self.label.clone()
                },
                size: if self.payload.depth <= 1 { 12. } else { 11. },
                color: if active || self.payload.depth <= 1 {
                    style.text
                } else {
                    style.muted
                },
                strong: self.payload.depth == 0 || self.selected,
                plate: style.background.gamma_multiply(0.72),
            });
        }
        shapes
    }
}

/// Which way a node's label extends from it, given where the node is relative to the root.
/// A leaf's label continues outward, past the end of its branch. An open branch has its
/// children out there, so its label goes sideways instead, to the left for a branch on the
/// left of the root and to the right otherwise, which keeps the labels of neighbouring
/// branches apart. The root's label hangs below it.
fn label_direction(from_root: Vec2, depth: usize, open_branch: bool) -> Vec2 {
    if depth == 0 || from_root.length() < 1. || !from_root.length().is_finite() {
        return Vec2::DOWN;
    }
    let outward = from_root.normalized();
    if !open_branch {
        return outward;
    }
    let sideways = Vec2::new(-outward.y, outward.x);
    if (sideways.x < 0.) == (outward.x < 0.) {
        sideways
    } else {
        -sideways
    }
}

#[derive(Clone)]
pub struct EdgeShape;

impl From<EdgeProps<()>> for EdgeShape {
    fn from(_: EdgeProps<()>) -> Self {
        Self
    }
}

type GraphNode = egui_graphs::Node<Payload, (), Directed, DefaultIx, NodeShape>;

impl DisplayEdge<Payload, (), Directed, DefaultIx, NodeShape> for EdgeShape {
    fn shapes(&mut self, start: &GraphNode, end: &GraphNode, ctx: &DrawContext) -> Vec<Shape> {
        let style = STYLE.get();
        let direction = end.location() - start.location();
        if direction.length() < f32::EPSILON {
            return Vec::new();
        }
        let from = start.display().closest_boundary_point(direction);
        let to = end.display().closest_boundary_point(-direction);
        // The edges of the selected node stand out, to show what a delete would take.
        let lit = start.selected() || end.selected();
        vec![Shape::line_segment(
            [
                ctx.meta.canvas_to_screen_pos(from),
                ctx.meta.canvas_to_screen_pos(to),
            ],
            Stroke::new(
                if lit { 1.8 } else { 1.2 },
                if lit { style.muted } else { style.line },
            ),
        )]
    }

    fn update(&mut self, _: &EdgeProps<()>) {}

    fn is_inside(&self, _: &GraphNode, _: &GraphNode, _: Pos2) -> bool {
        false
    }
}

/// Where a node that just appeared starts: around its parent, spread by the golden angle so
/// that siblings do not start on top of each other and nothing is random.
fn seat(parent: Pos2, sibling: usize, depth: usize) -> Pos2 {
    let angle = sibling as f32 * 2.399_963 + depth as f32;
    let distance = 46. + 4. * (sibling as f32).sqrt();
    parent + distance * Vec2::new(angle.cos(), angle.sin())
}

/// What one frame of the graph produced.
pub struct Shown {
    pub frame: Frame,
    /// A node the user clicked (not dragged).
    pub clicked: Option<Node>,
    /// The layout is still moving, or the pointer is at work: draw another frame.
    pub animating: bool,
}

pub struct CookieGraph {
    host: Host,
    graph: Graph,
    nodes: HashMap<Node, NodeIndex>,
    /// Keep the whole graph in view. Ends when the user zooms or pans, until `fit`.
    fitting: bool,
    /// The graph changed shape, or a node was dragged: the layout has to run again.
    unsettled: bool,
}

impl Default for CookieGraph {
    fn default() -> Self {
        Self {
            host: Host::default(),
            graph: Graph::new(),
            nodes: HashMap::new(),
            fitting: true,
            unsettled: true,
        }
    }
}

impl CookieGraph {
    /// Makes the graph match the visible rows: new rows appear beside their parent, rows
    /// that are gone leave, and the rest keep their place.
    pub fn sync(&mut self, rows: &[Row], selection: &Node) {
        let mut seats: HashMap<usize, usize> = HashMap::new();
        let mut live = Vec::with_capacity(rows.len());
        for row in rows {
            let payload = Payload {
                node: row.node.clone(),
                depth: row.depth,
                count: row.count,
                closed: row.expanded == Some(false),
            };
            // Two cookies of one name differ by path; the path tells them apart.
            let label = match &row.note {
                Some(note) => format!("{} {note}", row.label),
                None => row.label.clone(),
            };
            let parent = row.parent.and_then(|parent| live.get(parent).copied());
            let index = match self.nodes.get(&row.node) {
                Some(index) => *index,
                None => {
                    let origin = parent
                        .and_then(|parent| self.graph.node(parent))
                        .map(GraphNode::location);
                    let sibling = seats.entry(row.parent.unwrap_or(usize::MAX)).or_default();
                    *sibling += 1;
                    let location = match origin {
                        Some(origin) => seat(origin, *sibling, row.depth),
                        // The root: where the layout's gravity pulls, roughly.
                        None => Pos2::new(300., 200.),
                    };
                    let index = self.graph.add_node_with_label_and_location(
                        payload.clone(),
                        label.clone(),
                        location,
                    );
                    if let Some(parent) = parent {
                        self.graph.add_edge(parent, index, ());
                    }
                    self.nodes.insert(row.node.clone(), index);
                    self.unsettled = true;
                    index
                }
            };
            if let Some(node) = self.graph.node_mut(index) {
                if *node.payload() != payload {
                    *node.payload_mut() = payload;
                }
                if node.label() != label {
                    node.set_label(label);
                }
                node.set_selected(row.node == *selection);
            }
            live.push(index);
        }
        let live: HashSet<NodeIndex> = live.into_iter().collect();
        let gone: Vec<Node> = self
            .nodes
            .iter()
            .filter(|(_, index)| !live.contains(index))
            .map(|(node, _)| node.clone())
            .collect();
        for node in gone {
            if let Some(index) = self.nodes.remove(&node) {
                self.graph.remove_node(index);
                self.unsettled = true;
            }
        }
    }

    /// Brings the whole graph back into view.
    pub fn fit(&mut self) {
        self.fitting = true;
    }

    #[cfg(test)]
    fn payloads(&self) -> Vec<Payload> {
        let mut payloads: Vec<Payload> = self
            .graph
            .nodes_iter()
            .map(|(_, node)| node.payload().clone())
            .collect();
        payloads.sort_by_key(|payload| (payload.depth, format!("{:?}", payload.node)));
        payloads
    }

    /// Runs one frame of the graph. With `animate` off (Reduce Motion, or animations turned
    /// off) the layout settles as far as one frame's budget allows instead of moving step by
    /// step, so a small graph appears settled at once and a large one in a few frames.
    pub fn show(&mut self, p: Palette, animate: bool, focused: bool) -> Shown {
        let root = self
            .nodes
            .get(&Node::All)
            .and_then(|root| self.graph.node(*root))
            .map_or(Pos2::ZERO, GraphNode::location);
        STYLE.set(Style::new(p, root));
        let input = self.host.has_input();
        if self.host.has_zoom_input() {
            self.fitting = false;
        }
        let (graph, fitting, unsettled) = (&mut self.graph, self.fitting, self.unsettled);
        let mut clicked = None;
        let mut moving = false;
        let mut panned = false;
        let frame = self.host.pass(focused, |ui| {
            let mut state: LayoutState = egui_graphs::get_layout_state(ui, None);
            if unsettled {
                state.base.is_running = true;
                state.base.step_count = 0;
                state.base.last_avg_displacement = None;
            }
            // Calmer than the defaults: less overshoot, and a pull to the middle that
            // keeps separate sites from drifting apart.
            state.base.dt = 0.04;
            state.base.damping = 0.45;
            state.base.k_scale = 0.62;
            state.extras.0.params.c = 0.42;
            let running = state.base.is_running;
            egui_graphs::set_layout_state(ui, state, None);

            if !animate && running {
                GraphView::<LayoutState, Layout>::fast_forward_until_stable_budgeted(
                    ui,
                    graph,
                    SETTLED,
                    SETTLE_STEPS_PER_FRAME,
                    SETTLE_BUDGET_MS,
                    None,
                );
            } else if animate {
                GraphView::<LayoutState, Layout>::fast_forward_budgeted(
                    ui,
                    graph,
                    STEPS_PER_FRAME,
                    STEP_BUDGET_MS,
                    None,
                );
            }

            let result = GraphView::<LayoutState, Layout>::new()
                .with_interactions(
                    &SettingsInteraction::new()
                        .with_dragging_enabled(true)
                        .with_hover_enabled(true)
                        .with_node_clicking_enabled(true),
                )
                .with_navigations(
                    &SettingsNavigation::new()
                        .with_fit_to_screen_enabled(fitting)
                        .with_zoom_and_pan_enabled(!fitting)
                        .with_fit_to_screen_padding(0.22)
                        .with_zoom_speed(0.05),
                )
                .show(ui, graph);

            let mut dragging = false;
            for change in &result.changes {
                match change {
                    GraphChange::NodeClicked { node } => {
                        clicked = graph.node(*node).map(|node| node.payload().node.clone());
                    }
                    GraphChange::NodeDragStarted { .. } | GraphChange::NodeMoved { .. } => {
                        dragging = true;
                    }
                    _ => {}
                }
            }
            // A drag on empty space is a pan, which fitting would undo at once.
            panned = result.response.dragged() && graph.dragged_node().is_none();

            let mut state: LayoutState = egui_graphs::get_layout_state(ui, None);
            let settled = state
                .base
                .last_avg_displacement
                .is_some_and(|step| step < SETTLED)
                || state.base.step_count > MAX_STEPS;
            if dragging {
                state.base.is_running = true;
                state.base.step_count = 0;
            } else if settled {
                state.base.is_running = false;
            }
            // Without animation a layout that did not settle within this frame's budget
            // goes on settling in the next one, rather than staying half made.
            moving = state.base.is_running;
            egui_graphs::set_layout_state(ui, state, None);
        });
        self.unsettled = false;
        if panned {
            self.fitting = false;
        }
        Shown {
            frame,
            clicked,
            animating: moving || input,
        }
    }

    /// Whether the layout has come to rest, by the same rule `show` stops it by.
    #[cfg(test)]
    fn settled(&self) -> bool {
        let mut settled = false;
        self.host.pass(true, |ui| {
            let state: LayoutState = egui_graphs::get_layout_state(ui, None);
            settled = state
                .base
                .last_avg_displacement
                .is_some_and(|step| step < SETTLED)
                || state.base.step_count > MAX_STEPS;
        });
        settled
    }

    pub fn element(&self, frame: Frame, view: EntityId) -> Canvas<Hitbox> {
        self.host.element(frame, view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cookies::{Cookie, Tree};
    use crate::theme::Theme;

    fn cookie(domain: &str, name: &str) -> Cookie {
        Cookie {
            name: name.into(),
            domain: domain.into(),
            path: "/".into(),
            ..Default::default()
        }
    }

    fn tree() -> Tree {
        Tree::build(&[
            cookie("mail.example.com", "a"),
            cookie("mail.example.com", "b"),
            cookie("example.com", "c"),
            cookie("other.org", "d"),
        ])
    }

    fn settle(graph: &mut CookieGraph) -> Shown {
        let palette = Theme::default().palette();
        let mut shown = graph.show(palette, true, true);
        for _ in 0..400 {
            if !shown.animating {
                break;
            }
            shown = graph.show(palette, true, true);
        }
        shown
    }

    #[test]
    fn the_graph_follows_the_visible_rows() {
        let tree = tree();
        let mut graph = CookieGraph::default();
        let mut open = HashSet::from([Node::All]);

        graph.sync(&tree.rows(&open), &Node::All);
        assert_eq!((graph.graph.node_count(), graph.graph.edge_count()), (3, 2));
        let payloads = graph.payloads();
        assert_eq!(
            (payloads[0].depth, payloads[0].count, payloads[0].closed),
            (0, 4, false)
        );
        assert!(
            payloads[1..]
                .iter()
                .all(|site| site.depth == 1 && site.closed)
        );

        // Opening a site adds its hosts, each joined to the site.
        open.insert(Node::Site("example.com".into()));
        graph.sync(&tree.rows(&open), &Node::All);
        assert_eq!((graph.graph.node_count(), graph.graph.edge_count()), (5, 4));
        let site = graph.nodes[&Node::Site("example.com".into())];
        let host = graph.nodes[&Node::Host("mail.example.com".into())];
        assert_eq!(graph.graph.edges_connecting(site, host).count(), 1);

        // Closing it removes them again, and an unchanged sync changes nothing.
        open.remove(&Node::Site("example.com".into()));
        graph.sync(&tree.rows(&open), &Node::All);
        graph.unsettled = false;
        graph.sync(&tree.rows(&open), &Node::All);
        assert_eq!((graph.graph.node_count(), graph.graph.edge_count()), (3, 2));
        assert!(!graph.unsettled, "nothing moved, so the layout may rest");
        assert!(
            !graph
                .nodes
                .contains_key(&Node::Host("mail.example.com".into()))
        );
    }

    #[test]
    fn an_empty_store_is_a_single_root_node() {
        let mut graph = CookieGraph::default();
        graph.sync(
            &Tree::default().rows(&HashSet::from([Node::All])),
            &Node::All,
        );
        assert_eq!((graph.graph.node_count(), graph.graph.edge_count()), (1, 0));
        let shown = settle(&mut graph);
        assert!(!shown.animating);
        assert_eq!(shown.frame.labels.len(), 1);
        assert_eq!(shown.frame.labels[0].text, "All cookies  0");
        // No rows at all (before the first sync) is no nodes, and still a valid frame.
        let mut empty = CookieGraph::default();
        assert!(
            empty
                .show(Theme::default().palette(), true, true)
                .frame
                .labels
                .is_empty()
        );
    }

    #[test]
    fn existing_nodes_keep_their_place_and_follow_the_selection() {
        let tree = tree();
        let mut graph = CookieGraph::default();
        let mut open = HashSet::from([Node::All]);
        graph.sync(&tree.rows(&open), &Node::All);
        settle(&mut graph);
        let site = graph.nodes[&Node::Site("example.com".into())];
        let before = graph.graph.node(site).unwrap().location();

        open.insert(Node::Site("other.org".into()));
        graph.sync(&tree.rows(&open), &Node::Site("example.com".into()));
        assert_eq!(graph.graph.node(site).unwrap().location(), before);
        assert!(graph.graph.node(site).unwrap().selected());
        let root = graph.nodes[&Node::All];
        assert!(!graph.graph.node(root).unwrap().selected());
        // The new host starts beside its site, not on top of it or of a sibling.
        let other = graph.nodes[&Node::Site("other.org".into())];
        let host = graph.nodes[&Node::Host("other.org".into())];
        let gap = graph.graph.node(host).unwrap().location()
            - graph.graph.node(other).unwrap().location();
        assert!((30. ..80.).contains(&gap.length()), "{gap:?}");
    }

    #[test]
    fn the_layout_settles_and_draws_every_node_with_its_label() {
        let tree = tree();
        let mut graph = CookieGraph::default();
        let open: HashSet<Node> = [
            Node::All,
            Node::Site("example.com".into()),
            Node::Site("other.org".into()),
            Node::Host("mail.example.com".into()),
        ]
        .into();
        let rows = tree.rows(&open);
        graph.sync(&rows, &Node::All);
        let shown = settle(&mut graph);
        assert!(!shown.animating, "the simulation stops by itself");
        assert_eq!(shown.frame.labels.len(), rows.len());
        let texts: Vec<&str> = shown
            .frame
            .labels
            .iter()
            .map(|label| label.text.as_str())
            .collect();
        for expected in [
            "All cookies  4",
            "example.com  3",
            "mail.example.com  2",
            "a",
        ] {
            assert!(texts.contains(&expected), "{expected} in {texts:?}");
        }
        // Nodes ended up apart from each other, and finite.
        let places: Vec<Pos2> = graph
            .graph
            .nodes_iter()
            .map(|(_, node)| node.location())
            .collect();
        for (index, a) in places.iter().enumerate() {
            assert!(a.x.is_finite() && a.y.is_finite());
            for b in &places[index + 1..] {
                assert!((*a - *b).length() > 8., "{a:?} {b:?}");
            }
        }
        // A settled graph stays still: no more frames are asked for.
        assert!(!graph.show(Theme::default().palette(), true, true).animating);
    }

    #[test]
    fn without_animation_the_layout_settles_in_one_frame() {
        let tree = tree();
        let mut graph = CookieGraph::default();
        let open: HashSet<Node> = [Node::All, Node::Site("example.com".into())].into();
        graph.sync(&tree.rows(&open), &Node::All);
        let palette = Theme::default().palette();
        let first = graph.show(palette, false, true);
        assert!(!first.animating, "Reduce Motion: nothing moves on screen");
        let site = graph.nodes[&Node::Site("example.com".into())];
        let settled = graph.graph.node(site).unwrap().location();
        graph.show(palette, false, true);
        assert_eq!(graph.graph.node(site).unwrap().location(), settled);
    }

    #[test]
    fn without_animation_a_large_layout_still_settles() {
        // 1 root, 1 site, 3 hosts and 300 cookies: more than one frame's budget settles.
        let cookies: Vec<Cookie> = ["a", "b", "c"]
            .iter()
            .flat_map(|host| {
                (0..100)
                    .map(move |index| cookie(&format!("{host}.example.com"), &format!("c{index}")))
            })
            .collect();
        let tree = Tree::build(&cookies);
        let mut open = HashSet::from([Node::All, Node::Site("example.com".into())]);
        for host in ["a", "b", "c"] {
            open.insert(Node::Host(format!("{host}.example.com")));
        }
        let rows = tree.rows(&open);
        assert_eq!(rows.len(), 305);
        let mut graph = CookieGraph::default();
        graph.sync(&rows, &Node::All);
        let palette = Theme::default().palette();
        let mut frames = 1;
        while graph.show(palette, false, true).animating {
            frames += 1;
            assert!(frames < 1_000, "the layout stops");
        }
        assert!(
            graph.settled(),
            "Reduce Motion must not leave the layout half made ({frames} frames)"
        );
        // Once settled it stays still and asks for no more frames.
        assert!(!graph.show(palette, false, true).animating);
    }

    #[test]
    fn nodes_keep_a_sensible_size_on_screen_at_any_zoom() {
        let mut graph = Graph::new();
        let index = graph.add_node_with_label_and_location(
            Payload {
                node: Node::All,
                depth: 0,
                count: 9,
                closed: false,
            },
            String::new(),
            Pos2::new(100., 100.),
        );
        let mut shape = graph.node(index).unwrap().display().clone();
        // The view refreshes each shape from its node's properties before drawing it.
        DisplayNode::update(&mut shape, graph.node(index).unwrap().props());
        let base = radius(0, 9);
        for (zoom, on_screen) in [
            (0.1, 0.85),
            (0.85, 0.85),
            (1., 1.),
            (1.25, 1.25),
            (5., 1.25),
        ] {
            shape.zoom = zoom;
            assert!(
                (shape.radius() * zoom - base * on_screen).abs() < 1e-3,
                "{zoom}"
            );
            // Hit-testing follows the drawn size: just inside hits, well outside misses.
            let edge = shape.radius();
            assert!(shape.is_inside(Pos2::new(100. + edge, 100.)));
            assert!(!shape.is_inside(Pos2::new(100. + edge + 4., 100.)));
            let boundary = shape.closest_boundary_point(Vec2::new(0., -3.));
            assert!((boundary.y - (100. - edge)).abs() < 1e-3);
        }
    }

    #[test]
    fn labels_avoid_the_branch_they_belong_to() {
        let right = Vec2::new(40., 0.);
        // The root, and a node on top of it, hang their label below.
        assert_eq!(label_direction(right, 0, true), Vec2::DOWN);
        assert_eq!(label_direction(Vec2::ZERO, 2, false), Vec2::DOWN);
        assert_eq!(
            label_direction(Vec2::new(f32::NAN, 1.), 2, false),
            Vec2::DOWN
        );
        // A leaf continues outward.
        assert_eq!(label_direction(right, 3, false), Vec2::new(1., 0.));
        assert_eq!(
            label_direction(Vec2::new(0., -9.), 1, false),
            Vec2::new(0., -1.)
        );
        // An open branch goes sideways, away from the root's side of it.
        for from_root in [
            Vec2::new(30., -30.),
            Vec2::new(-5., 50.),
            Vec2::new(-40., -3.),
            Vec2::new(0., -20.),
        ] {
            let toward = label_direction(from_root, 1, true);
            assert!(toward.dot(from_root).abs() < 1e-3, "{from_root:?}");
            assert!((toward.length() - 1.).abs() < 1e-4);
            assert_eq!(toward.x < 0., from_root.x < 0., "{from_root:?}");
        }
    }

    #[test]
    fn node_sizes_grow_slowly_and_stay_bounded() {
        assert!(radius(0, 0) > radius(1, 1));
        assert!(radius(1, 50) > radius(1, 2));
        assert!(radius(1, 2) > radius(2, 2));
        assert!(radius(2, 1) > radius(3, 1));
        assert_eq!(radius(1, 1_000_000), 18.);
        assert_eq!(radius(2, 1_000_000), 13.);
        assert_eq!(radius(3, 99), radius(9, 0));
    }
}
