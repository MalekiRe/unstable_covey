//! Widgets: a behavior plus the components it reads (headless), with
//! default styles from [`theme`]. Spawn them with the `spawn_*` functions;
//! add [`ControlsPlugin`] and [`ContainersPlugin`] (or [`widgets`]).
//!
//! - Button: [`Clicks`] counts activations (click, Enter or Space).
//! - Checkbox: [`Checked`]; the mark is a child opened and closed.
//! - Slider: [`SliderValue`] within [`SliderRange`], dragged or clicked.
//! - Dropdown: the option list is built once, `Closed`; opening it is one
//!   `open`, picking races the options' clicks, nothing is spawned per use.
//! - Text field: [`TextValue`], [`Cursor`] with selection, keyboard editing.
//! - Virtual list: a pool of rows recycled as it scrolls ([`RowIndex`]).
//! - Tabs: one panel open, the rest `Closed`.
//! - Tooltip and modal: floating; the tooltip lives in its behavior's scope.
//! - Inspector: a text listing every UI entity, closed ones included
//!   (`MayHave<Closed>`).
use std::future::poll_fn;
use std::marker::PhantomData;
use std::pin::pin;
use std::task::Poll;
use std::time::Duration;

use static_ecs::prelude::*;
use static_ecs::{Behaving, Behavior, Components, Cx, Inbox, Wakeup, behavior};
use static_ui_layout::Vec2;

use crate::behave::{self, Scope, UiUpdate, select, select_all, sleep};
use crate::input::Key;
use crate::interact::{Click, Drag, DragEnd, DragStart, FocusIn, FocusOut, Focusable, Interaction, KeyPress, PointerEnter, PointerLeave, Press, Release, TextInput};
use crate::{
    AlignX, AlignY, Attach, AttachPoint, Background, Border, BorderWidth, ChildOf, Clip, Closed, Color, El, Floating, Measure, Monospace, Node,
    Overlay, Padding, Radius, Rect, Scroll, Sizing, SpawnUi, Text, TextConfig, UiFrame, UiLayout, el,
};

/// Default colors and sizes.
pub mod theme {
    use crate::Color;
    pub const TEXT: Color = Color::rgb(30, 30, 35);
    pub const PANEL: Color = Color::rgb(245, 245, 248);
    pub const FIELD: Color = Color::rgb(255, 255, 255);
    pub const BORDER: Color = Color::rgb(190, 190, 200);
    pub const ACCENT: Color = Color::rgb(66, 133, 244);
    pub const BUTTON: Color = Color::rgb(225, 225, 232);
    pub const BUTTON_HOVER: Color = Color::rgb(210, 210, 222);
    pub const BUTTON_PRESSED: Color = Color::rgb(185, 185, 202);
    pub const SELECTION: Color = Color::rgb(180, 205, 250);
    pub const BACKDROP: Color = Color::rgba(0, 0, 0, 110);
    pub const TEXT_SIZE: u16 = 16;
}

fn label(text: &str) -> Text {
    Text::new(text, theme::TEXT_SIZE).color(theme::TEXT)
}
fn child<S: Storage>(cx: Cx<S>, parent: Entity, index: usize) -> Option<Entity> {
    cx.with(move |mut q: Query<&Sources<ChildOf>>| q.get_mut(parent).and_then(|s| s.sources().nth(index))).ok().flatten()
}
fn children<S: Storage>(cx: Cx<S>, parent: Entity) -> Vec<Entity> {
    cx.with(move |mut q: Query<&Sources<ChildOf>>| q.get_mut(parent).map(|s| s.sources().collect()).unwrap_or_default())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- Button

/// A button's behavior marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct Button;
/// Colors a button shows at rest, hovered, and pressed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonColors {
    pub normal: Color,
    pub hovered: Color,
    pub pressed: Color,
}
impl Default for ButtonColors {
    fn default() -> Self {
        Self { normal: theme::BUTTON, hovered: theme::BUTTON_HOVER, pressed: theme::BUTTON_PRESSED }
    }
}
/// How often the button was activated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clicks(pub u32);

/// A labeled button.
pub type ButtonTree = El<(Node, Background, Radius, Button, ButtonColors, Clicks, Interaction, Focusable), (El<(Text,)>,)>;
pub fn button(text: &str) -> ButtonTree {
    el((
        Node::row().padding(Padding::xy(14, 8)).align(AlignX::Center, AlignY::Center),
        Background(theme::BUTTON),
        Radius::all(6.0),
        Button,
        ButtonColors::default(),
        Clicks(0),
        Interaction::default(),
        Focusable,
    ))
    .children((el((label(text),)),))
}

