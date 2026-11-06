//! Drawing a render list into a canvas, only inside damaged rectangles:
//! each damaged area is cleared and every command that reaches it is drawn
//! again in paint order, clipped to it.
use static_ui::layout::Rect;
use static_ui::{BorderWidth, Color, Draw, Radius, RenderList, UiLayout};

use crate::canvas::Canvas;
use crate::font::{Atlas, Fonts};

/// What a paint call did.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Painted {
    /// Commands drawn (counted once per damaged rectangle they reach).
    pub commands: u64,
    /// Pixels written.
    pub pixels: u64,
}

/// Redraw `damage` (buffer pixels) from `list`, whose coordinates are
/// logical: `scale` pixels per unit (1 on an unscaled display, 1.5 at 150%).
#[allow(clippy::too_many_arguments)]
pub fn paint(
    canvas: &mut Canvas,
    list: &RenderList,
    ui: &UiLayout,
    fonts: &Fonts,
    atlas: &mut Atlas,
    clear: Color,
    scale: f32,
    damage: impl IntoIterator<Item = Rect>,
) -> Painted {
    let s = scale;
    let px = |r: Rect| Rect::new(r.x * s, r.y * s, r.width * s, r.height * s);
    let before = canvas.touched;
    let mut painted = Painted::default();
    for area in damage {
        // Whole pixels: partially covered edge pixels are redrawn whole.
        let span = canvas.span(area, area);
        let area = Rect::new(span.x0 as f32, span.y0 as f32, (span.x1 - span.x0) as f32, (span.y1 - span.y0) as f32);
        if area.is_empty() {
            continue;
        }
        canvas.clear(span, clear);
        for command in list.commands() {
            let (rect, clip) = (px(command.rect), px(command.clip).intersect(area));
            if rect.intersect(clip).is_empty() {
                continue;
            }
            painted.commands += 1;
            match command.draw {
                Draw::Rect { color, radius } => canvas.fill(rect, color, &scaled_radius(&radius, s), clip),
                Draw::Border { color, width, radius } => canvas.border(rect, &scaled_width(&width, s), color, &scaled_radius(&radius, s), clip),
                Draw::Overlay { color } => canvas.fill(rect, color, &Default::default(), clip),
                Draw::Text { line, color, config } => {
                    let text = ui.tree().text_of(command.element);
                    let text = &text[line.start as usize..(line.start + line.len) as usize];
                    // Glyphs rastered at the pixel size, not scaled up.
                    let size = (config.size as f32 * s).round() as u16;
                    let ascent = fonts.ascent(config.font, size);
                    let natural = fonts.line_height(config.font, size);
                    let baseline = (rect.y + (rect.height - natural) / 2.0 + ascent).round() as i32;
                    let mut pen = rect.x;
                    for ch in text.chars() {
                        let advance = fonts.advance(config.font, size, ch);
                        let x = pen.floor();
                        let (glyph, coverage) = atlas.glyph(fonts, config.font, size, ch, pen - x);
                        if glyph.width > 0 {
                            canvas.glyph(
                                coverage,
                                glyph.width as usize,
                                glyph.height as usize,
                                x as i32 + glyph.left as i32,
                                baseline + glyph.top as i32,
                                color,
                                clip,
                            );
                        }
                        pen += advance + config.letter_spacing as f32 * s;
                    }
                }
            }
        }
    }
    painted.pixels = canvas.touched - before;
    painted
}

fn scaled_radius(r: &Radius, s: f32) -> Radius {
    Radius { top_left: r.top_left * s, top_right: r.top_right * s, bottom_left: r.bottom_left * s, bottom_right: r.bottom_right * s }
}
fn scaled_width(w: &BorderWidth, s: f32) -> BorderWidth {
    let px = |v: u16| (v as f32 * s).round() as u16;
    BorderWidth { left: px(w.left), right: px(w.right), top: px(w.top), bottom: px(w.bottom), between: px(w.between) }
}
