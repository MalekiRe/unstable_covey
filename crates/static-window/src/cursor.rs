//! The pointer's look over a window, and capturing it. As in sk_app, the
//! compositor draws the cursor when it offers `wp_cursor_shape_v1`; an
//! XCursor theme (`XCURSOR_THEME`, `XCURSOR_SIZE`) is the fallback.
use std::os::fd::AsFd;

use wayland_client::QueueHandle;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

use crate::wayland::State;

/// The cursor while the pointer is over a window: a component (absent:
/// `Default`). Names follow CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CursorIcon {
    #[default]
    Default,
    /// Over editable text.
    Text,
    /// Over a link or button.
    Pointer,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    Progress,
    Help,
    /// Resize left–right, up–down, and along the diagonals.
    EwResize,
    NsResize,
    NwseResize,
    NeswResize,
    ColResize,
    RowResize,
    /// No cursor at all.
    Hidden,
}

impl CursorIcon {
    pub(crate) fn shape(self) -> Option<Shape> {
        Some(match self {
            Self::Default => Shape::Default,
            Self::Text => Shape::Text,
            Self::Pointer => Shape::Pointer,
            Self::Crosshair => Shape::Crosshair,
            Self::Move => Shape::Move,
            Self::Grab => Shape::Grab,
            Self::Grabbing => Shape::Grabbing,
            Self::NotAllowed => Shape::NotAllowed,
            Self::Wait => Shape::Wait,
            Self::Progress => Shape::Progress,
            Self::Help => Shape::Help,
            Self::EwResize => Shape::EwResize,
            Self::NsResize => Shape::NsResize,
            Self::NwseResize => Shape::NwseResize,
            Self::NeswResize => Shape::NeswResize,
            Self::ColResize => Shape::ColResize,
            Self::RowResize => Shape::RowResize,
            Self::Hidden => return None,
        })
    }
    /// Theme file names to try, the CSS name first, then the older X11 ones.
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::Default => &["default", "left_ptr"],
            Self::Text => &["text", "xterm", "ibeam"],
            Self::Pointer => &["pointer", "hand2", "hand1"],
            Self::Crosshair => &["crosshair", "cross"],
            Self::Move => &["move", "fleur", "all-scroll"],
            Self::Grab => &["grab", "openhand", "hand1"],
            Self::Grabbing => &["grabbing", "closedhand", "fleur"],
            Self::NotAllowed => &["not-allowed", "crossed_circle"],
            Self::Wait => &["wait", "watch"],
            Self::Progress => &["progress", "left_ptr_watch"],
            Self::Help => &["help", "question_arrow"],
            Self::EwResize => &["ew-resize", "sb_h_double_arrow", "h_double_arrow"],
            Self::NsResize => &["ns-resize", "sb_v_double_arrow", "v_double_arrow"],
            Self::NwseResize => &["nwse-resize", "bd_double_arrow", "size_fdiag"],
            Self::NeswResize => &["nesw-resize", "fd_double_arrow", "size_bdiag"],
            Self::ColResize => &["col-resize", "sb_h_double_arrow"],
            Self::RowResize => &["row-resize", "sb_v_double_arrow"],
            Self::Hidden => &[],
        }
    }
}

/// Lock the pointer in place while it is over this window, and report its
/// raw motion as [`PointerDelta`](crate::PointerDelta) (a first-person
/// camera, a drag that must not stop at the screen's edge). The cursor is
/// hidden meanwhile. Remove it to let go.
#[derive(Clone, Copy, Debug, Default)]
pub struct PointerLock;

/// A loaded theme image, as a buffer the compositor can show.
struct Image {
    icon: CursorIcon,
    scale: u32,
    buffer: WlBuffer,
    hotspot: (i32, i32),
}

/// Cursor images from the XCursor theme, loaded on first use.
pub(crate) struct Theme {
    theme: xcursor::CursorTheme,
    size: u32,
    pub surface: WlSurface,
    images: Vec<Image>,
}

impl Theme {
    pub fn new(compositor: &WlCompositor, qh: &QueueHandle<State>) -> Self {
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size = std::env::var("XCURSOR_SIZE").ok().and_then(|s| s.parse().ok()).filter(|&s| s > 0).unwrap_or(24);
        Self { theme: xcursor::CursorTheme::load(&name), size, surface: compositor.create_surface(qh, ()), images: Vec::new() }
    }
    /// Attach `icon` at `scale` to the cursor surface; its hotspot, in
    /// surface units, or `None` if the theme lacks it.
    pub fn show(&mut self, icon: CursorIcon, scale: u32, shm: &WlShm, qh: &QueueHandle<State>) -> Option<(i32, i32)> {
        if !self.images.iter().any(|i| i.icon == icon && i.scale == scale) {
            let image = self.load(icon, scale, shm, qh)?;
            self.images.push(image);
        }
        let image = self.images.iter().find(|i| i.icon == icon && i.scale == scale)?;
        self.surface.set_buffer_scale(image.scale as i32);
        self.surface.attach(Some(&image.buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.surface.commit();
        Some(image.hotspot)
    }
    fn load(&self, icon: CursorIcon, scale: u32, shm: &WlShm, qh: &QueueHandle<State>) -> Option<Image> {
        let path = icon.names().iter().find_map(|name| self.theme.load_icon(name))?;
        let images = xcursor::parser::parse_xcursor(&std::fs::read(path).ok()?)?;
        let want = self.size * scale;
        // The nominal size nearest the one wanted; an animated cursor's
        // first frame.
        let image = images.iter().min_by_key(|i| i.size.abs_diff(want))?;
        // Buffer scale must divide the size; otherwise show it unscaled.
        let scale = if image.width % scale == 0 && image.height % scale == 0 { scale } else { 1 };
        let bytes = image.pixels_argb.len();
        let fd = rustix::fs::memfd_create("static-window-cursor", rustix::fs::MemfdFlags::CLOEXEC).ok()?;
        rustix::io::write(&fd, &image.pixels_argb).ok()?;
        let pool = shm.create_pool(fd.as_fd(), bytes as i32, qh, ());
        let buffer = pool.create_buffer(0, image.width as i32, image.height as i32, image.width as i32 * 4, wl_shm::Format::Argb8888, qh, ());
        pool.destroy();
        Some(Image { icon, scale, buffer, hotspot: ((image.xhot / scale) as i32, (image.yhot / scale) as i32) })
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        for image in &self.images {
            image.buffer.destroy();
        }
        self.surface.destroy();
    }
}
