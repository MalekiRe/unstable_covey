//! clay.h as an oracle for `static-ui-layout`: random trees are laid out by
//! both, through the same text measurement, and compared box by box.
use static_ui_layout::{
    AlignX, AlignY, AttachPoint, AttachTo, Clip, Config, Direction, Floating, Layout, Padding, Rect, Size, Sizing,
    TextAlign, TextCache, TextConfig, Tree, Vec2, Wrap,
};

/// One node of a pre-order tree description; mirrors `OracleNode` in
/// `clay/oracle.c`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub kind: i32,
    pub child_count: i32,
    pub width_type: i32,
    pub width_min: f32,
    pub width_max: f32,
    pub height_type: i32,
    pub height_min: f32,
    pub height_max: f32,
    pub pad_left: u16,
    pub pad_right: u16,
    pub pad_top: u16,
    pub pad_bottom: u16,
    pub gap: u16,
    pub align_x: u8,
    pub align_y: u8,
    pub direction: u8,
    pub attach_to: i32,
    pub attach_element: i32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub expand_w: f32,
    pub expand_h: f32,
    pub z: i16,
    pub element_point: u8,
    pub parent_point: u8,
    pub clip_to: u8,
    pub clip_h: u8,
    pub clip_v: u8,
    pub child_offset_x: f32,
    pub child_offset_y: f32,
    pub aspect: f32,
    pub text_start: i32,
    pub text_len: i32,
    pub font: u16,
    pub size: u16,
    pub letter_spacing: u16,
    pub line_height: u16,
    pub wrap: u8,
    pub text_align: u8,
}

/// A text render command: its box and byte length.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextLine {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub length: i32,
}

unsafe extern "C" {
    fn oracle_init(max_elements: i32);
    fn oracle_layout(
        nodes: *const Node,
        count: i32,
        text: *const u8,
        width: f32,
        height: f32,
        boxes: *mut Rect,
        lines: *mut TextLine,
        max_lines: i32,
        errors: *mut i32,
    ) -> i32;
    fn oracle_layout_only(nodes: *const Node, count: i32, text: *const u8, width: f32, height: f32);
    fn oracle_measure_width(chars: *const u8, length: i32, size: u16, letter_spacing: u16) -> f32;
    fn oracle_line_height(size: u16) -> f32;
}

/// The measurement both sides use.
pub fn measure(text: &str, config: &TextConfig) -> Size {
    // SAFETY: a valid byte range.
    unsafe {
        Size::new(
            oracle_measure_width(text.as_ptr(), text.len() as i32, config.size, config.letter_spacing),
            oracle_line_height(config.size),
        )
    }
}

/// A tree description: nodes in pre-order (top-level nodes are the root's
/// children) and the text they slice.
#[derive(Clone, Debug, Default)]
pub struct Case {
    pub nodes: Vec<Node>,
    pub text: String,
    pub width: f32,
    pub height: f32,
}

/// What a layout produced: each node's box (containers only; text nodes
/// get their parent's slot unused) and the text commands in paint order.
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub boxes: Vec<Rect>,
    pub lines: Vec<TextLine>,
}

/// clay.h, set up once per thread of use. Clay's context is global: use
/// one `Clay` at a time.
pub struct Clay(());
impl Clay {
    pub fn new(max_elements: i32) -> Self {
        // SAFETY: allocates Clay's arena.
        unsafe { oracle_init(max_elements) };
        Self(())
    }
    /// Lay `case` out `frames` times (a fresh empty frame first, so no
    /// earlier boxes leak in); returns the last frame and Clay's error count.
    pub fn layout(&mut self, case: &Case, frames: usize) -> (Output, i32) {
        let empty = Case { width: case.width, height: case.height, ..Case::default() };
        self.frame(&empty);
        let mut result = (Output::default(), 0);
        for _ in 0..frames {
            result = self.frame(case);
        }
        result
    }
    fn frame(&mut self, case: &Case) -> (Output, i32) {
        let mut boxes = vec![Rect::default(); case.nodes.len()];
        let mut lines = vec![TextLine::default(); 4096];
        let mut errors = 0;
        // SAFETY: buffers sized as passed; nodes and text outlive the call.
        let written = unsafe {
            oracle_layout(
                case.nodes.as_ptr(),
                case.nodes.len() as i32,
                case.text.as_ptr(),
                case.width,
                case.height,
                boxes.as_mut_ptr(),
                lines.as_mut_ptr(),
                lines.len() as i32,
                &mut errors,
            )
        };
        lines.truncate(written as usize);
        (Output { boxes, lines }, errors)
    }
    /// One layout, for timing.
    pub fn layout_only(&mut self, case: &Case) {
        // SAFETY: as in `frame`.
        unsafe { oracle_layout_only(case.nodes.as_ptr(), case.nodes.len() as i32, case.text.as_ptr(), case.width, case.height) };
    }
}

