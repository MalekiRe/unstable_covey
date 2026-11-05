//! static-ui: a retained, ECS-native UI on static-ecs.
//!
//! - Every element is an entity: a container holds [`Node`], a text leaf
//!   [`Text`]; children relate to their parent with [`ChildOf`] (ordered,
//!   linked, acyclic).
//! - Layout is Clay's ([`static_ui_layout`]), run over a flattened copy of
//!   the visible tree that is rebuilt only when the structure changes;
//!   changed nodes and text are patched in place, and an unchanged UI runs
//!   no layout at all. Results land in each entity's [`Rect`], written only
//!   when it moved.
//! - Closing a panel inserts [`Closed`], a `Disabling` component, down its
//!   subtree ([`close`] / [`open`]): every query of the UI pipeline skips it
//!   without mentioning it, while its components (and behaviors) stay put.
//!
//! See `UI_PLAN.md` at the workspace root for the whole design.
use std::marker::PhantomData;

use static_ecs::prelude::*;
pub use static_ui_layout as layout;
pub use static_ui_layout::{
    AlignX, AlignY, AttachPoint, Direction, Layout, Padding, PointerCapture, Size, Sizing, SizingKind, TextAlign,
    TextConfig, Vec2, Wrap,
};

mod flatten;
pub mod input;
mod interact;
mod render;
mod spawn;

pub use flatten::{UiLayout, layout_system};
pub use input::{Input, Key, Modifiers, MouseButton, Redraw};
pub use interact::{
    Click, Drag, DragEnd, DragStart, Focus, FocusIn, FocusOut, Focusable, InputPlugin, Interaction, KeyPress, Pointer,
    PointerEnter, PointerLeave, Press, Release, TextInput, Wheel, bubble, hit_test, DRAG_THRESHOLD,
};
pub use render::{ClearColor, Command, Draw, RenderList, render_system};
pub use spawn::{El, SpawnList, SpawnTree, SpawnUi, el};

/// The UI's frame: input, behaviors, layout and rendering run under it.
pub struct UiFrame;
impl Schedule for UiFrame {}

/// The parent link of the UI tree: ordered (children keep declaration
/// order), linked (despawning a node despawns its subtree), acyclic.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
    const LINKED: bool = true;
    const ACYCLIC: bool = true;
    const ORDERED: bool = true;
}

/// A container's layout: Clay's layout config. Build them as constants:
/// `const PANEL: Node = Node::column().padding(Padding::all(16)).gap(8);`
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Node(pub Layout);
impl Node {
    pub const fn row() -> Self {
        Self(Layout::row())
    }
    pub const fn column() -> Self {
        Self(Layout::column())
    }
    pub const fn width(self, width: Sizing) -> Self {
        Self(self.0.width(width))
    }
    pub const fn height(self, height: Sizing) -> Self {
        Self(self.0.height(height))
    }
    pub const fn padding(self, padding: Padding) -> Self {
        Self(self.0.padding(padding))
    }
    pub const fn gap(self, gap: u16) -> Self {
        Self(self.0.gap(gap))
    }
    pub const fn align(self, x: AlignX, y: AlignY) -> Self {
        Self(self.0.align(x, y))
    }
}
impl Requires for Node {
    type Required = (Rect,);
}

/// An RGBA color, 0–255 per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}
impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
}

/// A text leaf: its string, Clay text config, and color.
#[derive(Clone, Debug, PartialEq)]
pub struct Text {
    pub text: String,
    pub config: TextConfig,
    pub color: Color,
}
impl Text {
    pub fn new(text: impl Into<String>, size: u16) -> Self {
        Self { text: text.into(), config: TextConfig { size, ..TextConfig::default() }, color: Color::BLACK }
    }
    pub fn color(self, color: Color) -> Self {
        Self { color, ..self }
    }
    pub fn config(self, config: TextConfig) -> Self {
        Self { config, ..self }
    }
}
impl Requires for Text {
    type Required = (Rect,);
}

/// Layout output: the element's box, written only when it changes, so
/// `Changed<Rect>` marks what moved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect(pub layout::Rect);

