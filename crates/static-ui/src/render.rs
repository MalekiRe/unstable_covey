//! Render commands: after layout, the visible tree becomes a flat list of
//! drawing commands in paint order (floating roots by z, then depth-first),
//! each carrying its entity and its clip rectangle, culled to the window.
//! Backends draw it; damage says which parts of the window to redraw.
use static_ecs::prelude::*;
use static_ui_layout::{self as layout, Line, TextConfig};

use crate::{Background, Border, BorderWidth, Clip, Color, Overlay, Radius, Text, UiLayout, Viewport};

/// What a command draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Draw {
    /// A filled, possibly rounded, rectangle.
    Rect { color: Color, radius: Radius },
    /// Borders inside the box.
    Border { color: Color, width: BorderWidth, radius: Radius },
    /// One wrapped line of a text element.
    Text { line: Line, color: Color, config: TextConfig },
    /// A translucent color over the element and what it contains.
    Overlay { color: Color },
}
/// One drawing command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Command {
    pub entity: Entity,
    /// The element in the flattened tree (for text: where its string is).
    pub element: u32,
    /// Where to draw.
    pub rect: layout::Rect,
    /// Drawing outside this is cut off (scroll containers).
    pub clip: layout::Rect,
    pub draw: Draw,
}

/// The window's background, drawn under everything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClearColor(pub Color);
impl Default for ClearColor {
    fn default() -> Self {
        Self(Color::rgb(250, 250, 250))
    }
}

/// The commands of the last frame, and what changed since a backend last
/// drew: a resource.
#[derive(Default)]
pub struct RenderList {
    commands: Vec<Command>,
    damage: Vec<layout::Rect>,
    /// The layout run the list was built from.
    built_from: u64,
    builds: u64,
    clips: Vec<layout::Rect>,
    roots: Vec<u32>,
    walk: Vec<(u32, bool)>,
    clip_stack: Vec<layout::Rect>,
}
impl RenderList {
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }
    /// Times the list was rebuilt: an unchanged UI rebuilds nothing.
    pub fn builds(&self) -> u64 {
        self.builds
    }
    /// Areas to redraw since the last `take_damage`, merged into a few
    /// rectangles.
    pub fn take_damage(&mut self) -> Vec<layout::Rect> {
        merge(std::mem::take(&mut self.damage))
    }
    pub fn has_damage(&self) -> bool {
        !self.damage.is_empty()
    }
}

