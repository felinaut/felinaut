// Adapted from the Zed GPUI 0.2.2 examples/input.rs TextInput example.
// Copyright Zed Industries. Licensed under the Apache License, Version 2.0 (Apache-2.0).
// https://www.apache.org/licenses/LICENSE-2.0

use std::ops::Range;

use crate::theme::{self, ThemeSettings};
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::UnicodeSegmentation;

actions!(
    address_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteToStart,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        SubmitAddress,
        Up,
        Down,
        Escape,
    ]
);

/// A submitted address or search query. Navigation is handled by the subscriber.
pub struct Submit(pub String);

/// Typing and suggestion-list keys; the browser owns the suggestions themselves.
pub enum AddressEvent {
    /// The user edited the text (not a programmatic `set_text`).
    Changed,
    Up,
    Down,
    Escape,
}

/// Register editing shortcuts scoped to the address input's key context.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("AddressInput")),
        KeyBinding::new("delete", Delete, Some("AddressInput")),
        KeyBinding::new("left", Left, Some("AddressInput")),
        KeyBinding::new("right", Right, Some("AddressInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("AddressInput")),
        KeyBinding::new("shift-right", SelectRight, Some("AddressInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("AddressInput")),
        KeyBinding::new("cmd-v", Paste, Some("AddressInput")),
        KeyBinding::new("cmd-c", Copy, Some("AddressInput")),
        KeyBinding::new("cmd-x", Cut, Some("AddressInput")),
        KeyBinding::new("home", Home, Some("AddressInput")),
        KeyBinding::new("end", End, Some("AddressInput")),
        KeyBinding::new("cmd-left", Home, Some("AddressInput")),
        KeyBinding::new("cmd-right", End, Some("AddressInput")),
        KeyBinding::new("shift-home", SelectHome, Some("AddressInput")),
        KeyBinding::new("shift-end", SelectEnd, Some("AddressInput")),
        KeyBinding::new("cmd-shift-left", SelectHome, Some("AddressInput")),
        KeyBinding::new("cmd-shift-right", SelectEnd, Some("AddressInput")),
        KeyBinding::new("alt-backspace", DeleteWordLeft, Some("AddressInput")),
        KeyBinding::new("cmd-backspace", DeleteToStart, Some("AddressInput")),
        KeyBinding::new("alt-left", WordLeft, Some("AddressInput")),
        KeyBinding::new("alt-right", WordRight, Some("AddressInput")),
        KeyBinding::new("alt-shift-left", SelectWordLeft, Some("AddressInput")),
        KeyBinding::new("alt-shift-right", SelectWordRight, Some("AddressInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("AddressInput")),
        KeyBinding::new("enter", SubmitAddress, Some("AddressInput")),
        KeyBinding::new("up", Up, Some("AddressInput")),
        KeyBinding::new("down", Down, Some("AddressInput")),
        KeyBinding::new("escape", Escape, Some("AddressInput")),
    ]);
}

pub struct AddressInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// Inline autocomplete drawn dimmed around the typed text: `goog·le·.com` for `le`.
    ghost: Option<(String, String)>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    _theme_observer: gpui::Subscription,
}

impl EventEmitter<Submit> for AddressInput {}
impl EventEmitter<AddressEvent> for AddressInput {}

impl AddressInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: "Search or enter an address".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            ghost: None,
            last_layout: None,
            last_bounds: None,
            is_selecting: false,
            _theme_observer: cx.observe_global::<ThemeSettings>(|_, cx| cx.notify()),
        }
    }

    /// Replace the contents and place the cursor at the end, clearing composition.
    pub fn set_text(&mut self, text: String, cx: &mut Context<Self>) {
        let end = text.len();
        self.content = text.into();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.ghost = None;
        self.last_layout = None;
        self.last_bounds = None;
        self.is_selecting = false;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Whether the caret sits at the end of the text with no composition in progress, the only
    /// place inline autocomplete makes sense.
    pub fn caret_at_end(&self) -> bool {
        self.selected_range.end == self.content.len() && self.marked_range.is_none()
    }

    /// Inline autocomplete: `prefix` and `suffix` are drawn dimmed around the typed text and are
    /// not part of it, so editing keeps working on what was typed. `→` at the end accepts them.
    pub fn set_completion(&mut self, ghost: Option<(String, String)>, cx: &mut Context<Self>) {
        if self.ghost != ghost {
            self.ghost = ghost;
            cx.notify();
        }
    }

    /// Bytes of ghost text drawn before the content, to map between content and layout indices.
    fn ghost_offset(&self) -> usize {
        match &self.ghost {
            Some((prefix, _)) if !self.content.is_empty() => prefix.len(),
            _ => 0,
        }
    }

    /// Window-relative bounds of the text line from the last paint, for anchoring popups.
    pub fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.last_bounds
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn submit(&mut self, _: &SubmitAddress, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Submit(self.content.to_string()));
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(AddressEvent::Up);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(AddressEvent::Down);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(AddressEvent::Escape);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() && self.caret_at_end() && self.ghost.is_some() {
            // Accept the completion.
            let (prefix, suffix) = self.ghost.take().unwrap();
            let full = format!("{prefix}{}{suffix}", self.content);
            self.selected_range = 0..self.content.len();
            self.replace_text_in_range(None, &full, window, cx);
        } else if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.select_all_text(cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(previous_word(&self.content, self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(next_word(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(previous_word(&self.content, self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(next_word(&self.content, self.cursor_offset()), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(previous_word(&self.content, self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(0, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\n", " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
            .saturating_sub(self.ghost_offset())
            .min(self.content.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

/// Start of the word before `offset`, as Option+Left finds it. Punctuation and spaces are not words.
fn previous_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices()
        .rev()
        .find_map(|(start, word)| (start < offset && is_word(word)).then_some(start))
        .unwrap_or(0)
}

/// End of the word after `offset`, as Option+Right finds it.
fn next_word(text: &str, offset: usize) -> usize {
    text.split_word_bound_indices()
        .find_map(|(start, word)| {
            let end = start + word.len();
            (end > offset && is_word(word)).then_some(end)
        })
        .unwrap_or(text.len())
}

fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

impl EntityInputHandler for AddressInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.selection_reversed = false;
        self.marked_range.take();
        self.ghost = None;
        cx.emit(AddressEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                // IME selection offsets are relative to the inserted text, not the buffer.
                let byte_offset = |offset: usize| {
                    let mut utf16_count = 0;
                    let mut utf8_offset = 0;
                    for ch in new_text.chars() {
                        if utf16_count >= offset {
                            break;
                        }
                        utf16_count += ch.len_utf16();
                        utf8_offset += ch.len_utf8();
                    }
                    range.start + utf8_offset
                };
                byte_offset(range_utf16.start)..byte_offset(range_utf16.end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        self.selection_reversed = false;
        self.ghost = None;
        cx.emit(AddressEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let offset = self.ghost_offset();
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start + offset),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end + offset),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line_point = bounds.localize(&point)?;
        if self.content.is_empty() {
            return Some(0);
        }
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout
            .index_for_x(line_point.x)?
            .saturating_sub(self.ghost_offset())
            .min(self.content.len());
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<AddressInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let offset = input.ghost_offset();
        let selected_range = input.selected_range.start + offset..input.selected_range.end + offset;
        let cursor = input.cursor_offset() + offset;
        let style = window.text_style();
        let p = theme::palette(cx);

        let ghost = input.ghost.clone().filter(|_| !content.is_empty());
        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), rgb(p.muted).into())
        } else if let Some((prefix, suffix)) = &ghost {
            (format!("{prefix}{content}{suffix}").into(), style.color)
        } else {
            (content.clone(), style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some((prefix, suffix)) = &ghost {
            let dim = rgb(p.muted).into();
            vec![
                TextRun {
                    len: prefix.len(),
                    color: dim,
                    ..run.clone()
                },
                TextRun {
                    len: content.len(),
                    ..run.clone()
                },
                TextRun {
                    len: suffix.len(),
                    color: dim,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else if let Some(marked_range) = input.marked_range.as_ref() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    rgb(p.text),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba((p.secondary << 8) | 0x40),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for AddressInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = theme::palette(cx);
        div()
            .flex()
            .key_context("AddressInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::escape))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .min_w_0()
            .rounded_lg()
            .bg(rgb(p.input))
            .text_color(rgb(p.text))
            .border_1()
            .border_color(rgb(p.border))
            .line_height(px(24.))
            .text_size(px(14.))
            .px_3()
            // 24 + 2 × 4 + border = 34 pt, matching the toolbar buttons and the title chip.
            .py(px(4.))
            .overflow_hidden()
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for AddressInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{next_word, previous_word};

    #[test]
    fn word_motion_stops_at_words_not_punctuation() {
        let text = "https://example.com/foo bar";
        assert_eq!(previous_word(text, text.len()), 24);
        assert_eq!(previous_word(text, 24), 20);
        assert_eq!(previous_word(text, 8), 0);
        assert_eq!(previous_word(text, 0), 0);
        assert_eq!(next_word(text, 0), 5);
        assert_eq!(next_word(text, 5), 19);
        assert_eq!(next_word(text, 23), text.len());
        assert_eq!(next_word(text, text.len()), text.len());
    }
}