fn sizing(kind: i32, min: f32, max: f32) -> Sizing {
    match kind {
        0 => Sizing::fit(min, max),
        1 => Sizing::grow(min, max),
        2 => Sizing::percent(min),
        _ => Sizing::fixed(min),
    }
}
fn point(p: u8) -> AttachPoint {
    [
        AttachPoint::LeftTop,
        AttachPoint::LeftCenter,
        AttachPoint::LeftBottom,
        AttachPoint::CenterTop,
        AttachPoint::CenterCenter,
        AttachPoint::CenterBottom,
        AttachPoint::RightTop,
        AttachPoint::RightCenter,
        AttachPoint::RightBottom,
    ][p as usize]
}

/// Declare `case` into `tree`; returns each node's element index.
pub fn declare(tree: &mut Tree, case: &Case) -> Vec<u32> {
    let mut elements = vec![0; case.nodes.len()];
    tree.begin(case.width, case.height);
    fn build(tree: &mut Tree, case: &Case, i: usize, elements: &mut [u32]) -> usize {
        let n = case.nodes[i];
        if n.kind == 1 {
            let text = &case.text[n.text_start as usize..(n.text_start + n.text_len) as usize];
            elements[i] = tree.text(text, text_config(&n));
            return i + 1;
        }
        let attach_to = match n.attach_to {
            0 => AttachTo::None,
            1 => AttachTo::Parent,
            2 => AttachTo::Element(elements[n.attach_element as usize]),
            _ => AttachTo::Root,
        };
        let config = Config {
            layout: Layout {
                width: sizing(n.width_type, n.width_min, n.width_max),
                height: sizing(n.height_type, n.height_min, n.height_max),
                padding: Padding { left: n.pad_left, right: n.pad_right, top: n.pad_top, bottom: n.pad_bottom },
                gap: n.gap,
                align_x: [AlignX::Left, AlignX::Right, AlignX::Center][n.align_x as usize],
                align_y: [AlignY::Top, AlignY::Bottom, AlignY::Center][n.align_y as usize],
                direction: [Direction::LeftToRight, Direction::TopToBottom][n.direction as usize],
            },
            floating: Floating {
                attach_to,
                offset: Vec2::new(n.offset_x, n.offset_y),
                expand: Size::new(n.expand_w, n.expand_h),
                z: n.z,
                element: point(n.element_point),
                parent: point(n.parent_point),
                clip_to_parent: n.clip_to == 1,
                ..Floating::default()
            },
            clip: Clip { horizontal: n.clip_h != 0, vertical: n.clip_v != 0, child_offset: Vec2::new(n.child_offset_x, n.child_offset_y) },
            aspect_ratio: n.aspect,
        };
        elements[i] = tree.open(config);
        let mut next = i + 1;
        for _ in 0..n.child_count {
            next = build(tree, case, next, elements);
        }
        tree.close();
        next
    }
    let mut next = 0;
    while next < case.nodes.len() {
        next = build(tree, case, next, &mut elements);
    }
    tree.end();
    elements
}
fn text_config(n: &Node) -> TextConfig {
    TextConfig {
        font: n.font,
        size: n.size,
        letter_spacing: n.letter_spacing,
        line_height: n.line_height,
        wrap: [Wrap::Words, Wrap::Newlines, Wrap::None][n.wrap as usize],
        align: [TextAlign::Left, TextAlign::Center, TextAlign::Right][n.text_align as usize],
    }
}