enum ButtonEvent {
    Enter,
    Leave,
    Press,
    Release,
    Activate,
}
impl Behavior for Button {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let (mut hovered, mut pressed) = (false, false);
        loop {
            let event = {
                let mut enter = pin!(cx.on::<PointerEnter>(me));
                let mut leave = pin!(cx.on::<PointerLeave>(me));
                let mut press = pin!(cx.on::<Press>(me));
                let mut release = pin!(cx.on::<Release>(me));
                let mut click = pin!(cx.on::<Click>(me));
                let mut key = pin!(cx.on::<KeyPress>(me));
                poll_fn(|c| {
                    if enter.as_mut().poll(c).is_ready() {
                        return Poll::Ready(ButtonEvent::Enter);
                    }
                    if leave.as_mut().poll(c).is_ready() {
                        return Poll::Ready(ButtonEvent::Leave);
                    }
                    if press.as_mut().poll(c).is_ready() {
                        return Poll::Ready(ButtonEvent::Press);
                    }
                    if release.as_mut().poll(c).is_ready() {
                        return Poll::Ready(ButtonEvent::Release);
                    }
                    if click.as_mut().poll(c).is_ready() {
                        return Poll::Ready(ButtonEvent::Activate);
                    }
                    if let Poll::Ready(KeyPress { key: Key::Enter | Key::Space, .. }) = key.as_mut().poll(c) {
                        return Poll::Ready(ButtonEvent::Activate);
                    }
                    Poll::Pending
                })
                .await
            };
            match event {
                ButtonEvent::Enter => hovered = true,
                ButtonEvent::Leave => (hovered, pressed) = (false, false),
                ButtonEvent::Press => pressed = true,
                ButtonEvent::Release => pressed = false,
                ButtonEvent::Activate => {
                    let _ = cx.with(|mut q: Query<&mut Clicks>| {
                        if let Some(mut clicks) = q.get_mut(me) {
                            clicks.0 += 1;
                        }
                    });
                }
            }
            // One write restyles all three states.
            let _ = cx.with(|mut q: Query<(&ButtonColors, &mut Background)>| {
                if let Some((colors, mut background)) = q.get_mut(me) {
                    let color = if pressed {
                        colors.pressed
                    } else if hovered {
                        colors.hovered
                    } else {
                        colors.normal
                    };
                    if background.0 != color {
                        background.0 = color;
                    }
                }
            });
        }
    }
}

// -------------------------------------------------------------- Checkbox

/// A checkbox's behavior marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct Checkbox;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checked(pub bool);
pub type CheckboxTree = El<(Node, Background, Border, Radius, Checkbox, Checked, Interaction, Focusable), (El<(Node, Background, Radius)>,)>;
pub fn checkbox(checked: bool) -> CheckboxTree {
    el((
        Node::row().width(Sizing::fixed(20.0)).height(Sizing::fixed(20.0)).align(AlignX::Center, AlignY::Center),
        Background(theme::FIELD),
        Border { color: theme::BORDER, width: BorderWidth::all(2) },
        Radius::all(4.0),
        Checkbox,
        Checked(checked),
        Interaction::default(),
        Focusable,
    ))
    .children((el((Node::row().width(Sizing::fixed(10.0)).height(Sizing::fixed(10.0)), Background(theme::ACCENT), Radius::all(2.0))),))
}
impl Behavior for Checkbox {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let show = |checked: bool| {
            if let Some(mark) = child(cx, me, 0) {
                if checked {
                    behave::open(cx, mark);
                } else {
                    behave::close(cx, mark);
                }
            }
        };
        let checked = cx.with(|mut q: Query<&Checked>| q.get_mut(me).is_some_and(|c| c.0)).unwrap_or(false);
        show(checked);
        loop {
            match select(cx.on::<Click>(me), cx.on::<KeyPress>(me)).await {
                behave::Either::Left(_) | behave::Either::Right(KeyPress { key: Key::Space, .. }) => {}
                behave::Either::Right(_) => continue,
            }
            let checked = cx
                .with(|mut q: Query<&mut Checked>| {
                    let mut checked = q.get_mut(me)?;
                    checked.0 = !checked.0;
                    Some(checked.0)
                })
                .ok()
                .flatten()
                .unwrap_or(false);
            show(checked);
        }
    }
}

// ---------------------------------------------------------------- Slider

