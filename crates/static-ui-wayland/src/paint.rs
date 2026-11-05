//! Drawing a render list into a canvas, only inside damaged rectangles:
//! each damaged area is cleared and every command that reaches it is drawn
//! again in paint order, clipped to it.
use static_ui::layout::Rect;
use static_ui::{Color, Draw, RenderList, UiLayout};

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

/// Redraw `damage` (window coordinates) from `list`.
pub fn paint(
    canvas: &mut Canvas,
    list: &RenderList,
    ui: &UiLayout,
    fonts: &Fonts,
    atlas: &mut Atlas,
    clear: Color,
    damage: &[Rect],
) -> Painted {
    let before = canvas.touched;
    let mut painted = Painted::default();
    for &area in damage {
        // Whole pixels: partially covered edge pixels are redrawn whole.
        let span = canvas.span(area, area);
        let area = Rect::new(span.x0 as f32, span.y0 as f32, (span.x1 - span.x0) as f32, (span.y1 - span.y0) as f32);
        if area.is_empty() {
            continue;
        }
        canvas.clear(span, clear);
        for command in list.commands() {
            let clip = command.clip.intersect(area);
            if command.rect.intersect(clip).is_empty() {
                continue;
            }
            painted.commands += 1;
            match command.draw {
                Draw::Rect { color, radius } => canvas.fill(command.rect, color, &radius, clip),
                Draw::Border { color, width, radius } => canvas.border(command.rect, &width, color, &radius, clip),
                Draw::Overlay { color } => canvas.fill(command.rect, color, &Default::default(), clip),
                Draw::Text { line, color, config } => {
                    let text = ui.tree().text_of(command.element);
                    let text = &text[line.start as usize..(line.start + line.len) as usize];
                    let ascent = fonts.ascent(config.font, config.size);
                    let natural = fonts.line_height(config.font, config.size);
                    let baseline = (command.rect.y + (command.rect.height - natural) / 2.0 + ascent).round() as i32;
                    let mut pen = command.rect.x;
                    for ch in text.chars() {
                        let advance = fonts.advance(config.font, config.size, ch);
                        let x = pen.floor();
                        let (glyph, coverage) = atlas.glyph(fonts, config.font, config.size, ch, pen - x);
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
                        pen += advance + config.letter_spacing as f32;
                    }
                }
            }
        }
    }
    painted.pixels = canvas.touched - before;
    painted
}
