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
    /// The element's entity; `None` for debug overlay marks.
    pub entity: Option<Entity>,
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
    debug: DebugOverlay,
    /// Damage marked by the overlay last frame, to erase.
    flashed: Vec<layout::Rect>,
    build_time: std::time::Duration,
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
    /// How long the last rebuild took.
    pub fn build_time(&self) -> std::time::Duration {
        self.build_time
    }
}

/// Debugging aids drawn over the UI: a resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DebugOverlay {
    /// Outline every element's box.
    pub boxes: bool,
    /// Tint what each frame redraws.
    pub damage: bool,
    /// Show layout and render-list timings in a corner.
    pub timings: bool,
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
    debug: Option<Res<DebugOverlay>>,
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
    let debug = debug.map(|d| *d).unwrap_or_default();
    if debug.damage {
        // Erase last frame's marks; mark this frame's damage.
        let previous = std::mem::take(&mut list.flashed);
        list.flashed = list.damage.clone();
        list.damage.extend(previous);
        changed |= !list.flashed.is_empty();
    } else if !list.flashed.is_empty() {
        let previous = std::mem::take(&mut list.flashed);
        list.damage.extend(previous);
        changed = true;
    }
    if !changed && ui.runs() == list.built_from && debug == list.debug {
        return;
    }
    let started = std::time::Instant::now();
    list.debug = debug;
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
    if debug.boxes {
        // Every element's box, outlined.
        for element in 1..tree.len() as u32 {
            let rect = tree.rect(element);
            list.commands.push(Command {
                entity: Some(ui.entity(element)),
                element,
                rect,
                clip: window,
                draw: Draw::Border { color: Color::rgba(255, 0, 200, 160), width: BorderWidth::all(1), radius: Radius::default() },
            });
        }
    }
    for &area in &list.flashed {
        list.commands.push(Command { entity: None, element: 0, rect: area, clip: window, draw: Draw::Overlay { color: Color::rgba(255, 40, 40, 70) } });
    }
    list.build_time = started.elapsed();
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
        let Some((background, border, radius, overlay, text, clip_config)) = styles.get_mut(ui.entity(element)) else {
            continue;
        };
        let entity = Some(ui.entity(element));
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

/// Marks the overlay's timings text.
#[derive(Clone, Copy, Debug, Default)]
pub struct DebugTimings;
/// Show (or hide) the timings corner of the debug overlay.
#[allow(clippy::type_complexity)]
pub fn debug_timings_system<S: Storage>(
    debug: Option<Res<DebugOverlay>>,
    ui: Res<UiLayout>,
    list: Res<RenderList>,
    mut texts: Query<(&mut Text, &Relates<crate::ChildOf>), With<DebugTimings>>,
    mut commands: Commands<S>,
) {
    let on = debug.is_some_and(|d| d.timings);
    let mut found = false;
    texts.each(|(mut text, parent)| {
        found = true;
        if !on {
            commands.despawn(parent.target());
            return;
        }
        let shown = format!(
            "layout {:.1} µs · {} runs, {} rebuilds · render list {:.1} µs, {} builds",
            ui.layout_time().as_secs_f64() * 1e6,
            ui.runs(),
            ui.rebuilds(),
            list.build_time().as_secs_f64() * 1e6,
            list.builds(),
        );
        if text.text != shown {
            text.text = shown;
        }
    });
    if on && !found {
        let corner = commands.spawn((
            crate::Node::row().padding(crate::Padding::xy(6, 3)),
            Background(Color::rgba(20, 20, 24, 200)),
            crate::Floating {
                attach: crate::Attach::Root,
                element: crate::AttachPoint::RightTop,
                parent: crate::AttachPoint::RightTop,
                z: i16::MAX,
                pointer: crate::PointerCapture::Passthrough,
                ..crate::Floating::default()
            },
        ));
        commands.spawn((Text::new("", 12).color(Color::WHITE), DebugTimings, crate::ChildOf.to(corner)));
    }
}