/// A slider's behavior marker (on the track).
#[derive(Clone, Copy, Debug, Default)]
pub struct Slider;
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliderValue(pub f32);
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderRange {
    pub min: f32,
    pub max: f32,
}
impl Default for SliderRange {
    fn default() -> Self {
        Self { min: 0.0, max: 1.0 }
    }
}
/// The thumb's width, in pixels.
pub const THUMB: f32 = 16.0;
/// The track's width, in pixels.
pub const TRACK: f32 = 200.0;
pub type SliderTree = El<(Node, Background, Radius, Slider, SliderValue, SliderRange, Interaction), (El<(Node, Background, Radius)>, El<(Node, Background, Radius)>)>;
/// A slider in `range`, starting at `value`: a track, a filled part, a thumb.
pub fn slider(range: SliderRange, value: f32) -> SliderTree {
    let fraction = ((value - range.min) / (range.max - range.min)).clamp(0.0, 1.0);
    el((
        Node::row().width(Sizing::fixed(TRACK)).height(Sizing::fixed(20.0)).align(AlignX::Left, AlignY::Center),
        Background(theme::BUTTON),
        Radius::all(10.0),
        Slider,
        SliderValue(value),
        range,
        Interaction::default(),
    ))
    .children((
        el((Node::row().width(Sizing::fixed(fraction * (TRACK - THUMB))).height(Sizing::fixed(4.0)), Background(theme::ACCENT), Radius::all(2.0))),
        el((Node::row().width(Sizing::fixed(THUMB)).height(Sizing::fixed(THUMB)), Background(theme::ACCENT), Radius::all(THUMB / 2.0))),
    ))
}
/// Set the slider from a pointer x position.
fn slide_to<S: Storage>(cx: Cx<S>, me: Entity, x: f32) {
    let fill = child(cx, me, 0);
    let _ = cx.with(move |mut sliders: Query<(&Rect, &SliderRange, &mut SliderValue)>, mut nodes: Query<&mut Node>| {
        let Some((rect, range, mut value)) = sliders.get_mut(me) else { return };
        let travel = (rect.0.width - THUMB).max(1.0);
        let fraction = ((x - rect.0.x - THUMB / 2.0) / travel).clamp(0.0, 1.0);
        let next = range.min + fraction * (range.max - range.min);
        if value.0 != next {
            value.0 = next;
        }
        if let Some(fill) = fill
            && let Some(mut node) = nodes.get_mut(fill)
        {
            let width = Sizing::fixed(fraction * travel);
            if node.0.width != width {
                node.0.width = width;
            }
        }
    });
}
impl Behavior for Slider {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            match select(cx.on::<Press>(me), cx.on::<DragStart>(me)).await {
                behave::Either::Left(press) => slide_to(cx, me, press.position.x),
                behave::Either::Right(_) => {
                    while let behave::Either::Left(drag) = select(cx.on::<Drag>(me), cx.on::<DragEnd>(me)).await {
                        slide_to(cx, me, drag.position.x);
                    }
                }
            }
        }
    }
}

// -------------------------------------------------------------- Dropdown

/// A dropdown's behavior marker (on its header).
#[derive(Clone, Copy, Debug, Default)]
pub struct Dropdown;
/// The chosen option's index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selected(pub usize);
/// Spawn a dropdown under `parent`: a header showing the choice, and a
/// floating option list built once and closed.
pub fn spawn_dropdown<S: Storage>(world: &mut World<S>, parent: Option<Entity>, options: &[&str], selected: usize) -> Entity {
    let header = el((
        Node::row().width(Sizing::fixed(160.0)).padding(Padding::xy(10, 6)),
        Background(theme::FIELD),
        Border { color: theme::BORDER, width: BorderWidth::all(1) },
        Radius::all(4.0),
        Interaction::default(),
        Selected(selected),
        Dropdown,
    ))
    .children((el((label(options.get(selected).copied().unwrap_or("")),)),));
    let header = match parent {
        Some(parent) => world.spawn_ui_under(parent, header),
        None => world.spawn_ui(header),
    };
    let list = world.spawn_ui_under(
        header,
        el((
            Node::column().width(Sizing::fixed(160.0)).padding(Padding::all(4)),
            Background(theme::FIELD),
            Border { color: theme::BORDER, width: BorderWidth::all(1) },
            Floating { attach: Attach::Parent, element: AttachPoint::LeftTop, parent: AttachPoint::LeftBottom, z: 10, ..Floating::default() },
        )),
    );
    for option in options {
        world.spawn_ui_under(
            list,
            el((Node::row().width(Sizing::GROW).padding(Padding::xy(6, 4)), Background(theme::FIELD), Interaction::default()))
                .children((el((label(option),)),)),
        );
    }
    world.disable_subtree::<Closed, ChildOf>(list);
    header
}
impl Behavior for Dropdown {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<Click>(me).await;
            let (Some(text), Some(list)) = (child(cx, me, 0), child(cx, me, 1)) else { continue };
            behave::open(cx, list);
            let options = children(cx, list);
            // An option, or the header again (which just closes).
            let picked = select(select_all(options.iter().map(|&o| cx.on::<Click>(o))), cx.on::<Click>(me)).await;
            if let behave::Either::Left((index, _)) = picked {
                let chosen = child(cx, options[index], 0);
                let _ = cx.with(move |mut selected: Query<&mut Selected>, mut texts: Query<&mut Text>| {
                    if let Some(mut selected) = selected.get_mut(me) {
                        selected.0 = index;
                    }
                    let value = chosen.and_then(|c| texts.get_mut(c).map(|t| t.text.clone()));
                    if let (Some(value), Some(mut header)) = (value, texts.get_mut(text)) {
                        header.text = value;
                    }
                });
            }
            behave::close(cx, list);
        }
    }
}

