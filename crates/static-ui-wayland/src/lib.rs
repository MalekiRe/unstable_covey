//! static-ui on Wayland: render commands rastered on the CPU into shared
//! memory, only where something changed, with frames requested only while
//! something is changing. Fonts are TrueType outlines through our own
//! rasterizer, cached in a glyph atlas.
pub mod canvas;
pub mod font;
pub mod glyph;
pub mod paint;
mod window;

pub use canvas::Canvas;
pub use font::{Atlas, Fonts};
pub use paint::{Painted, paint};
pub use window::{Stats, WindowOptions, run};
