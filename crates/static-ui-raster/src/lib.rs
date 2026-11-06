//! static-ui, rastered: render commands drawn on the CPU into a
//! [`static_window`] framebuffer, only where something changed, at the
//! window's scale. Fonts are TrueType outlines through our own rasterizer,
//! cached in a glyph atlas.
pub mod canvas;
pub mod font;
pub mod glyph;
pub mod paint;
mod ui;

pub use canvas::Canvas;
pub use font::{Atlas, Fonts};
pub use paint::{Painted, paint};
pub use ui::{RasterPlugin, UiLabels, run, ui_frame, ui_window};