// ------------------------------------------------------------ Text field

/// A text field's behavior marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextField;
/// The field's text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextValue(pub String);
/// Cursor and selection, in characters: the selection runs from `anchor`
/// to `position`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub position: usize,
    pub anchor: usize,
}
impl Cursor {
    pub fn selection(self) -> (usize, usize) {
        (self.position.min(self.anchor), self.position.max(self.anchor))
    }
}
/// The field's text, selection and caret children, found by the systems.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldMarks;
pub type FieldTree = El<
    (Node, Background, Border, Radius, Clip, TextField, TextValue, Cursor, Interaction, Focusable, FieldMarks),
    (El<(Node, Background, Floating)>, El<(Text,)>, El<(Node, Background, Floating)>),
>;
/// A single-line text field `width` pixels wide.
pub fn text_field(value: &str, width: f32) -> FieldTree {
    let end = value.chars().count();
    el((
        Node::row().width(Sizing::fixed(width)).height(Sizing::fixed(30.0)).padding(Padding::xy(6, 4)).align(AlignX::Left, AlignY::Center),
        Background(theme::FIELD),
        Border { color: theme::BORDER, width: BorderWidth::all(1) },
        Radius::all(4.0),
        Clip { horizontal: true, vertical: false },
        TextField,
        TextValue(value.into()),
        Cursor { position: end, anchor: end },
        Interaction::default(),
        Focusable,
        FieldMarks,
    ))
    .children((
        el((Node::row().width(Sizing::fixed(0.0)).height(Sizing::fixed(20.0)), Background(theme::SELECTION), Floating { z: -1, ..Floating::default() })),
        el((label(value),)),
        el((Node::row().width(Sizing::fixed(1.0)).height(Sizing::fixed(18.0)), Background(theme::TEXT), Floating { z: 1, ..Floating::default() })),
    ))
}
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices().nth(chars).map_or(text.len(), |(i, _)| i)
}
/// Apply a key to a value and cursor.
pub fn edit(value: &mut String, cursor: &mut Cursor, key: Key, shift: bool) {
    let len = value.chars().count();
    let (start, end) = cursor.selection();
    let collapse = |cursor: &mut Cursor, to: usize, shift: bool| {
        cursor.position = to;
        if !shift {
            cursor.anchor = to;
        }
    };
    match key {
        Key::Left if !shift && start != end => collapse(cursor, start, false),
        Key::Right if !shift && start != end => collapse(cursor, end, false),
        Key::Left => collapse(cursor, cursor.position.saturating_sub(1), shift),
        Key::Right => collapse(cursor, (cursor.position + 1).min(len), shift),
        Key::Home => collapse(cursor, 0, shift),
        Key::End => collapse(cursor, len, shift),
        Key::Backspace | Key::Delete => {
            let (from, to) = if start != end {
                (start, end)
            } else if key == Key::Backspace {
                (start.saturating_sub(1), start)
            } else {
                (start, (start + 1).min(len))
            };
            value.replace_range(byte_at(value, from)..byte_at(value, to), "");
            *cursor = Cursor { position: from, anchor: from };
        }
        _ => {}
    }
}
/// Type `ch` over the selection.
pub fn type_char(value: &mut String, cursor: &mut Cursor, ch: char) {
    let (start, end) = cursor.selection();
    let mut buffer = [0u8; 4];
    value.replace_range(byte_at(value, start)..byte_at(value, end), ch.encode_utf8(&mut buffer));
    *cursor = Cursor { position: start + 1, anchor: start + 1 };
}
impl Behavior for TextField {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let caret = child(cx, me, 2);
        if let Some(caret) = caret {
            behave::close(cx, caret);
        }
        loop {
            let mut key = pin!(cx.on::<KeyPress>(me));
            let mut text = pin!(cx.on::<TextInput>(me));
            let mut focus_in = pin!(cx.on::<FocusIn>(me));
            let mut focus_out = pin!(cx.on::<FocusOut>(me));
            enum Event {
                Key(KeyPress),
                Text(char),
                Focus(bool),
            }
            let event = poll_fn(|c| {
                if let Poll::Ready(k) = key.as_mut().poll(c) {
                    return Poll::Ready(Event::Key(k));
                }
                if let Poll::Ready(TextInput(ch)) = text.as_mut().poll(c) {
                    return Poll::Ready(Event::Text(ch));
                }
                if focus_in.as_mut().poll(c).is_ready() {
                    return Poll::Ready(Event::Focus(true));
                }
                if focus_out.as_mut().poll(c).is_ready() {
                    return Poll::Ready(Event::Focus(false));
                }
                Poll::Pending
            })
            .await;
            match event {
                Event::Focus(focused) => {
                    if let Some(caret) = caret {
                        if focused {
                            behave::open(cx, caret);
                        } else {
                            behave::close(cx, caret);
                        }
                    }
                }
                Event::Key(KeyPress { key, modifiers }) => {
                    let _ = cx.with(|mut q: Query<(&mut TextValue, &mut Cursor)>| {
                        if let Some((mut value, mut cursor)) = q.get_mut(me) {
                            let (mut v, mut c) = (value.0.clone(), *cursor);
                            edit(&mut v, &mut c, key, modifiers.shift);
                            if v != value.0 {
                                value.0 = v;
                            }
                            if c != *cursor {
                                *cursor = c;
                            }
                        }
                    });
                }
                Event::Text(ch) => {
                    let _ = cx.with(|mut q: Query<(&mut TextValue, &mut Cursor)>| {
                        if let Some((mut value, mut cursor)) = q.get_mut(me) {
                            type_char(&mut value.0, &mut cursor, ch);
                        }
                    });
                }
            }
        }
    }
}
/// `UiFrame`, before layout: show a changed field's text, and place its
/// caret and selection by measuring the text before them.
#[allow(clippy::type_complexity)]
pub fn field_marks_system<M: Measure>(
    mut measure: ResMut<M>,
    mut fields: Query<(&TextValue, &Cursor, &Sources<ChildOf>, &Node), (With<FieldMarks>, Or<(Changed<TextValue>, Changed<Cursor>)>)>,
    mut texts: Query<&mut Text>,
    mut marks: Query<(&mut Floating, &mut Node), (Without<FieldMarks>, MayHave<Closed>)>,
) {
    fields.each(|(value, cursor, children, node)| {
        let mut parts = children.sources();
        let (Some(selection), Some(text), Some(caret)) = (parts.next(), parts.next(), parts.next()) else { return };
        let Some(mut shown) = texts.get_mut(text) else { return };
        if shown.text != value.0 {
            shown.text = value.0.clone();
        }
        let config: TextConfig = shown.config;
        let left = node.0.padding.left as f32;
        let width_to = |measure: &mut M, chars: usize| measure.measure(&value.0[..byte_at(&value.0, chars)], &config).width;
        let (start, end) = cursor.selection();
        let (x_start, x_end, x_caret) = (width_to(&mut measure, start), width_to(&mut measure, end), width_to(&mut measure, cursor.position));
        if let Some((mut floating, _)) = marks.get_mut(caret) {
            let offset = Vec2::new(left + x_caret, 6.0);
            if floating.offset != offset {
                floating.offset = offset;
            }
        }
        if let Some((mut floating, mut node)) = marks.get_mut(selection) {
            let offset = Vec2::new(left + x_start, 5.0);
            if floating.offset != offset {
                floating.offset = offset;
            }
            let width = Sizing::fixed(x_end - x_start);
            if node.0.width != width {
                node.0.width = width;
            }
        }
    });
}