/// Lay `case` out with static-ui-layout in a fresh tree, `frames` times;
/// the output mirrors Clay's: container boxes, then text commands in paint
/// order (lines of zero length skipped, aligned and offset as Clay's are).
/// `None` if the layout stalled where Clay would loop forever.
pub fn port(case: &Case, cache: &mut TextCache, frames: usize) -> Option<Output> {
    let mut tree = Tree::new();
    // An empty frame first, as `Clay::layout` does: the root's box exists.
    declare(&mut tree, &Case { width: case.width, height: case.height, ..Case::default() });
    tree.layout(cache, measure);
    let elements = declare(&mut tree, case);
    for _ in 0..frames {
        tree.layout(cache, measure);
    }
    let boxes = case
        .nodes
        .iter()
        .zip(&elements)
        .map(|(n, &e)| if n.kind == 0 { tree.rect(e) } else { Rect::default() })
        .collect();
    if tree.stalls() > 0 {
        return None;
    }
    let mut lines = Vec::new();
    for root in tree.roots() {
        paint(&tree, root, &mut lines);
    }
    Some(Output { boxes, lines })
}
fn paint(tree: &Tree, element: u32, out: &mut Vec<TextLine>) {
    if tree.is_text(element) {
        let bounds = tree.rect(element);
        let config = tree.text_config(element);
        // Every word measures one natural line height.
        let natural = measure("", config).height;
        let line_height = if config.line_height > 0 { config.line_height as f32 } else { natural };
        let mut y = (line_height - natural) / 2.0;
        for line in tree.lines(element) {
            if line.len == 0 {
                y += line_height;
                continue;
            }
            let mut offset = bounds.width - line.width;
            match config.align {
                TextAlign::Left => offset = 0.0,
                TextAlign::Center => offset /= 2.0,
                TextAlign::Right => {}
            }
            out.push(TextLine { x: bounds.x + offset, y: bounds.y + y, width: line.width, height: line.height, length: line.len as i32 });
            y += line_height;
        }
        return;
    }
    for &child in tree.children(element) {
        paint(tree, child, out);
    }
}

/// A small xorshift generator: deterministic cases from a seed.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn bits(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.bits() % n
    }
    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub fn float(&mut self, max: u32) -> f32 {
        self.below(max as u64 + 1) as f32
    }
}

const WORDS: &[&str] = &["clay", "layout", "is", "a", "flex", "box", "model", "with", "little", "illusion", "wrapping", "text", "and", "lists"];

/// Parameters for random trees.
#[derive(Clone, Copy)]
pub struct Shape {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_children: u64,
    pub floating_percent: u64,
    pub text_percent: u64,
}
impl Default for Shape {
    fn default() -> Self {
        Self { max_nodes: 60, max_depth: 5, max_children: 5, floating_percent: 8, text_percent: 30 }
    }
}

