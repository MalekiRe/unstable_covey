//! A window is an entity. The app writes what it wants ([`Window`],
//! [`WindowMode`], [`Minimize`]); the crate writes what the compositor
//! reports ([`Surface`], [`WindowState`], the focus markers), as immutable
//! components so their hooks see every real change.
use static_ecs::prelude::*;

use crate::scale::Scale;

/// A window the app wants: spawn it to open one, despawn it (or remove this)
/// to close it. Changes after creation are sent to the compositor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub title: String,
    /// Matches the window to its `.desktop` file (and so its icon); empty:
    /// the executable's name.
    pub app_id: String,
    /// Logical size, used whenever the compositor leaves the size to us.
    pub width: u32,
    pub height: u32,
    pub min_size: Option<(u32, u32)>,
    pub max_size: Option<(u32, u32)>,
    /// Off: the size above is both the minimum and the maximum.
    pub resizable: bool,
    /// Off: no title bar or border from anyone.
    pub decorated: bool,
    /// On (the default), a close request (the title bar's ×) despawns the
    /// window after [`CloseRequested`](crate::CloseRequested) is emitted;
    /// off, the app decides.
    pub close_on_request: bool,
    /// On, the pointer passes through the window to whatever is below (an
    /// overlay, a window measured while idle).
    pub passthrough: bool,
}
impl Window {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            app_id: String::new(),
            width: 800,
            height: 600,
            min_size: None,
            max_size: None,
            resizable: true,
            decorated: true,
            close_on_request: true,
            passthrough: false,
        }
    }
    pub fn size(mut self, width: u32, height: u32) -> Self {
        self.width = width;
        self.height = height;
        self
    }
    pub fn app_id(mut self, app_id: impl Into<String>) -> Self {
        self.app_id = app_id.into();
        self
    }
    pub fn min_size(mut self, width: u32, height: u32) -> Self {
        self.min_size = Some((width, height));
        self
    }
    pub fn max_size(mut self, width: u32, height: u32) -> Self {
        self.max_size = Some((width, height));
        self
    }
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }
    pub fn decorated(mut self, decorated: bool) -> Self {
        self.decorated = decorated;
        self
    }
    pub fn close_on_request(mut self, close: bool) -> Self {
        self.close_on_request = close;
        self
    }
    pub fn passthrough(mut self, passthrough: bool) -> Self {
        self.passthrough = passthrough;
        self
    }
    /// The size limits to send: the fixed size when not resizable.
    pub(crate) fn limits(&self) -> ((u32, u32), (u32, u32)) {
        if self.resizable {
            (self.min_size.unwrap_or((0, 0)), self.max_size.unwrap_or((0, 0)))
        } else {
            ((self.width, self.height), (self.width, self.height))
        }
    }
}

/// How the app wants the window shown. The compositor may refuse; what it
/// granted is in [`WindowState`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

/// Insert to minimize the window once; the crate removes it after asking.
/// (Wayland never says whether a window is minimized.)
#[derive(Clone, Copy, Debug, Default)]
pub struct Minimize;

/// The window's live size, as the compositor configured it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Logical size: what layout and input coordinates use.
    pub width: u32,
    pub height: u32,
    pub scale: Scale,
}
impl Immutable for Surface {}
impl Surface {
    /// The framebuffer's size in pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (self.scale.to_pixels(self.width), self.scale.to_pixels(self.height))
    }
}

/// The window's live state, as the compositor reports it (not the last
/// request).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
    /// The window has the compositor's focus (its title bar looks active).
    pub activated: bool,
    /// Being resized interactively.
    pub resizing: bool,
    /// Hidden from view entirely (another workspace, fully covered): a
    /// renderer may stop drawing.
    pub suspended: bool,
}
impl Immutable for WindowState {}

/// On the window with keyboard focus.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardFocus;
/// On the window under the pointer.
#[derive(Clone, Copy, Debug, Default)]
pub struct PointerOver;
/// Keep drawing frames at the display's rate (a running animation); without
/// it a window draws only when something changed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Animating;