/// Buttons, checkboxes, sliders, dropdowns and text fields, measuring text
/// with `M`.
pub struct ControlsPlugin<M = Monospace>(PhantomData<fn() -> M>);
impl<M> Default for ControlsPlugin<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<M: Measure> Plugin for ControlsPlugin<M> {
    type Components = (
        Button,
        ButtonColors,
        Clicks,
        Checkbox,
        Checked,
        Slider,
        SliderValue,
        SliderRange,
        Dropdown,
        Selected,
        TextField,
        TextValue,
        Cursor,
        FieldMarks,
        Wakeup,
        Behaving,
        M,
        Inbox<PointerEnter>,
        Inbox<PointerLeave>,
        Inbox<Press>,
        Inbox<Release>,
        Inbox<Click>,
        Inbox<KeyPress>,
        Inbox<DragStart>,
        Inbox<Drag>,
        Inbox<DragEnd>,
        Inbox<TextInput>,
        Inbox<FocusIn>,
        Inbox<FocusOut>,
    );
    type Relations = (ChildOf,);
    type Storage = (Components<TextValue, true>, Components<Cursor, true>);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            behavior::<Button, _>(UiUpdate).systems::<S>(),
            behavior::<Checkbox, _>(UiUpdate).systems::<S>(),
            behavior::<Slider, _>(UiUpdate).systems::<S>(),
            behavior::<Dropdown, _>(UiUpdate).systems::<S>(),
            behavior::<TextField, _>(UiUpdate).systems::<S>(),
            schedule(UiUpdate, field_marks_system::<M>),
        )
    }
}