/// A random tree within `shape`.
pub fn random_case(rng: &mut Rng, shape: Shape) -> Case {
    let mut case = Case { width: 200.0 + rng.float(800), height: 150.0 + rng.float(600), ..Case::default() };
    let top = 1 + rng.below(3);
    for _ in 0..top {
        if case.nodes.len() >= shape.max_nodes {
            break;
        }
        node(rng, &mut case, shape, 1);
    }
    case
}
fn node(rng: &mut Rng, case: &mut Case, shape: Shape, depth: usize) {
    let index = case.nodes.len();
    if depth > 1 && rng.chance(shape.text_percent) {
        let mut text = String::new();
        for w in 0..1 + rng.below(8) {
            if w > 0 {
                text.push(if rng.chance(10) { '\n' } else { ' ' });
            }
            text.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        }
        if rng.chance(10) {
            text.push(' ');
        }
        let start = case.text.len();
        case.text.push_str(&text);
        case.nodes.push(Node {
            kind: 1,
            text_start: start as i32,
            text_len: text.len() as i32,
            font: 0,
            size: [10, 12, 16, 20][rng.below(4) as usize],
            letter_spacing: rng.below(3) as u16,
            line_height: if rng.chance(30) { 14 + rng.below(16) as u16 } else { 0 },
            wrap: if rng.chance(70) { 0 } else { 1 + rng.below(2) as u8 },
            text_align: rng.below(3) as u8,
            ..Node::default()
        });
        return;
    }
    let mut n = Node::default();
    for axis in 0..2 {
        let kind = rng.below(4) as i32;
        let (min, max) = match kind {
            0 | 1 => {
                let min = if rng.chance(30) { rng.float(80) } else { 0.0 };
                let max = if rng.chance(30) { min + rng.float(300) } else { 0.0 };
                (min, max)
            }
            2 => ((1 + rng.below(100)) as f32 / 100.0, 0.0),
            _ => {
                let size = rng.float(300);
                (size, size)
            }
        };
        if axis == 0 {
            (n.width_type, n.width_min, n.width_max) = (kind, min, max);
        } else {
            (n.height_type, n.height_min, n.height_max) = (kind, min, max);
        }
    }
    if rng.chance(50) {
        n.pad_left = rng.below(13) as u16;
        n.pad_right = rng.below(13) as u16;
        n.pad_top = rng.below(13) as u16;
        n.pad_bottom = rng.below(13) as u16;
    }
    n.gap = if rng.chance(50) { rng.below(11) as u16 } else { 0 };
    n.align_x = rng.below(3) as u8;
    n.align_y = rng.below(3) as u8;
    n.direction = rng.below(2) as u8;
    if rng.chance(shape.floating_percent) {
        n.attach_to = 1 + rng.below(3) as i32;
        if n.attach_to == 2 {
            // An earlier container (Clay finds it by id when declared).
            let earlier: Vec<usize> = (0..index).filter(|&i| case.nodes[i].kind == 0).collect();
            if earlier.is_empty() {
                n.attach_to = 1;
            } else {
                n.attach_element = earlier[rng.below(earlier.len() as u64) as usize] as i32;
            }
        }
        n.offset_x = rng.float(40) - 20.0;
        n.offset_y = rng.float(40) - 20.0;
        if rng.chance(20) {
            n.expand_w = rng.float(6);
            n.expand_h = rng.float(6);
        }
        n.z = rng.below(7) as i16 - 3;
        n.element_point = rng.below(9) as u8;
        n.parent_point = rng.below(9) as u8;
        n.clip_to = rng.below(2) as u8;
    }
    if rng.chance(15) {
        n.clip_h = rng.below(2) as u8;
        n.clip_v = rng.below(2) as u8;
        if rng.chance(50) {
            n.child_offset_x = -rng.float(30);
            n.child_offset_y = -rng.float(30);
        }
    }
    if rng.chance(8) {
        n.aspect = [0.5, 0.75, 1.0, 1.5, 2.0][rng.below(5) as usize];
    }
    case.nodes.push(n);
    let mut count = 0;
    if depth < shape.max_depth {
        for _ in 0..rng.below(shape.max_children + 1) {
            if case.nodes.len() >= shape.max_nodes {
                break;
            }
            node(rng, case, shape, depth + 1);
            count += 1;
        }
    }
    case.nodes[index].child_count = count;
}

/// A deterministic, text-heavy, roughly UI-shaped tree of about `count`
/// elements: rows of labeled cells in panels, for timing.
pub fn grid_case(count: usize) -> Case {
    let mut case = Case { width: 1280.0, height: 800.0, ..Case::default() };
    let words = ["Settings", "Volume", "Brightness", "Network", "Wi-Fi", "Bluetooth", "Display", "About"];
    let mut panel = Node {
        width_type: 1,
        height_type: 1,
        direction: 1,
        gap: 4,
        pad_left: 8,
        pad_right: 8,
        pad_top: 8,
        pad_bottom: 8,
        ..Node::default()
    };
    let rows = count / 5;
    panel.child_count = rows as i32;
    case.nodes.push(panel);
    for r in 0..rows {
        case.nodes.push(Node { width_type: 1, gap: 6, child_count: 2, align_y: 2, ..Node::default() });
        case.nodes.push(Node { width_type: 3, width_min: 24.0, width_max: 24.0, height_type: 3, height_min: 24.0, height_max: 24.0, ..Node::default() });
        case.nodes.push(Node { width_type: 1, direction: 1, child_count: 2, ..Node::default() });
        for line in 0..2 {
            let text = words[(r + line) % words.len()];
            let start = case.text.len();
            case.text.push_str(text);
            case.nodes.push(Node { kind: 1, text_start: start as i32, text_len: text.len() as i32, size: 14, ..Node::default() });
        }
    }
    case
}
