//! Glyph outlines to anti-aliased coverage: signed-area accumulation (the
//! font-rs algorithm). Each line segment adds its exact area contribution
//! to the cells it crosses; a running sum along the buffer turns those into
//! coverage. Curves are flattened into lines first.

/// Accumulates line segments into a `width` × `height` coverage buffer.
#[derive(Default)]
pub struct Rasterizer {
    width: usize,
    height: usize,
    accumulation: Vec<f32>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    fn lerp(self, t: f32, other: Point) -> Point {
        Point::new(self.x + t * (other.x - self.x), self.y + t * (other.y - self.y))
    }
}

impl Rasterizer {
    /// Start a glyph of `width` × `height` pixels, reusing the buffer.
    pub fn reset(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.accumulation.clear();
        // Spill-over room: segments at the right edge write one cell past
        // the row, which the running sum carries into the next row's start.
        self.accumulation.resize(width * height + 4, 0.0);
    }
    /// Add a straight edge from `p0` to `p1` (pixel coordinates, y down).
    pub fn line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y {
            return;
        }
        let (direction, p0, p1) = if p0.y < p1.y { (1.0, p0, p1) } else { (-1.0, p1, p0) };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        let mut x = p0.x;
        let y0 = p0.y.max(0.0) as usize;
        if p0.y < 0.0 {
            x -= p0.y * dxdy;
        }
        let end = (p1.y.ceil().max(0.0) as usize).min(self.height);
        for y in y0..end {
            let row = y * self.width;
            let dy = (y as f32 + 1.0).min(p1.y) - (y as f32).max(p0.y);
            let x_next = x + dxdy * dy;
            let d = dy * direction;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let x0_floor = x0.floor();
            let x0i = x0_floor as isize;
            let x1_ceil = x1.ceil();
            let x1i = x1_ceil as isize;
            let last = self.accumulation.len() as isize - 1;
            let cell = |i: isize| (row as isize + i).clamp(0, last) as usize;
            if x1i <= x0i + 1 {
                let x_mid = 0.5 * (x + x_next) - x0_floor;
                self.accumulation[cell(x0i)] += d - d * x_mid;
                self.accumulation[cell(x0i + 1)] += d * x_mid;
            } else {
                let s = (x1 - x0).recip();
                let x0f = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.accumulation[cell(x0i)] += d * a0;
                if x1i == x0i + 2 {
                    self.accumulation[cell(x0i + 1)] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.accumulation[cell(x0i + 1)] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.accumulation[cell(xi)] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.accumulation[cell(x1i - 1)] += d * (1.0 - a2 - am);
                }
                self.accumulation[cell(x1i)] += d * am;
            }
            x = x_next;
        }
    }
    /// A quadratic Bézier, flattened into lines.
    pub fn quad(&mut self, p0: Point, p1: Point, p2: Point) {
        let dev_x = p0.x - 2.0 * p1.x + p2.x;
        let dev_y = p0.y - 2.0 * p1.y + p2.y;
        let dev_squared = dev_x * dev_x + dev_y * dev_y;
        if dev_squared < 0.333 {
            self.line(p0, p2);
            return;
        }
        let tolerance = 3.0;
        let n = 1 + (tolerance * dev_squared).sqrt().sqrt().floor() as usize;
        let mut previous = p0;
        let step = 1.0 / n as f32;
        for i in 1..n {
            let t = i as f32 * step;
            let next = p0.lerp(t, p1).lerp(t, p1.lerp(t, p2));
            self.line(previous, next);
            previous = next;
        }
        self.line(previous, p2);
    }
    /// A cubic Bézier, flattened into lines.
    pub fn cubic(&mut self, p0: Point, p1: Point, p2: Point, p3: Point) {
        let dev = |a: Point, b: Point, c: Point| {
            let x = a.x - 2.0 * b.x + c.x;
            let y = a.y - 2.0 * b.y + c.y;
            x * x + y * y
        };
        let dev_squared = dev(p0, p1, p2).max(dev(p1, p2, p3));
        let n = 1 + (3.0 * dev_squared).sqrt().sqrt().floor() as usize * 2;
        let mut previous = p0;
        let step = 1.0 / n as f32;
        for i in 1..n {
            let t = i as f32 * step;
            let a = p0.lerp(t, p1);
            let b = p1.lerp(t, p2);
            let c = p2.lerp(t, p3);
            let next = a.lerp(t, b).lerp(t, b.lerp(t, c));
            self.line(previous, next);
            previous = next;
        }
        self.line(previous, p3);
    }
    /// Coverage 0–255 per pixel, row-major, appended to `out`.
    pub fn coverage(&self, out: &mut Vec<u8>) {
        let mut sum = 0.0f32;
        out.extend(self.accumulation[..self.width * self.height].iter().map(|&a| {
            sum += a;
            (sum.abs().min(1.0) * 255.0 + 0.5) as u8
        }));
    }
}