// ---------------------------------------------------------- Virtual list

/// A scroll view over `count` rows of `row_height`, showing a pool of rows
/// recycled as it scrolls: rows get a new [`RowIndex`] (fill them from it)
/// instead of being spawned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualList {
    pub count: usize,
    pub row_height: f32,
}
/// Which item a pooled row shows now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowIndex(pub usize);
/// A pooled row (and spacers, with `usize::MAX`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pooled(pub usize);
/// Spawn a virtual list `height` pixels tall under `parent`, with enough
/// pooled rows to fill it plus one; `row` builds each row's content (its
/// root gets `RowIndex`).
pub fn spawn_virtual_list<S: Storage, T: crate::SpawnTree<S>>(
    world: &mut World<S>,
    parent: Option<Entity>,
    list: VirtualList,
    width: f32,
    height: f32,
    mut row: impl FnMut() -> T,
) -> Entity {
    let view = el((
        Node::column().width(Sizing::fixed(width)).height(Sizing::fixed(height)),
        Clip { horizontal: false, vertical: true },
        Scroll::default(),
        list,
        Background(theme::FIELD),
    ));
    let view = match parent {
        Some(parent) => world.spawn_ui_under(parent, view),
        None => world.spawn_ui(view),
    };
    let spacer = |world: &mut World<S>| world.spawn_ui_under(view, el((Node::row().height(Sizing::fixed(0.0)), Pooled(usize::MAX))));
    spacer(world);
    let pool = (height / list.row_height).ceil() as usize + 1;
    for i in 0..pool {
        let entity = world.spawn_ui_under(view, row());
        world.insert_bundle(entity, (RowIndex(i), Pooled(i)));
    }
    spacer(world);
    view
}
/// Recycle a virtual list's rows when it scrolls (and when its count
/// changes): pure data rewrites, spacer heights, and spare rows closed.
#[allow(clippy::type_complexity)]
pub fn virtual_list_system<S: Storage>(
    mut lists: Query<(&VirtualList, &Scroll, &Sources<ChildOf>), Or<(Changed<Scroll>, Changed<VirtualList>)>>,
    mut nodes: Query<&mut Node, (With<Pooled>, MayHave<Closed>)>,
    mut rows: Query<(&mut RowIndex, Has<Closed>), MayHave<Closed>>,
    mut commands: Commands<S>,
) {
    lists.each(|(list, scroll, children)| {
        let count = children.len();
        if count < 2 {
            return;
        }
        let mut entries = children.sources();
        let (Some(top), Some(bottom)) = (entries.next(), entries.next_back()) else { return };
        let pool = count - 2;
        let first = ((scroll.offset.y / list.row_height).floor().max(0.0) as usize).min(list.count.saturating_sub(pool));
        let shown = pool.min(list.count.saturating_sub(first));
        let mut set_height = |entity: Entity, height: f32| {
            if let Some(mut node) = nodes.get_mut(entity) {
                let sizing = Sizing::fixed(height);
                if node.0.height != sizing {
                    node.0.height = sizing;
                }
            }
        };
        set_height(top, first as f32 * list.row_height);
        set_height(bottom, (list.count - first - shown) as f32 * list.row_height);
        for (i, row) in entries.enumerate() {
            let Some((mut index, closed)) = rows.get_mut(row) else { continue };
            if index.0 != first + i {
                index.0 = first + i;
            }
            // Rows past the end wait in the pool, closed.
            match (i < shown, closed) {
                (true, true) => commands.enable_subtree::<Closed, ChildOf>(row),
                (false, false) => commands.disable_subtree::<Closed, ChildOf>(row),
                _ => {}
            }
        }
    });
}

// ----------------------------------------------------------------- Tabs

