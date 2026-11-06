//! Fonts: TrueType outlines (ttf-parser), our rasterizer, and a glyph cache
//! keyed by glyph, size and quarter-pixel offset. Layout measures text with
//! the same advances drawing uses.
use std::collections::HashMap;

use static_ui::{Measure, Size, TextConfig};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::glyph::{Point, Rasterizer};

/// Liberation Sans Regular (SIL Open Font License 1.1, see
/// `fonts/LICENSE-LiberationSans`): font 0.
pub static LIBERATION_SANS: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");

/// Subpixel positions per pixel.
const SUBPIXELS: f32 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    font: u16,
    glyph: u16,
    size: u16,
    subpixel: u8,
}
/// A rasterized glyph: coverage in the atlas, and where it sits relative to
/// the pen (x) and baseline (y, down).
#[derive(Clone, Copy, Debug, Default)]
pub struct Glyph {
    pub width: u16,
    pub height: u16,
    pub left: i16,
    pub top: i16,
    start: u32,
}

/// Loaded fonts: a resource, and the UI's [`Measure`].
pub struct Fonts {
    faces: Vec<Face<'static>>,
}
impl Default for Fonts {
    fn default() -> Self {
        let mut fonts = Self { faces: Vec::new() };
        fonts.add(LIBERATION_SANS).expect("bundled font parses");
        fonts
    }
}
/// Every glyph drawn so far, rasterized once: the renderer's.
#[derive(Default)]
pub struct Atlas {
    glyphs: HashMap<Key, Glyph>,
    /// Glyph coverage, packed.
    pixels: Vec<u8>,
    raster: Rasterizer,
}
impl Fonts {
    /// Add a TrueType/OpenType font; returns its id for `TextConfig::font`.
    pub fn add(&mut self, data: &'static [u8]) -> Result<u16, ttf_parser::FaceParsingError> {
        self.faces.push(Face::parse(data, 0)?);
        Ok(self.faces.len() as u16 - 1)
    }
    fn face(&self, font: u16) -> &Face<'static> {
        self.faces.get(font as usize).unwrap_or(&self.faces[0])
    }
    fn scale(face: &Face, size: u16) -> f32 {
        size as f32 / face.units_per_em() as f32
    }
    /// The pen advance of `ch`, in pixels.
    pub fn advance(&self, font: u16, size: u16, ch: char) -> f32 {
        let face = self.face(font);
        let glyph = face.glyph_index(ch).unwrap_or(GlyphId(0));
        face.glyph_hor_advance(glyph).unwrap_or(0) as f32 * Self::scale(face, size)
    }
    /// Ascent above the baseline, in pixels.
    pub fn ascent(&self, font: u16, size: u16) -> f32 {
        let face = self.face(font);
        face.ascender() as f32 * Self::scale(face, size)
    }
    /// The natural line height: ascent, descent and line gap.
    pub fn line_height(&self, font: u16, size: u16) -> f32 {
        let face = self.face(font);
        (face.ascender() as f32 - face.descender() as f32 + face.line_gap() as f32) * Self::scale(face, size)
    }
}
impl Atlas {
    /// Glyphs cached so far.
    pub fn cached(&self) -> usize {
        self.glyphs.len()
    }
    /// `ch` at `size`, drawn with the pen at fractional x `fraction`:
    /// rasterized once, then served from the atlas.
    pub fn glyph(&mut self, fonts: &Fonts, font: u16, size: u16, ch: char, fraction: f32) -> (Glyph, &[u8]) {
        let font = if (font as usize) < fonts.faces.len() { font } else { 0 };
        let face = &fonts.faces[font as usize];
        let id = face.glyph_index(ch).unwrap_or(GlyphId(0));
        let subpixel = ((fraction * SUBPIXELS) as u8).min(SUBPIXELS as u8 - 1);
        let key = Key { font, glyph: id.0, size, subpixel };
        if let Some(&glyph) = self.glyphs.get(&key) {
            let start = glyph.start as usize;
            return (glyph, &self.pixels[start..start + glyph.width as usize * glyph.height as usize]);
        }
        let scale = Fonts::scale(face, size);
        let offset = subpixel as f32 / SUBPIXELS;
        let glyph = match face.glyph_bounding_box(id) {
            Some(bounds) => {
                let x0 = (bounds.x_min as f32 * scale + offset).floor();
                let x1 = (bounds.x_max as f32 * scale + offset).ceil();
                let y0 = (-bounds.y_max as f32 * scale).floor();
                let y1 = (-bounds.y_min as f32 * scale).ceil();
                let (width, height) = ((x1 - x0) as usize + 2, (y1 - y0) as usize + 1);
                self.raster.reset(width, height);
                let mut outline = Outline {
                    raster: &mut self.raster,
                    scale,
                    dx: offset - x0,
                    dy: -y0,
                    start: Point::new(0.0, 0.0),
                    last: Point::new(0.0, 0.0),
                };
                face.outline_glyph(id, &mut outline);
                let start = self.pixels.len() as u32;
                self.raster.coverage(&mut self.pixels);
                Glyph { width: width as u16, height: height as u16, left: x0 as i16, top: y0 as i16, start }
            }
            None => Glyph::default(), // a space
        };
        self.glyphs.insert(key, glyph);
        let start = glyph.start as usize;
        (glyph, &self.pixels[start..start + glyph.width as usize * glyph.height as usize])
    }
}
impl Measure for Fonts {
    fn measure(&mut self, text: &str, config: &TextConfig) -> Size {
        let spacing = config.letter_spacing as f32;
        let width = text.chars().map(|ch| self.advance(config.font, config.size, ch) + spacing).sum();
        Size::new(width, self.line_height(config.font, config.size))
    }
}

/// Feeds an outline to the rasterizer, in pixel space (y down).
struct Outline<'a> {
    raster: &'a mut Rasterizer,
    scale: f32,
    dx: f32,
    dy: f32,
    start: Point,
    last: Point,
}
impl Outline<'_> {
    fn point(&self, x: f32, y: f32) -> Point {
        Point::new(x * self.scale + self.dx, -y * self.scale + self.dy)
    }
}
impl OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.start = self.point(x, y);
        self.last = self.start;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.point(x, y);
        self.raster.line(self.last, p);
        self.last = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.point(x1, y1), self.point(x, y));
        self.raster.quad(self.last, c, p);
        self.last = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.point(x1, y1), self.point(x2, y2), self.point(x, y));
        self.raster.cubic(self.last, c1, c2, p);
        self.last = p;
    }
    fn close(&mut self) {
        if self.last != self.start {
            self.raster.line(self.last, self.start);
        }
        self.last = self.start;
    }
}
