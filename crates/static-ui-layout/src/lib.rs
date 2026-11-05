//! Clay's layout algorithm (<https://github.com/nicbarker/clay>), ported to
//! Rust over flat arrays: no ECS, no dependencies, and no allocation once
//! the arrays are warm.
//!
//! A [`Tree`] is declared like a Clay frame, `open` / `text` / `close` in
//! depth-first order, and then laid out with [`Tree::layout`]. Unlike Clay,
//! declaring and laying out are separate: a declared tree can have its
//! configs patched in place ([`Tree::config_mut`], [`Tree::set_text`]) and be
//! laid out again without being declared again.
//!
//! Every step follows `Clay__CloseElement`, `Clay__SizeContainersAlongAxis`
//! and `Clay__CalculateFinalLayout`, including their floating-point order of
//! operations, so boxes match clay.h exactly (see the `static-ui-clay-oracle`
//! crate's differential test). One deliberate deviation: where Clay's size
//! distribution makes no progress and loops forever, this port stops (see
//! [`Tree::stalls`]). Two Clay behaviors carry over on purpose:
//! a percent sizing's maximum reads as 0 where Clay reads its union, and a
//! floating element attached to an element laid out later in the frame (a
//! higher z) is placed against that element's previous box.
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Clay's `CLAY__EPSILON`.
const EPSILON: f32 = 0.01;
/// Clay's `CLAY__MAXFLOAT`.
pub const MAX: f32 = f32::MAX;
/// Not an element.
const NONE: u32 = u32::MAX;

#[inline(always)]
fn max(x: f32, y: f32) -> f32 {
    if x > y { x } else { y }
}
#[inline(always)]
fn min(x: f32, y: f32) -> f32 {
    if x < y { x } else { y }
}
#[inline(always)]
fn float_equal(left: f32, right: f32) -> bool {
    let subtracted = left - right;
    subtracted < EPSILON && subtracted > -EPSILON
}

/// Width and height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}
impl Size {
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
    #[inline(always)]
    fn axis(&mut self, x: bool) -> &mut f32 {
        if x { &mut self.width } else { &mut self.height }
    }
    #[inline(always)]
    fn get(self, x: bool) -> f32 {
        if x { self.width } else { self.height }
    }
}
/// A point or offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}
impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}
/// A laid-out box. `repr(C)`: the same as Clay's `Clay_BoundingBox`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }
    /// Whether `point` lies inside, edges included (Clay's
    /// `Clay__PointIsInsideRect`).
    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.x && point.x <= self.x + self.width && point.y >= self.y && point.y <= self.y + self.height
    }
    /// The overlap of two boxes; empty (zero-sized) if they do not overlap.
    pub fn intersect(self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        Rect::new(x, y, (right - x).max(0.0), (bottom - y).max(0.0))
    }
    /// The smallest box holding both; an empty box contributes nothing.
    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        Rect::new(x, y, right - x, bottom - y)
    }
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// How an element takes up space along one axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizingKind {
    /// Wraps its content, clamped to `min..=max`.
    Fit,
    /// Fills the space its parent has left, shared with other `Grow`
    /// siblings, clamped to `min..=max`.
    Grow,
    /// A share (0–1) of the parent's content size, minus padding and gaps.
    Percent,
    /// Exactly `min` (= `max`) pixels.
    Fixed,
}
/// Sizing along one axis. A `max` of 0 or less means unbounded. As in
/// Clay's union, `Percent` keeps its share in `min` and reads `max` as 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sizing {
    pub kind: SizingKind,
    pub min: f32,
    pub max: f32,
}
impl Sizing {
    pub const FIT: Sizing = Sizing::fit(0.0, 0.0);
    pub const GROW: Sizing = Sizing::grow(0.0, 0.0);
    pub const fn fit(min: f32, max: f32) -> Self {
        Self { kind: SizingKind::Fit, min, max }
    }
    pub const fn grow(min: f32, max: f32) -> Self {
        Self { kind: SizingKind::Grow, min, max }
    }
    pub const fn fixed(size: f32) -> Self {
        Self { kind: SizingKind::Fixed, min: size, max: size }
    }
    pub const fn percent(share: f32) -> Self {
        Self { kind: SizingKind::Percent, min: share, max: 0.0 }
    }
    /// The `Percent` share.
    pub const fn share(self) -> f32 {
        self.min
    }
}
impl Default for Sizing {
    fn default() -> Self {
        Self::FIT
    }
}
/// Space between an element's edges and its children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Padding {
    pub left: u16,
    pub right: u16,
    pub top: u16,
    pub bottom: u16,
}
impl Padding {
    pub const fn all(padding: u16) -> Self {
        Self { left: padding, right: padding, top: padding, bottom: padding }
    }
    pub const fn xy(x: u16, y: u16) -> Self {
        Self { left: x, right: x, top: y, bottom: y }
    }
}
/// The axis children are laid out along.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    LeftToRight,
    TopToBottom,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignX {
    #[default]
    Left,
    Right,
    Center,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlignY {
    #[default]
    Top,
    Bottom,
    Center,
}
/// Clay's `Clay_LayoutConfig`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Layout {
    pub width: Sizing,
    pub height: Sizing,
    pub padding: Padding,
    /// Space between children along the layout axis.
    pub gap: u16,
    pub align_x: AlignX,
    pub align_y: AlignY,
    pub direction: Direction,
}
impl Layout {
    /// A left-to-right row with `Fit` sizing.
    pub const fn row() -> Self {
        Self {
            width: Sizing::FIT,
            height: Sizing::FIT,
            padding: Padding::all(0),
            gap: 0,
            align_x: AlignX::Left,
            align_y: AlignY::Top,
            direction: Direction::LeftToRight,
        }
    }
    /// A top-to-bottom column with `Fit` sizing.
    pub const fn column() -> Self {
        Self { direction: Direction::TopToBottom, ..Self::row() }
    }
    pub const fn width(self, width: Sizing) -> Self {
        Self { width, ..self }
    }
    pub const fn height(self, height: Sizing) -> Self {
        Self { height, ..self }
    }
    pub const fn padding(self, padding: Padding) -> Self {
        Self { padding, ..self }
    }
    pub const fn gap(self, gap: u16) -> Self {
        Self { gap, ..self }
    }
    pub const fn align(self, align_x: AlignX, align_y: AlignY) -> Self {
        Self { align_x, align_y, ..self }
    }
    fn padding_along(&self, x: bool) -> f32 {
        if x {
            (self.padding.left + self.padding.right) as f32
        } else {
            (self.padding.top + self.padding.bottom) as f32
        }
    }
}
/// Where on a box a floating element attaches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AttachPoint {
    #[default]
    LeftTop,
    LeftCenter,
    LeftBottom,
    CenterTop,
    CenterCenter,
    CenterBottom,
    RightTop,
    RightCenter,
    RightBottom,
}
impl AttachPoint {
    /// Horizontal position as a fraction: 0, 1/2 or 1.
    fn x(self) -> u8 {
        match self {
            Self::LeftTop | Self::LeftCenter | Self::LeftBottom => 0,
            Self::CenterTop | Self::CenterCenter | Self::CenterBottom => 1,
            Self::RightTop | Self::RightCenter | Self::RightBottom => 2,
        }
    }
    fn y(self) -> u8 {
        match self {
            Self::LeftTop | Self::CenterTop | Self::RightTop => 0,
            Self::LeftCenter | Self::CenterCenter | Self::RightCenter => 1,
            Self::LeftBottom | Self::CenterBottom | Self::RightBottom => 2,
        }
    }
}
/// What a floating element is positioned against.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AttachTo {
    /// Not floating.
    #[default]
    None,
    /// Its declaring parent.
    Parent,
    /// An element declared earlier, by index.
    Element(u32),
    /// The root: absolute positioning.
    Root,
}
/// Whether pointer events stop at a floating element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerCapture {
    #[default]
    Capture,
    Passthrough,
}
/// Clay's `Clay_FloatingElementConfig`: tooltips, dropdowns, modals. A
/// floating element is laid out as its own tree, above lower `z`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Floating {
    pub attach_to: AttachTo,
    pub offset: Vec2,
    /// Grows the box on every side without moving children.
    pub expand: Size,
    pub z: i16,
    /// The point on this element ...
    pub element: AttachPoint,
    /// ... placed on this point of the attached element.
    pub parent: AttachPoint,
    pub pointer: PointerCapture,
    /// Clip to the attached element's clip rectangle.
    pub clip_to_parent: bool,
}
/// Clay's `Clay_ClipElementConfig`: scroll containers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clip {
    pub horizontal: bool,
    pub vertical: bool,
    /// Offsets every child: the scroll position, as a negative offset.
    pub child_offset: Vec2,
}
/// Everything that shapes a container.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Config {
    pub layout: Layout,
    pub floating: Floating,
    pub clip: Clip,
    /// Width over height; 0 for none.
    pub aspect_ratio: f32,
}
impl From<Layout> for Config {
    fn from(layout: Layout) -> Self {
        Self { layout, ..Self::default() }
    }
}
/// How text breaks into lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Wrap {
    #[default]
    Words,
    Newlines,
    None,
}
/// How wrapped lines sit inside the text's box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}
/// Clay's `Clay_TextElementConfig`, minus the color (a rendering concern).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextConfig {
    pub font: u16,
    pub size: u16,
    pub letter_spacing: u16,
    /// 0: the font's natural line height.
    pub line_height: u16,
    pub wrap: Wrap,
    pub align: TextAlign,
}
/// One wrapped line of a text element: a byte range of its text.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Line {
    pub start: u32,
    pub len: u32,
    pub width: f32,
    pub height: f32,
}