/// A tab strip's behavior marker (on its root: a bar and the panels).
#[derive(Clone, Copy, Debug, Default)]
pub struct Tabs;
/// The open tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActiveTab(pub usize);
/// Spawn tabs: one button per title, one panel per subtree; the first is
/// open, the others closed.
pub fn spawn_tabs<S: Storage>(world: &mut World<S>, parent: Option<Entity>, titles: &[&str], mut panel: impl FnMut(&mut World<S>, usize, Entity)) -> Entity {
    let root = el((Node::column().gap(0), Tabs, ActiveTab(0)));
    let root = match parent {
        Some(parent) => world.spawn_ui_under(parent, root),
        None => world.spawn_ui(root),
    };
    let bar = world.spawn_ui_under(root, el((Node::row().gap(2),)));
    let panels = world.spawn_ui_under(root, el((Node::column().padding(Padding::all(8)), Background(theme::PANEL))));
    for (i, title) in titles.iter().enumerate() {
        let tab = world.spawn_ui_under(bar, button(title));
        if i == 0 {
            world.insert(tab, Background(theme::ACCENT));
        }
        let page = world.spawn_ui_under(panels, el((Node::column(),)));
        panel(world, i, page);
        if i > 0 {
            world.disable_subtree::<Closed, ChildOf>(page);
        }
    }
    root
}
impl Behavior for Tabs {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let (Some(bar), Some(panels)) = (child(cx, me, 0), child(cx, me, 1)) else { return };
        let tabs = children(cx, bar);
        let pages = children(cx, panels);
        loop {
            let (index, _) = select_all(tabs.iter().map(|&t| cx.on::<Click>(t))).await;
            for (i, &page) in pages.iter().enumerate() {
                if i == index {
                    behave::open(cx, page);
                } else {
                    behave::close(cx, page);
                }
            }
            let tabs = tabs.clone();
            let _ = cx.with(move |mut active: Query<&mut ActiveTab>, mut colors: Query<(&mut ButtonColors, &mut Background)>| {
                if let Some(mut active) = active.get_mut(me) {
                    active.0 = index;
                }
                for (i, &tab) in tabs.iter().enumerate() {
                    if let Some((mut colors, mut background)) = colors.get_mut(tab) {
                        let normal = if i == index { theme::ACCENT } else { theme::BUTTON };
                        if colors.normal != normal {
                            colors.normal = normal;
                            background.0 = normal;
                        }
                    }
                }
            });
        }
    }
}

// --------------------------------------------------------------- Tooltip

/// Shows `0` near the element after the pointer rests on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tooltip(pub String);
/// How long the pointer rests before a tooltip shows.
pub const TOOLTIP_DELAY: Duration = Duration::from_millis(400);
/// Marks a tooltip's floating box.
#[derive(Clone, Copy, Debug, Default)]
pub struct TooltipBox;
impl Behavior for Tooltip {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<PointerEnter>(me).await;
            if let behave::Either::Right(_) = select(sleep(cx, TOOLTIP_DELAY), cx.on::<PointerLeave>(me)).await {
                continue;
            }
            let text = cx.with(|mut q: Query<&Tooltip>| q.get_mut(me).map(|t| t.0.clone())).ok().flatten().unwrap_or_default();
            let scope = Scope::new(cx);
            scope.spawn(
                Some(me),
                el((
                    Node::row().padding(Padding::xy(8, 4)),
                    Background(theme::TEXT),
                    Radius::all(4.0),
                    TooltipBox,
                    Floating { attach: Attach::Parent, element: AttachPoint::CenterTop, parent: AttachPoint::CenterBottom, offset: Vec2::new(0.0, 6.0), z: 50, pointer: crate::PointerCapture::Passthrough, ..Floating::default() },
                ))
                .children((el((Text::new(text, 14).color(Color::WHITE),)),)),
            );
            select(cx.on::<PointerLeave>(me), cx.on::<Press>(me)).await;
            drop(scope);
        }
    }
}

// ----------------------------------------------------------------- Modal