/// Merge overlapping rectangles; past 16 of them, their union.
fn merge(mut rects: Vec<layout::Rect>) -> Vec<layout::Rect> {
    rects.retain(|r| !r.is_empty());
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if !rects[i].intersect(rects[j]).is_empty() {
                    rects[i] = rects[i].union(rects[j]);
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    if rects.len() > 16 {
        let all = rects.iter().fold(layout::Rect::default(), |a, &r| a.union(r));
        rects.clear();
        rects.push(all);
    }
    rects
}

type Styles = (
    Option<&'static Background>,
    Option<&'static Border>,
    Option<&'static Radius>,
    Option<&'static Overlay>,
    Option<&'static Text>,
    Option<&'static Clip>,
);
type Restyled<'w> = Query<'w, &'static crate::Rect, Or<(Changed<Background>, Changed<Border>, Changed<Radius>)>>;

/// Build the render list if the layout ran or a style changed, and collect
/// damage: layout's (moves, departures) plus restyled elements' boxes.
pub fn render_system(
    mut ui: ResMut<UiLayout>,
    mut list: ResMut<RenderList>,
    viewport: Res<Viewport>,
    mut styles: Query<Styles>,
    mut restyled: Restyled,
    mut overlaid: Query<&crate::Rect, Changed<Overlay>>,
) {
    let list = &mut *list;
    let mut changed = false;
    restyled.each(|rect| {
        list.damage.push(rect.0);
        changed = true;
    });
    overlaid.each(|rect| {
        list.damage.push(rect.0);
        changed = true;
    });
    list.damage.extend(ui.take_damage());
    let ui = &*ui;
    if !changed && ui.runs() == list.built_from {
        return;
    }
    list.built_from = ui.runs();
    list.builds += 1;
    list.commands.clear();
    let window = layout::Rect::new(0.0, 0.0, viewport.width, viewport.height);
    let tree = ui.tree();
    // Each element's effective clip, filled as elements are painted.
    list.clips.clear();
    list.clips.resize(tree.len(), window);
    let mut roots = std::mem::take(&mut list.roots);
    roots.clear();
    roots.extend(tree.roots());
    for &root in &roots {
        // A floating element clipped to its parent takes the attached
        // element's clip (painted already when it has a lower z).
        let clip = match (root, tree.config(root).floating.clip_to_parent, tree.attached(root)) {
            (0, ..) => window,
            (_, true, Some(parent)) if parent != 0 => list.clips[parent as usize],
            _ => window,
        };
        paint(ui, list, &mut styles, root, clip, window);
    }
    list.roots = roots;
}

/// Commands for `root`'s subtree, depth-first: an element's background
/// before its children, its border and overlay after them.
fn paint(ui: &UiLayout, list: &mut RenderList, styles: &mut Query<Styles>, root: u32, clip: layout::Rect, window: layout::Rect) {
    let tree = ui.tree();
    // (element, leaving): entering paints the background and pushes the
    // children; leaving paints border and overlay.
    let mut stack = std::mem::take(&mut list.walk);
    let mut clips = std::mem::take(&mut list.clip_stack);
    stack.clear();
    clips.clear();
    stack.push((root, false));
    clips.push(clip);
    while let Some((element, leaving)) = stack.pop() {
        let rect = tree.rect(element);
        if element == 0 {
            // The window root draws nothing.
            if !leaving {
                stack.push((0, true));
                stack.extend(tree.children(0).iter().rev().map(|&c| (c, false)));
                clips.push(clip);
            } else {
                clips.pop();
            }
            continue;
        }
        let entity = ui.entity(element);
        let Some((background, border, radius, overlay, text, clip_config)) = styles.get_mut(entity) else {
            continue;
        };
        let clip = *clips.last().unwrap();
        let radius = radius.copied().unwrap_or_default();
        let visible = !rect.intersect(window).is_empty();
        if leaving {
            if visible {
                if let Some(border) = border {
                    list.commands.push(Command {
                        entity,
                        element,
                        rect,
                        clip,
                        draw: Draw::Border { color: border.color, width: border.width, radius },
                    });
                }
                if let Some(overlay) = overlay {
                    list.commands.push(Command { entity, element, rect, clip, draw: Draw::Overlay { color: overlay.0 } });
                }
            }
            clips.pop();
            continue;
        }
        list.clips[element as usize] = clip;
        if let Some(text) = text {
            if visible {
                let config = text.config;
                let natural = tree.lines(element).first().map_or(0.0, |l| l.height);
                let line_height = if config.line_height > 0 { config.line_height as f32 } else { natural };
                let mut y = 0.0;
                for line in tree.lines(element) {
                    if line.len > 0 {
                        let mut offset = rect.width - line.width;
                        match config.align {
                            layout::TextAlign::Left => offset = 0.0,
                            layout::TextAlign::Center => offset /= 2.0,
                            layout::TextAlign::Right => {}
                        }
                        let line_rect = layout::Rect::new(rect.x + offset, rect.y + y, line.width, line_height);
                        list.commands.push(Command {
                            entity,
                            element,
                            rect: line_rect,
                            clip,
                            draw: Draw::Text { line: *line, color: text.color, config },
                        });
                    }
                    y += line_height;
                }
            }
            continue;
        }
        if visible && let Some(background) = background {
            list.commands.push(Command { entity, element, rect, clip, draw: Draw::Rect { color: background.0, radius } });
        }
        let inner = if clip_config.is_some_and(|c| c.horizontal || c.vertical) { clip.intersect(rect) } else { clip };
        clips.push(inner);
        stack.push((element, true));
        stack.extend(tree.children(element).iter().rev().map(|&c| (c, false)));
    }
    list.walk = stack;
    list.clip_stack = clips;
}