/// One measured word: Clay's `Clay__MeasuredWord`. A length of 0 is a
/// newline.
#[derive(Clone, Copy, Debug)]
struct Word {
    start: u32,
    len: u32,
    width: f32,
}
/// A text's measurement: Clay's `Clay__MeasureTextCacheItem`.
#[derive(Debug, Default)]
struct Measured {
    unwrapped: Size,
    min_width: f32,
    contains_newlines: bool,
    words: Vec<Word>,
    generation: u32,
}

/// FNV-1a: the cache's keys are already hashes.
#[derive(Default)]
struct Fnv(u64);
impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ byte as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

/// Measured words by (text, config), like Clay's `measureTextCache`. Kept
/// across layouts; entries unused for a few layouts are dropped.
#[derive(Default)]
pub struct TextCache {
    entries: HashMap<u64, Measured, BuildHasherDefault<Fnv>>,
    generation: u32,
}
impl TextCache {
    fn key(text: &str, config: &TextConfig) -> u64 {
        let mut hash = Fnv(0xcbf2_9ce4_8422_2325);
        hash.write(text.as_bytes());
        hash.write(&[0xff]);
        hash.write(&config.font.to_le_bytes());
        hash.write(&config.size.to_le_bytes());
        hash.write(&config.letter_spacing.to_le_bytes());
        hash.0
    }
    /// `Clay__MeasureTextCached`.
    fn measure(&mut self, text: &str, config: &TextConfig, measure: &mut impl FnMut(&str, &TextConfig) -> Size) -> u64 {
        let key = Self::key(text, config);
        let generation = self.generation;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.generation = generation;
            return key;
        }
        let mut measured = Measured { generation, ..Measured::default() };
        let bytes = text.as_bytes();
        let (mut start, mut end) = (0usize, 0usize);
        let (mut line_width, mut measured_width, mut measured_height) = (0f32, 0f32, 0f32);
        let space_width = measure(" ", config).width;
        while end < bytes.len() {
            let current = bytes[end];
            if current == b' ' || current == b'\n' {
                let length = end - start;
                let mut dimensions = Size::default();
                if length > 0 {
                    dimensions = measure(&text[start..end], config);
                }
                measured.min_width = max(dimensions.width, measured.min_width);
                measured_height = max(measured_height, dimensions.height);
                if current == b' ' {
                    dimensions.width += space_width;
                    measured.words.push(Word { start: start as u32, len: length as u32 + 1, width: dimensions.width });
                    line_width += dimensions.width;
                }
                if current == b'\n' {
                    if length > 0 {
                        measured.words.push(Word { start: start as u32, len: length as u32, width: dimensions.width });
                    }
                    measured.words.push(Word { start: end as u32 + 1, len: 0, width: 0.0 });
                    line_width += dimensions.width;
                    measured_width = max(line_width, measured_width);
                    measured.contains_newlines = true;
                    line_width = 0.0;
                }
                start = end + 1;
            }
            end += 1;
        }
        if end - start > 0 {
            let dimensions = measure(&text[start..end], config);
            measured.words.push(Word { start: start as u32, len: (end - start) as u32, width: dimensions.width });
            line_width += dimensions.width;
            measured_height = max(measured_height, dimensions.height);
            measured.min_width = max(dimensions.width, measured.min_width);
        }
        measured_width = max(line_width, measured_width) - config.letter_spacing as f32;
        measured.unwrapped = Size::new(measured_width, measured_height);
        self.entries.insert(key, measured);
        key
    }
    fn get(&self, key: u64) -> &Measured {
        &self.entries[&key]
    }
    /// Start a layout: drop entries unused for more than two layouts, every
    /// so often.
    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation % 64 == 0 {
            let generation = self.generation;
            self.entries.retain(|_, entry| generation.wrapping_sub(entry.generation) <= 2);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Element {
    config: Config,
    /// This layout's sizing: `config`'s, with Clay's in-place edits (an
    /// unbounded `max`, an aspect ratio's height cap).
    sizing: [Sizing; 2],
    is_text: bool,
    text: TextConfig,
    /// Byte range in the tree's text arena.
    text_start: u32,
    text_len: u32,
    measured: u64,
    preferred: Size,
    /// Range of `Tree::lines`.
    lines_start: u32,
    lines_len: u32,
    children_start: u32,
    children_len: u32,
    dimensions: Size,
    min_dimensions: Size,
    /// For floating elements: the element attached to.
    attached: u32,
}
impl Element {
    fn new(config: Config) -> Self {
        Self {
            config,
            sizing: [config.layout.width, config.layout.height],
            is_text: false,
            text: TextConfig::default(),
            text_start: 0,
            text_len: 0,
            measured: 0,
            preferred: Size::default(),
            lines_start: 0,
            lines_len: 0,
            children_start: 0,
            children_len: 0,
            dimensions: Size::default(),
            min_dimensions: Size::default(),
            attached: NONE,
        }
    }
    /// `Clay__GetElementSizing`: text sizes as an unbounded `Fit`... with
    /// Clay's zeroed min and max.
    #[inline(always)]
    fn sizing(&self, x: bool) -> Sizing {
        if self.is_text {
            Sizing::fit(0.0, 0.0)
        } else if x {
            self.sizing[0]
        } else {
            self.sizing[1]
        }
    }
    fn is_floating(&self) -> bool {
        !self.is_text && self.config.floating.attach_to != AttachTo::None
    }
    fn clips(&self, x: bool) -> bool {
        if x { self.config.clip.horizontal } else { self.config.clip.vertical }
    }
}

/// A root of the layout: the main tree (element 0) or a floating element.
#[derive(Clone, Copy, Debug)]
struct Root {
    element: u32,
    z: i16,
}

/// A node of the positioning walk.
#[derive(Clone, Copy)]
struct Walk {
    element: u32,
    position: Vec2,
    next_child: Vec2,
    visited: bool,
}

/// A declared UI tree and its last layout.
#[derive(Default)]
pub struct Tree {
    elements: Vec<Element>,
    children: Vec<u32>,
    /// Roots in declaration order (sizing walks them so).
    roots: Vec<Root>,
    /// Roots in paint order: stably sorted by `z` each layout.
    paint: Vec<Root>,
    text: String,
    lines: Vec<Line>,
    /// Each element's box, kept between layouts (floating elements attached
    /// to later elements read the previous one, as in Clay).
    boxes: Vec<Rect>,
    /// Each clipping element's content size, for scroll limits.
    content: Vec<Size>,
    // Declaration state.
    open: Vec<u32>,
    pending: Vec<u32>,
    // Layout scratch.
    bfs: Vec<u32>,
    resizable: Vec<u32>,
    text_elements: Vec<u32>,
    aspect: Vec<u32>,
    walk: Vec<Walk>,
    stalls: u64,
    /// Bytes of `text` no element refers to any more.
    dead_text: usize,
    /// The previous arena, reused by compaction.
    spare_text: String,
}

impl Tree {
    pub fn new() -> Self {
        Self::default()
    }
    /// Start declaring a tree inside a `width` × `height` root (element 0),
    /// keeping every buffer's capacity.
    pub fn begin(&mut self, width: f32, height: f32) {
        self.elements.clear();
        self.children.clear();
        self.roots.clear();
        self.text.clear();
        self.open.clear();
        self.pending.clear();
        let layout = Layout::row().width(Sizing::fixed(width)).height(Sizing::fixed(height));
        self.elements.push(Element::new(layout.into()));
        self.open.push(0);
        self.roots.push(Root { element: 0, z: 0 });
    }
    /// Open a container as the last open element's child; returns its index.
    pub fn open(&mut self, config: impl Into<Config>) -> u32 {
        let config = config.into();
        let index = self.elements.len() as u32;
        let mut element = Element::new(config);
        let parent = *self.open.last().expect("open inside `begin`");
        if config.floating.attach_to != AttachTo::None {
            element.attached = match config.floating.attach_to {
                AttachTo::Parent => parent,
                AttachTo::Root => 0,
                // Checked in `end`: unlike Clay, a later element may be the
                // target (Clay reports an error for those).
                AttachTo::Element(target) => target,
                AttachTo::None => NONE,
            };
            self.roots.push(Root { element: index, z: config.floating.z });
        }
        self.elements.push(element);
        self.open.push(index);
        index
    }
    /// Close the last open container.
    pub fn close(&mut self) {
        let index = self.open.pop().expect("close without open");
        assert!(index != 0, "close the root with `end`");
        self.close_element(index);
        if !self.elements[index as usize].is_floating() {
            self.pending.push(index);
        }
    }
    fn close_element(&mut self, index: u32) {
        // This element's children are the last `n` pending entries, where
        // `n` was counted as they closed: collect them in order.
        let start = self.pending_start(index);
        let children_start = self.children.len() as u32;
        self.children.extend_from_slice(&self.pending[start..]);
        let children_len = (self.pending.len() - start) as u32;
        self.pending.truncate(start);
        let element = &mut self.elements[index as usize];
        element.children_start = children_start;
        element.children_len = children_len;
    }
    /// Where `index`'s children begin in `pending`: every pending entry
    /// after them is a later sibling's descendant, already collected, so
    /// its children are exactly the entries declared after it.
    fn pending_start(&self, index: u32) -> usize {
        self.pending.partition_point(|&child| child < index)
    }
    /// Add a text element as the last open container's child.
    pub fn text(&mut self, text: &str, config: TextConfig) -> u32 {
        let index = self.elements.len() as u32;
        let mut element = Element::new(Config::default());
        element.is_text = true;
        element.text = config;
        element.text_start = self.text.len() as u32;
        element.text_len = text.len() as u32;
        self.text.push_str(text);
        self.elements.push(element);
        self.pending.push(index);
        index
    }
    /// Finish declaring: close the root.
    pub fn end(&mut self) {
        assert_eq!(self.open.as_slice(), [0], "unbalanced open/close");
        self.open.clear();
        self.close_element(0);
        debug_assert!(self.pending.is_empty());
        let n = self.elements.len();
        for element in &mut self.elements {
            if element.attached != NONE && element.attached as usize >= n {
                element.attached = NONE;
            }
        }
        self.dead_text = 0;
        self.boxes.resize(n, Rect::default());
        self.content.resize(n, Size::default());
    }
    /// The number of declared elements, root included.
    pub fn len(&self) -> usize {
        self.elements.len()
    }
    pub fn is_empty(&self) -> bool {
        self.elements.len() <= 1
    }
    /// A container's config, to change in place before the next layout.
    pub fn config_mut(&mut self, element: u32) -> &mut Config {
        &mut self.elements[element as usize].config
    }
    pub fn config(&self, element: u32) -> &Config {
        &self.elements[element as usize].config
    }
    /// Resize the root (nothing to resize before the first `begin`).
    pub fn set_root_size(&mut self, width: f32, height: f32) {
        let Some(root) = self.elements.first_mut() else { return };
        let layout = &mut root.config.layout;
        layout.width = Sizing::fixed(width);
        layout.height = Sizing::fixed(height);
    }
    /// Replace a text element's text and config. Text that fits is
    /// overwritten in place; the arena is compacted once half of it is
    /// stale, reusing its capacity.
    pub fn set_text(&mut self, element: u32, text: &str, config: TextConfig) {
        let index = element as usize;
        assert!(self.elements[index].is_text, "not a text element");
        let (start, len) = (self.elements[index].text_start as usize, self.elements[index].text_len as usize);
        self.elements[index].text = config;
        if text.len() <= len && self.text.is_char_boundary(start + text.len()) {
            self.text.replace_range(start..start + text.len(), text);
            self.dead_text += len - text.len();
        } else {
            self.dead_text += len;
            let start = self.text.len() as u32;
            self.text.push_str(text);
            self.elements[index].text_start = start;
        }
        self.elements[index].text_len = text.len() as u32;
        if self.dead_text * 2 > self.text.len() {
            self.compact_text();
        }
    }
    /// Pack live text to the front of the arena.
    fn compact_text(&mut self) {
        let mut packed = std::mem::take(&mut self.spare_text);
        packed.clear();
        for element in self.elements.iter_mut().filter(|e| e.is_text) {
            let (start, len) = (element.text_start as usize, element.text_len as usize);
            element.text_start = packed.len() as u32;
            packed.push_str(&self.text[start..start + len]);
        }
        self.spare_text = std::mem::replace(&mut self.text, packed);
        self.dead_text = 0;
    }
    /// Point floating `element` at `target` (another element), or at
    /// nothing; for targets declared after it.
    pub fn set_attached(&mut self, element: u32, target: Option<u32>) {
        let target = target.filter(|&t| (t as usize) < self.elements.len()).unwrap_or(NONE);
        self.elements[element as usize].attached = target;
    }
    pub fn is_text(&self, element: u32) -> bool {
        self.elements[element as usize].is_text
    }
    /// A text element's text.
    pub fn text_of(&self, element: u32) -> &str {
        let e = &self.elements[element as usize];
        &self.text[e.text_start as usize..(e.text_start + e.text_len) as usize]
    }
    pub fn text_config(&self, element: u32) -> &TextConfig {
        &self.elements[element as usize].text
    }
    /// A text element's wrapped lines from the last layout.
    pub fn lines(&self, element: u32) -> &[Line] {
        let e = &self.elements[element as usize];
        &self.lines[e.lines_start as usize..(e.lines_start + e.lines_len) as usize]
    }
    /// The element's box from the last layout.
    pub fn rect(&self, element: u32) -> Rect {
        self.boxes[element as usize]
    }
    /// A clipping element's content size (children plus padding) from the
    /// last layout: how far it can scroll.
    pub fn content_size(&self, element: u32) -> Size {
        self.content[element as usize]
    }
    /// The element's children, in order (floating ones excluded).
    pub fn children(&self, element: u32) -> &[u32] {
        let e = &self.elements[element as usize];
        &self.children[e.children_start as usize..(e.children_start + e.children_len) as usize]
    }
    /// Roots in paint order from the last layout: floating elements and
    /// the main tree (element 0, at z 0), stably sorted by `z`.
    pub fn roots(&self) -> impl DoubleEndedIterator<Item = u32> + '_ {
        self.paint.iter().map(|root| root.element)
    }
    /// How many times a size distribution made no progress and was cut
    /// short, over the tree's lifetime. Clay loops forever in those cases
    /// (e.g. a child already sized below its minimum); this port stops.
    pub fn stalls(&self) -> u64 {
        self.stalls
    }
    /// The element a floating element is attached to.
    pub fn attached(&self, element: u32) -> Option<u32> {
        let attached = self.elements[element as usize].attached;
        (attached != NONE).then_some(attached)
    }

    /// Lay the tree out: fills boxes and wrapped lines. `measure` returns a
    /// string's size in a text config; results are cached in `cache`.
    pub fn layout(&mut self, cache: &mut TextCache, mut measure: impl FnMut(&str, &TextConfig) -> Size) {
        cache.next_generation();
        self.lines.clear();
        self.fit(cache, &mut measure);
        // Calculate sizing along the X axis.
        let mut text_elements = std::mem::take(&mut self.text_elements);
        let mut aspect = std::mem::take(&mut self.aspect);
        text_elements.clear();
        aspect.clear();
        self.size_along_axis(true, Some(&mut text_elements), Some(&mut aspect));
        self.wrap(cache, &text_elements, &mut measure);
        // Scale vertical heights according to aspect ratio.
        for &i in &aspect {
            let element = &mut self.elements[i as usize];
            element.dimensions.height = (1.0 / element.config.aspect_ratio) * element.dimensions.width;
            element.sizing[1].max = element.dimensions.height;
        }
        self.propagate_heights();
        self.size_along_axis(false, None, None);
        // Scale horizontal widths according to aspect ratio.
        for &i in &aspect {
            let element = &mut self.elements[i as usize];
            element.dimensions.width = element.config.aspect_ratio * element.dimensions.height;
        }
        self.text_elements = text_elements;
        self.aspect = aspect;
        // Sort tree roots by z (stable, as Clay's bubble sort).
        self.paint.clear();
        self.paint.extend_from_slice(&self.roots);
        self.paint.sort_by_key(|root| root.z);
        self.position();
    }

    /// Fit sizes, bottom-up: `Clay__OpenTextElement` and
    /// `Clay__CloseElement`, children before parents.
    fn fit(&mut self, cache: &mut TextCache, measure: &mut impl FnMut(&str, &TextConfig) -> Size) {
        for index in (0..self.elements.len()).rev() {
            let element = self.elements[index];
            if element.is_text {
                let text = &self.text[element.text_start as usize..(element.text_start + element.text_len) as usize];
                let key = cache.measure(text, &element.text, measure);
                let measured = cache.get(key);
                let height = if element.text.line_height > 0 {
                    element.text.line_height as f32
                } else {
                    measured.unwrapped.height
                };
                let element = &mut self.elements[index];
                element.measured = key;
                element.dimensions = Size::new(measured.unwrapped.width, height);
                element.min_dimensions = Size::new(measured.min_width, height);
                element.preferred = measured.unwrapped;
                continue;
            }
            let layout = element.config.layout;
            let clip_x = element.config.clip.horizontal;
            let clip_y = element.config.clip.vertical;
            let left_right = (layout.padding.left + layout.padding.right) as f32;
            let top_bottom = (layout.padding.top + layout.padding.bottom) as f32;
            let mut dimensions = Size::default();
            let mut min_dimensions = Size::default();
            let children = &self.children[element.children_start as usize..(element.children_start + element.children_len) as usize];
            let child_count = children.len() as i32;
            if layout.direction == Direction::LeftToRight {
                dimensions.width = left_right;
                min_dimensions.width = left_right;
                for &child in children {
                    let child = &self.elements[child as usize];
                    dimensions.width += child.dimensions.width;
                    dimensions.height = max(dimensions.height, child.dimensions.height + top_bottom);
                    if !clip_x {
                        min_dimensions.width += child.min_dimensions.width;
                    }
                    if !clip_y {
                        min_dimensions.height = max(min_dimensions.height, child.min_dimensions.height + top_bottom);
                    }
                }
                let gap = ((child_count - 1).max(0) * layout.gap as i32) as f32;
                dimensions.width += gap;
                if !clip_x {
                    min_dimensions.width += gap;
                }
            } else {
                dimensions.height = top_bottom;
                min_dimensions.height = top_bottom;
                for &child in children {
                    let child = &self.elements[child as usize];
                    dimensions.height += child.dimensions.height;
                    dimensions.width = max(dimensions.width, child.dimensions.width + left_right);
                    if !clip_y {
                        min_dimensions.height += child.min_dimensions.height;
                    }
                    if !clip_x {
                        min_dimensions.width = max(min_dimensions.width, child.min_dimensions.width + left_right);
                    }
                }
                let gap = ((child_count - 1).max(0) * layout.gap as i32) as f32;
                dimensions.height += gap;
                if !clip_y {
                    min_dimensions.height += gap;
                }
            }
            let mut sizing = [layout.width, layout.height];
            for (axis, x) in [(0, true), (1, false)] {
                let s = &mut sizing[axis];
                if s.kind != SizingKind::Percent {
                    if s.max <= 0.0 {
                        s.max = MAX;
                    }
                    *dimensions.axis(x) = min(max(dimensions.get(x), s.min), s.max);
                    *min_dimensions.axis(x) = min(max(min_dimensions.get(x), s.min), s.max);
                } else {
                    *dimensions.axis(x) = 0.0;
                }
            }
            let element = &mut self.elements[index];
            element.sizing = sizing;
            element.dimensions = dimensions;
            element.min_dimensions = min_dimensions;
            update_aspect_ratio(element);
        }
    }

    /// `Clay__SizeContainersAlongAxis`.
    fn size_along_axis(&mut self, x: bool, mut text_out: Option<&mut Vec<u32>>, mut aspect_out: Option<&mut Vec<u32>>) {
        let mut bfs = std::mem::take(&mut self.bfs);
        let mut resizable = std::mem::take(&mut self.resizable);
        for r in 0..self.roots.len() {
            bfs.clear();
            let root_index = self.roots[r].element;
            bfs.push(root_index);
            let root = self.elements[root_index as usize];
            // Size floating containers to their parents.
            if root.is_floating() && root.attached != NONE {
                let parent = self.elements[root.attached as usize].dimensions;
                let element = &mut self.elements[root_index as usize];
                match root.config.layout.width.kind {
                    SizingKind::Grow => element.dimensions.width = parent.width,
                    SizingKind::Percent => element.dimensions.width = parent.width * root.config.layout.width.share(),
                    _ => {}
                }
                match root.config.layout.height.kind {
                    SizingKind::Grow => element.dimensions.height = parent.height,
                    SizingKind::Percent => element.dimensions.height = parent.height * root.config.layout.height.share(),
                    _ => {}
                }
            }
            {
                let element = &mut self.elements[root_index as usize];
                for (axis, ax) in [(0, true), (1, false)] {
                    let s = element.sizing[axis];
                    if s.kind != SizingKind::Percent {
                        *element.dimensions.axis(ax) = min(max(element.dimensions.get(ax), s.min), s.max);
                    }
                }
            }
            let mut i = 0;
            while i < bfs.len() {
                let parent_index = bfs[i];
                i += 1;
                let parent = self.elements[parent_index as usize];
                let layout = parent.config.layout;
                let mut grow_count = 0;
                let parent_size = parent.dimensions.get(x);
                let parent_padding = layout.padding_along(x);
                let mut inner_content_size = 0f32;
                let mut total_padding_and_gaps = parent_padding;
                let along = (x && layout.direction == Direction::LeftToRight)
                    || (!x && layout.direction == Direction::TopToBottom);
                resizable.clear();
                let gap = layout.gap as f32;
                let mut first = true;
                let children = parent.children_start as usize..(parent.children_start + parent.children_len) as usize;
                for c in children.clone() {
                    let child_index = self.children[c];
                    let child = &self.elements[child_index as usize];
                    let sizing = child.sizing(x);
                    let child_size = child.dimensions.get(x);
                    if child.is_text {
                        if let Some(out) = text_out.as_deref_mut() {
                            out.push(child_index);
                        }
                    } else if child.children_len > 0 {
                        bfs.push(child_index);
                    }
                    if !child.is_text
                        && child.config.aspect_ratio != 0.0
                        && let Some(out) = aspect_out.as_deref_mut()
                    {
                        out.push(child_index);
                    }
                    if sizing.kind != SizingKind::Percent
                        && sizing.kind != SizingKind::Fixed
                        && (!child.is_text || child.text.wrap == Wrap::Words)
                    {
                        resizable.push(child_index);
                    }
                    if along {
                        inner_content_size += if sizing.kind == SizingKind::Percent { 0.0 } else { child_size };
                        if sizing.kind == SizingKind::Grow {
                            grow_count += 1;
                        }
                        if !first {
                            inner_content_size += gap;
                            total_padding_and_gaps += gap;
                        }
                    } else {
                        inner_content_size = max(child_size, inner_content_size);
                    }
                    first = false;
                }
                // Expand percentage containers to size.
                for c in children {
                    let child_index = self.children[c];
                    let child = &mut self.elements[child_index as usize];
                    let sizing = child.sizing(x);
                    if sizing.kind == SizingKind::Percent {
                        *child.dimensions.axis(x) = (parent_size - total_padding_and_gaps) * sizing.share();
                        if along {
                            inner_content_size += child.dimensions.get(x);
                        }
                        update_aspect_ratio(child);
                    }
                }
                if along {
                    let mut size_to_distribute = parent_size - parent_padding - inner_content_size;
                    if size_to_distribute < 0.0 {
                        // A parent clipping this axis does not compress.
                        if parent.clips(x) {
                            continue;
                        }
                        while size_to_distribute < -EPSILON && !resizable.is_empty() {
                            let (before, count) = (size_to_distribute, resizable.len());
                            let mut largest = 0f32;
                            let mut second_largest = 0f32;
                            let mut width_to_add = size_to_distribute;
                            for &r in &resizable {
                                let child_size = self.elements[r as usize].dimensions.get(x);
                                if float_equal(child_size, largest) {
                                    continue;
                                }
                                if child_size > largest {
                                    second_largest = largest;
                                    largest = child_size;
                                }
                                if child_size < largest {
                                    second_largest = max(second_largest, child_size);
                                    width_to_add = second_largest - largest;
                                }
                            }
                            width_to_add = max(width_to_add, size_to_distribute / resizable.len() as f32);
                            let mut c = 0isize;
                            while (c as usize) < resizable.len() {
                                let child = &mut self.elements[resizable[c as usize] as usize];
                                let min_size = child.min_dimensions.get(x);
                                let previous = child.dimensions.get(x);
                                if float_equal(previous, largest) {
                                    let size = child.dimensions.axis(x);
                                    *size += width_to_add;
                                    if *size <= min_size {
                                        *size = min_size;
                                        resizable.swap_remove(c as usize);
                                        c -= 1;
                                    }
                                    size_to_distribute -= child.dimensions.get(x) - previous;
                                }
                                c += 1;
                            }
                            if size_to_distribute == before && resizable.len() == count {
                                // Clay loops forever here (e.g. a child already
                                // below its minimum): stop instead.
                                self.stalls += 1;
                                break;
                            }
                        }
                    } else if size_to_distribute > 0.0 && grow_count > 0 {
                        let mut c = 0isize;
                        while (c as usize) < resizable.len() {
                            if self.elements[resizable[c as usize] as usize].sizing(x).kind != SizingKind::Grow {
                                resizable.swap_remove(c as usize);
                                c -= 1;
                            }
                            c += 1;
                        }
                        while size_to_distribute > EPSILON && !resizable.is_empty() {
                            let (before, count) = (size_to_distribute, resizable.len());
                            let mut smallest = MAX;
                            let mut second_smallest = MAX;
                            let mut width_to_add = size_to_distribute;
                            for &r in &resizable {
                                let child_size = self.elements[r as usize].dimensions.get(x);
                                if float_equal(child_size, smallest) {
                                    continue;
                                }
                                if child_size < smallest {
                                    second_smallest = smallest;
                                    smallest = child_size;
                                }
                                if child_size > smallest {
                                    second_smallest = min(second_smallest, child_size);
                                    width_to_add = second_smallest - smallest;
                                }
                            }
                            width_to_add = min(width_to_add, size_to_distribute / resizable.len() as f32);
                            let mut c = 0isize;
                            while (c as usize) < resizable.len() {
                                let child = &mut self.elements[resizable[c as usize] as usize];
                                let max_size = child.sizing(x).max;
                                let previous = child.dimensions.get(x);
                                if float_equal(previous, smallest) {
                                    let size = child.dimensions.axis(x);
                                    *size += width_to_add;
                                    if *size >= max_size {
                                        *size = max_size;
                                        resizable.swap_remove(c as usize);
                                        c -= 1;
                                    }
                                    size_to_distribute -= child.dimensions.get(x) - previous;
                                }
                                c += 1;
                            }
                            if size_to_distribute == before && resizable.len() == count {
                                self.stalls += 1;
                                break;
                            }
                        }
                    }
                } else {
                    // Sizing along the non layout axis.
                    for &r in &resizable {
                        let child = &mut self.elements[r as usize];
                        let sizing = child.sizing(x);
                        let min_size = child.min_dimensions.get(x);
                        let mut max_size = parent_size - parent_padding;
                        // Inside a scroll container, grow to the content.
                        if parent.clips(x) {
                            max_size = max(max_size, inner_content_size);
                        }
                        let size = child.dimensions.axis(x);
                        if sizing.kind == SizingKind::Grow {
                            *size = min(max_size, sizing.max);
                        }
                        *size = max(min_size, min(*size, max_size));
                    }
                }
            }
        }
        self.bfs = bfs;
        self.resizable = resizable;
    }

    /// Wrap text to final widths.
    fn wrap(&mut self, cache: &TextCache, text_elements: &[u32], measure: &mut impl FnMut(&str, &TextConfig) -> Size) {
        for &index in text_elements {
            let element = self.elements[index as usize];
            let measured = cache.get(element.measured);
            let lines_start = self.lines.len() as u32;
            let line_height = if element.text.line_height > 0 {
                element.text.line_height as f32
            } else {
                element.preferred.height
            };
            let text = &self.text[element.text_start as usize..(element.text_start + element.text_len) as usize];
            let width = element.dimensions.width;
            if !measured.contains_newlines && element.preferred.width <= width {
                self.lines.push(Line { start: 0, len: element.text_len, width: element.dimensions.width, height: element.dimensions.height });
                let element = &mut self.elements[index as usize];
                element.lines_start = lines_start;
                element.lines_len = 1;
                continue;
            }
            let space_width = measure(" ", &element.text).width;
            let letter_spacing = element.text.letter_spacing as f32;
            let bytes = text.as_bytes();
            let (mut line_width, mut line_length, mut line_start) = (0f32, 0u32, 0u32);
            let mut w = 0;
            while w < measured.words.len() {
                let word = measured.words[w];
                if line_length == 0 && line_width + word.width > width {
                    // Only word on the line is too large: render it anyway.
                    self.lines.push(Line { start: word.start, len: word.len, width: word.width, height: line_height });
                    w += 1;
                    line_start = word.start + word.len;
                } else if word.len == 0 || line_width + word.width > width {
                    let last = (line_start + line_length).saturating_sub(1) as usize;
                    let final_space = bytes.get(last) == Some(&b' ');
                    self.lines.push(Line {
                        start: line_start,
                        len: line_length.saturating_sub(final_space as u32),
                        width: line_width + if final_space { -space_width } else { 0.0 },
                        height: line_height,
                    });
                    if line_length == 0 || word.len == 0 {
                        w += 1;
                    }
                    line_width = 0.0;
                    line_length = 0;
                    line_start = word.start;
                } else {
                    line_width += word.width + letter_spacing;
                    line_length += word.len;
                    w += 1;
                }
            }
            if line_length > 0 {
                self.lines.push(Line { start: line_start, len: line_length, width: line_width - letter_spacing, height: line_height });
            }
            let element = &mut self.elements[index as usize];
            element.lines_start = lines_start;
            element.lines_len = self.lines.len() as u32 - lines_start;
            element.dimensions.height = line_height * element.lines_len as f32;
        }
    }

    /// Propagate text wrapping and aspect scaling to parents' heights,
    /// children first. Elements without children keep their height.
    fn propagate_heights(&mut self) {
        for index in (0..self.elements.len()).rev() {
            let element = self.elements[index];
            if element.is_text || element.children_len == 0 {
                continue;
            }
            let layout = element.config.layout;
            let sizing = element.sizing[1];
            let top_bottom = (layout.padding.top + layout.padding.bottom) as f32;
            let children = element.children_start as usize..(element.children_start + element.children_len) as usize;
            let mut height = element.dimensions.height;
            if layout.direction == Direction::LeftToRight {
                for c in children {
                    let child = &self.elements[self.children[c] as usize];
                    let with_padding = max(child.dimensions.height + layout.padding.top as f32 + layout.padding.bottom as f32, height);
                    height = min(max(with_padding, sizing.min), sizing.max);
                }
            } else {
                let mut content = top_bottom;
                for c in children {
                    content += self.elements[self.children[c] as usize].dimensions.height;
                }
                content += ((element.children_len as i32 - 1).max(0) * layout.gap as i32) as f32;
                height = min(max(content, sizing.min), sizing.max);
            }
            self.elements[index].dimensions.height = height;
        }
    }

    /// Final positions, depth-first from each root in paint order.
    fn position(&mut self) {
        let mut walk = std::mem::take(&mut self.walk);
        for r in 0..self.paint.len() {
            walk.clear();
            let root_index = self.paint[r].element;
            let root = self.elements[root_index as usize];
            let mut root_position = Vec2::default();
            if root.is_floating() {
                let config = root.config.floating;
                let size = root.dimensions;
                let parent = if root.attached == NONE { Rect::default() } else { self.boxes[root.attached as usize] };
                let mut target = Vec2::new(
                    match config.parent.x() {
                        0 => parent.x,
                        1 => parent.x + (parent.width / 2.0),
                        _ => parent.x + parent.width,
                    },
                    match config.parent.y() {
                        0 => parent.y,
                        1 => parent.y + (parent.height / 2.0),
                        _ => parent.y + parent.height,
                    },
                );
                match config.element.x() {
                    0 => {}
                    1 => target.x -= size.width / 2.0,
                    _ => target.x -= size.width,
                }
                match config.element.y() {
                    0 => {}
                    1 => target.y -= size.height / 2.0,
                    _ => target.y -= size.height,
                }
                target.x += config.offset.x;
                target.y += config.offset.y;
                root_position = target;
            }
            walk.push(Walk {
                element: root_index,
                position: root_position,
                next_child: Vec2::new(root.config.layout.padding.left as f32, root.config.layout.padding.top as f32),
                visited: false,
            });
            while let Some(node) = walk.last_mut() {
                if node.visited {
                    walk.pop();
                    continue;
                }
                node.visited = true;
                let node = *node;
                let top = walk.len() - 1;
                let element = self.elements[node.element as usize];
                let mut bounds = Rect::new(node.position.x, node.position.y, element.dimensions.width, element.dimensions.height);
                let mut scroll = Vec2::default();
                if !element.is_text {
                    if element.is_floating() {
                        let expand = element.config.floating.expand;
                        bounds.x -= expand.width;
                        bounds.width += expand.width * 2.0;
                        bounds.y -= expand.height;
                        bounds.height += expand.height * 2.0;
                    }
                    if element.config.clip.horizontal || element.config.clip.vertical {
                        scroll = element.config.clip.child_offset;
                    }
                }
                self.boxes[node.element as usize] = bounds;
                if element.is_text {
                    continue;
                }
                let layout = element.config.layout;
                let children = element.children_start as usize..(element.children_start + element.children_len) as usize;
                let gaps = ((element.children_len as i32 - 1).max(0) * layout.gap as i32) as f32;
                let mut content = Size::default();
                let mut next_child = node.next_child;
                if layout.direction == Direction::LeftToRight {
                    for c in children.clone() {
                        let child = &self.elements[self.children[c] as usize];
                        content.width += child.dimensions.width;
                        content.height = max(content.height, child.dimensions.height);
                    }
                    content.width += gaps;
                    let mut extra = element.dimensions.width - (layout.padding.left + layout.padding.right) as f32 - content.width;
                    match layout.align_x {
                        AlignX::Left => extra = 0.0,
                        AlignX::Center => extra /= 2.0,
                        AlignX::Right => {}
                    }
                    extra = max(0.0, extra);
                    next_child.x += extra;
                } else {
                    for c in children.clone() {
                        let child = &self.elements[self.children[c] as usize];
                        content.width = max(content.width, child.dimensions.width);
                        content.height += child.dimensions.height;
                    }
                    content.height += gaps;
                    let mut extra = element.dimensions.height - (layout.padding.top + layout.padding.bottom) as f32 - content.height;
                    match layout.align_y {
                        AlignY::Top => extra = 0.0,
                        AlignY::Center => extra /= 2.0,
                        AlignY::Bottom => {}
                    }
                    extra = max(0.0, extra);
                    next_child.y += extra;
                }
                if element.config.clip.horizontal || element.config.clip.vertical {
                    self.content[node.element as usize] = Size::new(
                        content.width + (layout.padding.left + layout.padding.right) as f32,
                        content.height + (layout.padding.top + layout.padding.bottom) as f32,
                    );
                }
                // Children go on the stack in reverse, so they pop in order.
                let base = walk.len() + element.children_len as usize;
                walk.resize(base, walk[top]);
                for (i, c) in children.enumerate() {
                    let child_index = self.children[c];
                    let child = &self.elements[child_index as usize];
                    if layout.direction == Direction::LeftToRight {
                        next_child.y = layout.padding.top as f32;
                        let white_space = element.dimensions.height - (layout.padding.top + layout.padding.bottom) as f32 - child.dimensions.height;
                        match layout.align_y {
                            AlignY::Top => {}
                            AlignY::Center => next_child.y += white_space / 2.0,
                            AlignY::Bottom => next_child.y += white_space,
                        }
                    } else {
                        next_child.x = layout.padding.left as f32;
                        let white_space = element.dimensions.width - (layout.padding.left + layout.padding.right) as f32 - child.dimensions.width;
                        match layout.align_x {
                            AlignX::Left => {}
                            AlignX::Center => next_child.x += white_space / 2.0,
                            AlignX::Right => next_child.x += white_space,
                        }
                    }
                    let position = Vec2::new(bounds.x + next_child.x + scroll.x, bounds.y + next_child.y + scroll.y);
                    let (padding_left, padding_top) = if child.is_text {
                        (0.0, 0.0)
                    } else {
                        (child.config.layout.padding.left as f32, child.config.layout.padding.top as f32)
                    };
                    walk[base - 1 - i] = Walk {
                        element: child_index,
                        position,
                        next_child: Vec2::new(padding_left, padding_top),
                        visited: false,
                    };
                    if layout.direction == Direction::LeftToRight {
                        next_child.x += child.dimensions.width + layout.gap as f32;
                    } else {
                        next_child.y += child.dimensions.height + layout.gap as f32;
                    }
                }
                walk[top].next_child = next_child;
            }
        }
        self.walk = walk;
    }
}

/// `Clay__UpdateAspectRatioBox`.
fn update_aspect_ratio(element: &mut Element) {
    let ratio = element.config.aspect_ratio;
    if ratio != 0.0 {
        if element.dimensions.width == 0.0 && element.dimensions.height != 0.0 {
            element.dimensions.width = element.dimensions.height * ratio;
        } else if element.dimensions.width != 0.0 && element.dimensions.height == 0.0 {
            element.dimensions.height = element.dimensions.width * (1.0 / ratio);
        }
    }
}