/// What a floating element is positioned against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Attach {
    /// Its parent.
    #[default]
    Parent,
    /// The window: absolute positioning.
    Root,
    /// Another element.
    To(Entity),
}
/// A floating element (tooltip, dropdown, modal): laid out above its
/// parent's flow, attached to a point of `attach`, painted by `z`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Floating {
    pub attach: Attach,
    pub offset: Vec2,
    pub expand: Size,
    pub z: i16,
    /// The point on this element ...
    pub element: AttachPoint,
    /// ... placed on this point of `attach`.
    pub parent: AttachPoint,
    pub pointer: PointerCapture,
    pub clip_to_parent: bool,
}
/// Clips children to the box on the chosen axes: a scroll container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clip {
    pub horizontal: bool,
    pub vertical: bool,
}
/// A scroll container's position: children move by `-offset`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Scroll {
    pub offset: Vec2,
}
/// Width over height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AspectRatio(pub f32);

/// Fill color.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Background(pub Color);
/// Border widths per side and between children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BorderWidth {
    pub left: u16,
    pub right: u16,
    pub top: u16,
    pub bottom: u16,
    pub between: u16,
}
impl BorderWidth {
    pub const fn all(width: u16) -> Self {
        Self { left: width, right: width, top: width, bottom: width, between: 0 }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Border {
    pub color: Color,
    pub width: BorderWidth,
}
/// Corner radii.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_left: f32,
    pub bottom_right: f32,
}
impl Radius {
    pub const fn all(radius: f32) -> Self {
        Self { top_left: radius, top_right: radius, bottom_left: radius, bottom_right: radius }
    }
}
/// A color blended over the element and its children.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Overlay(pub Color);

/// A panel the user closed: invisible, inert, its behaviors frozen. Put it
/// on a subtree with [`close`]; [`open`] takes it off.
#[derive(Clone, Copy, Debug, Default)]
pub struct Closed;
impl Disabling for Closed {}
/// A subtree kept only to be instantiated (list rows, dialog prototypes).
/// Not `Clone`: `world.clone_subtree::<ChildOf>(prototype)` yields a
/// visible copy.
#[derive(Debug, Default)]
pub struct Template;
impl Disabling for Template {}
/// Visible, but ignores input.
#[derive(Clone, Copy, Debug, Default)]
pub struct Inert;

/// The window (or surface) size the root fills, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}
impl Default for Viewport {
    fn default() -> Self {
        Self { width: 800.0, height: 600.0 }
    }
}

/// Text measurement for layout: a string's size in a text config.
pub trait Measure: Default + Send + Sync + 'static {
    fn measure(&mut self, text: &str, config: &TextConfig) -> Size;
}
/// Half the font size per character, 1.25× it per line: deterministic, for
/// tests and headless use.
#[derive(Clone, Copy, Debug, Default)]
pub struct Monospace;
impl Measure for Monospace {
    fn measure(&mut self, text: &str, config: &TextConfig) -> Size {
        let chars = text.chars().count() as f32;
        Size::new(chars * (config.size as f32 * 0.5 + config.letter_spacing as f32), config.size as f32 * 1.25)
    }
}

/// Close `root`'s subtree: `Closed` on it and every descendant.
pub fn close<S: Storage>(commands: &mut Commands<S>, root: Entity) {
    commands.disable_subtree::<Closed, ChildOf>(root);
}
/// Reopen a subtree closed with [`close`]; panels closed inside it on their
/// own stay closed.
pub fn open<S: Storage>(commands: &mut Commands<S>, root: Entity) {
    commands.enable_subtree::<Closed, ChildOf>(root);
}

/// The UI core: components, the `ChildOf` relation, and layout under
/// [`UiFrame`], measuring text with `M`.
pub struct UiPlugin<M = Monospace>(PhantomData<fn() -> M>);
impl<M> Default for UiPlugin<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<M: Measure> Plugin for UiPlugin<M> {
    type Components = (
        Node,
        Text,
        Rect,
        Floating,
        Clip,
        Scroll,
        AspectRatio,
        Background,
        Border,
        Radius,
        Overlay,
        Closed,
        Template,
        Disabled,
        DisabledRoot<Closed>,
        Inert,
        Viewport,
        UiLayout,
        RenderList,
        ClearColor,
        Redraw,
        static_ecs::Wakeup,
        M,
    );
    type Relations = (ChildOf,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        flatten::systems::<S, M>()
    }
}
