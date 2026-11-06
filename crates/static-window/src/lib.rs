//! static-window: windows as entities, for static-ecs, on Wayland.
//!
//! Modeled on StereoKit's windowing layer, sk_app: logical sizes with exact
//! fractional scale, resizes reported only when something really changed,
//! physical keys plus separate text, client-side key repeat, display-driven
//! time steps, and title bars from the compositor or, failing that, libdecor.
//!
//! - Spawn a [`Window`] to open one; despawn it to close it. Write
//!   [`Window`]/[`WindowMode`] to change it; read [`Surface`] and
//!   [`WindowState`] for what the compositor granted.
//! - Each kind of event is its own label ([`KeyInput`], [`PointerMoved`],
//!   [`CloseRequested`], …), emitted once per event and delivered to the
//!   window entity for behaviors.
//! - [`Keys`] and [`Pointer`] hold input state.
//!
//! See `WINDOW_PLAN.md` at the workspace root for the design.

mod event;
mod input;
mod pacer;
mod repeat;
mod scale;
mod window;

pub use event::{
    CloseRequested, FocusChanged, KeyInput, PointerButton, PointerLeft, PointerMoved, Redraw, Resized, Scroll, Step, TextInput,
};
pub use input::{Key, KeyCode, Keys, Modifiers, MouseButton, Pointer};
pub use scale::Scale;
pub use window::{Animating, KeyboardFocus, Minimize, PointerOver, Surface, Window, WindowMode, WindowState};