/// A modal dialog's behavior marker (on its backdrop). Open it with
/// `open`; Escape or a click on the backdrop closes it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Modal;
/// Spawn a closed modal: a dimmed full-window backdrop centering a dialog
/// panel; `content` fills the dialog.
pub fn spawn_modal<S: Storage>(world: &mut World<S>, content: impl FnOnce(&mut World<S>, Entity)) -> Entity {
    let backdrop = world.spawn_ui(el((
        Node::row().width(Sizing::GROW).height(Sizing::GROW).align(AlignX::Center, AlignY::Center),
        Overlay(theme::BACKDROP),
        Floating { attach: Attach::Root, z: 100, ..Floating::default() },
        Modal,
        Interaction::default(),
        Focusable,
    )));
    let dialog = world.spawn_ui_under(
        backdrop,
        el((Node::column().padding(Padding::all(16)).gap(8), Background(theme::PANEL), Radius::all(8.0), Border { color: theme::BORDER, width: BorderWidth::all(1) })),
    );
    content(world, dialog);
    world.disable_subtree::<Closed, ChildOf>(backdrop);
    backdrop
}
impl Behavior for Modal {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            match select(cx.on::<Click>(me), cx.on::<KeyPress>(me)).await {
                behave::Either::Left(click) => {
                    // Clicks inside the dialog bubble here too: only the
                    // backdrop around it closes.
                    let inside = child(cx, me, 0)
                        .and_then(|dialog| cx.with(move |mut q: Query<&Rect>| q.get_mut(dialog).map(|r| r.0.contains(click.position))).ok().flatten())
                        .unwrap_or(false);
                    if inside {
                        continue;
                    }
                }
                behave::Either::Right(KeyPress { key: Key::Escape, .. }) => {}
                behave::Either::Right(_) => continue,
            }
            behave::close(cx, me);
        }
    }
}

// ------------------------------------------------------------- Inspector

/// A text entity listing the UI tree, closed panels included.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inspector;
/// Refresh inspectors when the tree was flattened again (anything opened,
/// closed, spawned or despawned).
#[allow(clippy::type_complexity)]
pub fn inspector_system(
    ui: Res<UiLayout>,
    mut last: Local<u64>,
    mut inspectors: Query<&mut Text, With<Inspector>>,
    mut nodes: Query<(Entity, Has<Closed>, Option<&Relates<ChildOf>>, Option<&Text>), (Or<(With<Node>, With<Text>)>, MayHave<Closed>, Without<Inspector>)>,
) {
    if ui.rebuilds() == *last {
        return;
    }
    *last = ui.rebuilds();
    let mut entries: Vec<(Entity, bool, Option<Entity>, Option<String>)> = Vec::new();
    nodes.each(|(entity, closed, parent, text)| entries.push((entity, closed, parent.map(|p| p.target()), text.map(|t| t.text.clone()))));
    entries.sort_by_key(|e| e.0);
    let mut listing = format!("{} elements\n", entries.len());
    fn walk(entries: &[(Entity, bool, Option<Entity>, Option<String>)], parent: Option<Entity>, depth: usize, out: &mut String) {
        for (entity, closed, _, text) in entries.iter().filter(|e| e.2 == parent) {
            out.push_str(&"  ".repeat(depth));
            out.push_str(&format!("{}v{}", entity.index(), entity.generation()));
            if let Some(text) = text {
                out.push_str(&format!(" {text:?}"));
            }
            if *closed {
                out.push_str(" [closed]");
            }
            out.push('\n');
            walk(entries, Some(*entity), depth + 1, out);
        }
    }
    let roots: Vec<_> = entries.iter().filter(|e| e.2.is_none_or(|p| !entries.iter().any(|x| x.0 == p))).map(|e| e.0).collect();
    for root in roots {
        let entry = entries.iter().find(|e| e.0 == root).unwrap().clone();
        listing.push_str(&format!("{}v{}{}\n", root.index(), root.generation(), if entry.1 { " [closed]" } else { "" }));
        walk(&entries, Some(root), 1, &mut listing);
    }
    inspectors.each(|mut text| {
        if text.text != listing {
            text.text = listing.clone();
        }
    });
}

/// Virtual lists, tabs, tooltips, modals and the inspector.
pub struct ContainersPlugin;
impl Plugin for ContainersPlugin {
    type Components = (
        VirtualList,
        RowIndex,
        Pooled,
        Tabs,
        ActiveTab,
        Tooltip,
        TooltipBox,
        Modal,
        Inspector,
        ButtonColors,
        Wakeup,
        Behaving,
        Inbox<Click>,
        Inbox<PointerEnter>,
        Inbox<PointerLeave>,
        Inbox<Press>,
        Inbox<KeyPress>,
    );
    type Relations = (ChildOf,);
    type Storage = (Components<VirtualList, true>, Components<RowIndex, true>);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            behavior::<Tabs, _>(UiUpdate).systems::<S>(),
            behavior::<Tooltip, _>(UiUpdate).systems::<S>(),
            behavior::<Modal, _>(UiUpdate).systems::<S>(),
            schedule(UiUpdate, virtual_list_system::<S>),
            schedule(UiFrame, inspector_system),
        )
    }
}

/// Every widget plugin, measuring with `M`.
pub fn widgets<M: Measure>() -> (ControlsPlugin<M>, ContainersPlugin) {
    (ControlsPlugin::default(), ContainersPlugin)
}
