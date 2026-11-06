//! CPU raster into an XRGB8888 buffer: anti-aliased (rounded) rectangles,
//! borders and glyph coverage, every operation clipped to a rectangle.
//! Row loops over opaque spans are plain slice fills.
use static_ui::layout::Rect;
use static_ui::{BorderWidth, Color, Radius};

/// Pixels to draw into: `width` × `height`, rows `stride` pixels apart.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// Pixels written, for measuring how much a frame rastered.
    pub touched: u64,
}

/// Integer pixel bounds `[x0, x1) × [y0, y1)` covering `rect` within `clip`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}
impl Span {
    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

#[inline(always)]
fn pack(color: Color) -> u32 {
    (color.r as u32) << 16 | (color.g as u32) << 8 | color.b as u32
}
/// `dst` with `color` over it at `alpha` (0–255).
#[inline(always)]
fn blend(dst: u32, color: Color, alpha: u32) -> u32 {
    let inverse = 255 - alpha;
    let channel = |shift: u32, src: u8| {
        let d = (dst >> shift) & 0xff;
        ((src as u32 * alpha + d * inverse + 127) / 255) << shift
    };
    channel(16, color.r) | channel(8, color.g) | channel(0, color.b)
}
/// How much of pixel `[p, p + 1)` lies inside `[a, b)`.
#[inline(always)]
fn overlap(p: f32, a: f32, b: f32) -> f32 {
    ((p + 1.0).min(b) - p.max(a)).clamp(0.0, 1.0)
}

/// Coverage of pixel `(px, py)` by a rounded rectangle.
#[inline(always)]
fn rounded_coverage(rect: Rect, radius: &Radius, px: f32, py: f32) -> f32 {
    let base = overlap(px, rect.x, rect.x + rect.width) * overlap(py, rect.y, rect.y + rect.height);
    if base == 0.0 {
        return 0.0;
    }
    let (cx, cy) = (px + 0.5, py + 0.5);
    let corner = |r: f32, ox: f32, oy: f32, inside_x: bool, inside_y: bool| -> Option<f32> {
        if r <= 0.0 || !(inside_x && inside_y) {
            return None;
        }
        let d = ((cx - ox).powi(2) + (cy - oy).powi(2)).sqrt();
        Some((r - d + 0.5).clamp(0.0, 1.0))
    };
    let (left, right) = (rect.x, rect.x + rect.width);
    let (top, bottom) = (rect.y, rect.y + rect.height);
    let c = corner(radius.top_left, left + radius.top_left, top + radius.top_left, cx < left + radius.top_left, cy < top + radius.top_left)
        .or_else(|| {
            corner(radius.top_right, right - radius.top_right, top + radius.top_right, cx > right - radius.top_right, cy < top + radius.top_right)
        })
        .or_else(|| {
            corner(
                radius.bottom_left,
                left + radius.bottom_left,
                bottom - radius.bottom_left,
                cx < left + radius.bottom_left,
                cy > bottom - radius.bottom_left,
            )
        })
        .or_else(|| {
            corner(
                radius.bottom_right,
                right - radius.bottom_right,
                bottom - radius.bottom_right,
                cx > right - radius.bottom_right,
                cy > bottom - radius.bottom_right,
            )
        });
    c.map_or(base, |c| c.min(base))
}

impl Canvas<'_> {
    /// The pixels `rect` touches inside `clip` and the canvas.
    pub fn span(&self, rect: Rect, clip: Rect) -> Span {
        let r = rect.intersect(clip);
        let x0 = r.x.floor().max(0.0) as usize;
        let y0 = r.y.floor().max(0.0) as usize;
        let x1 = ((r.x + r.width).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((r.y + r.height).ceil().max(0.0) as usize).min(self.height);
        Span { x0, y0, x1: x1.max(x0), y1: y1.max(y0) }
    }
    /// Fill `span` with an opaque color.
    pub fn clear(&mut self, span: Span, color: Color) {
        let value = pack(color);
        for y in span.y0..span.y1 {
            self.pixels[y * self.stride + span.x0..y * self.stride + span.x1].fill(value);
        }
        self.touched += ((span.x1 - span.x0) * (span.y1 - span.y0)) as u64;
    }
    /// A filled rectangle with rounded corners, anti-aliased.
    pub fn fill(&mut self, rect: Rect, color: Color, radius: &Radius, clip: Rect) {
        if color.a == 0 {
            return;
        }
        let span = self.span(rect, clip);
        if span.is_empty() {
            return;
        }
        let max_radius = radius.top_left.max(radius.top_right).max(radius.bottom_left).max(radius.bottom_right);
        // Rows and columns fully inside, away from edges and corners.
        let inner_x0 = (rect.x + max_radius).ceil().max(span.x0 as f32) as usize;
        let inner_x1 = ((rect.x + rect.width - max_radius).floor().max(0.0) as usize).min(span.x1);
        let solid_top = (rect.y + max_radius).ceil();
        let solid_bottom = (rect.y + rect.height - max_radius).floor();
        let opaque = color.a == 255;
        let value = pack(color);
        for y in span.y0..span.y1 {
            let row = y * self.stride;
            let fy = y as f32;
            let interior_row = fy >= solid_top && fy + 1.0 <= solid_bottom && fy >= rect.y && fy + 1.0 <= rect.y + rect.height;
            let mut x = span.x0;
            while x < span.x1 {
                if interior_row && x >= inner_x0 && x < inner_x1 && x as f32 >= rect.x {
                    let end = inner_x1.min(span.x1);
                    let run = &mut self.pixels[row + x..row + end];
                    if opaque {
                        run.fill(value);
                    } else {
                        for p in run {
                            *p = blend(*p, color, color.a as u32);
                        }
                    }
                    x = end;
                    continue;
                }
                let coverage = rounded_coverage(rect, radius, x as f32, fy);
                if coverage > 0.0 {
                    let alpha = (coverage * color.a as f32 + 0.5) as u32;
                    let p = &mut self.pixels[row + x];
                    *p = blend(*p, color, alpha);
                }
                x += 1;
            }
        }
        self.touched += ((span.x1 - span.x0) * (span.y1 - span.y0)) as u64;
    }
    /// Borders inside `rect`: the rounded outer box minus the inner one.
    pub fn border(&mut self, rect: Rect, width: &BorderWidth, color: Color, radius: &Radius, clip: Rect) {
        if color.a == 0 {
            return;
        }
        let inner = Rect::new(
            rect.x + width.left as f32,
            rect.y + width.top as f32,
            (rect.width - (width.left + width.right) as f32).max(0.0),
            (rect.height - (width.top + width.bottom) as f32).max(0.0),
        );
        let inner_radius = Radius {
            top_left: (radius.top_left - width.left.max(width.top) as f32).max(0.0),
            top_right: (radius.top_right - width.right.max(width.top) as f32).max(0.0),
            bottom_left: (radius.bottom_left - width.left.max(width.bottom) as f32).max(0.0),
            bottom_right: (radius.bottom_right - width.right.max(width.bottom) as f32).max(0.0),
        };
        let span = self.span(rect, clip);
        let max_radius = radius.top_left.max(radius.top_right).max(radius.bottom_left).max(radius.bottom_right);
        // Inside this band of rows only the left and right edges have ink.
        let band_top = (inner.y + max_radius).ceil();
        let band_bottom = (inner.y + inner.height - max_radius).floor();
        let skip_x0 = (inner.x + max_radius).ceil().max(0.0) as usize;
        let skip_x1 = (inner.x + inner.width - max_radius).floor().max(0.0) as usize;
        for y in span.y0..span.y1 {
            let fy = y as f32;
            let banded = fy >= band_top && fy + 1.0 <= band_bottom;
            let mut x = span.x0;
            while x < span.x1 {
                if banded && x >= skip_x0 && x < skip_x1 {
                    x = skip_x1;
                    continue;
                }
                let fx = x as f32;
                let coverage = rounded_coverage(rect, radius, fx, fy) - rounded_coverage(inner, &inner_radius, fx, fy);
                if coverage > 0.0 {
                    let alpha = (coverage * color.a as f32 + 0.5) as u32;
                    let p = &mut self.pixels[y * self.stride + x];
                    *p = blend(*p, color, alpha);
                    self.touched += 1;
                }
                x += 1;
            }
        }
    }
    /// Blend glyph coverage (`width` × `height`) at pixel `(x, y)`.
    #[allow(clippy::too_many_arguments)]
    pub fn glyph(&mut self, coverage: &[u8], width: usize, height: usize, x: i32, y: i32, color: Color, clip: Rect) {
        let bounds = Rect::new(x as f32, y as f32, width as f32, height as f32);
        let span = self.span(bounds, clip);
        if span.is_empty() {
            return;
        }
        for py in span.y0..span.y1 {
            let gy = (py as i32 - y) as usize;
            let source = &coverage[gy * width..(gy + 1) * width];
            let row = py * self.stride;
            for px in span.x0..span.x1 {
                let c = source[(px as i32 - x) as usize] as u32;
                if c != 0 {
                    let alpha = (c * color.a as u32 + 127) / 255;
                    let p = &mut self.pixels[row + px];
                    *p = blend(*p, color, alpha);
                }
            }
        }
        self.touched += ((span.x1 - span.x0) * (span.y1 - span.y0)) as u64;
    }
}
