//! Software rendering: two shared-memory buffers per window, each tracking
//! what changed since it was last drawn, so a frame repaints only the
//! damaged rectangles of whichever buffer the compositor has given back.
//!
//! Put a [`Framebuffer`] on a window to draw into it. During `Step`, call
//! [`Framebuffer::request`] for what changed; the loop then emits
//! [`Redraw`](crate::Redraw) for the window, where [`Framebuffer::target`]
//! hands out the back buffer and the regions to repaint.
use std::os::fd::{AsFd, OwnedFd};

use wayland_client::QueueHandle;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::{self, WlShm};

use crate::scale::Scale;
use crate::wayland::State;

/// A rectangle in buffer pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl PixelRect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    fn right(&self) -> u32 {
        self.x + self.width
    }
    fn bottom(&self) -> u32 {
        self.y + self.height
    }
    fn overlaps_or_touches(&self, other: &Self) -> bool {
        self.x <= other.right() && other.x <= self.right() && self.y <= other.bottom() && other.y <= self.bottom()
    }
    fn union(&self, other: &Self) -> Self {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Self { x, y, width: self.right().max(other.right()) - x, height: self.bottom().max(other.bottom()) - y }
    }
    /// Clipped to a `width` × `height` buffer.
    fn clip(&self, width: u32, height: u32) -> Self {
        let (x, y) = (self.x.min(width), self.y.min(height));
        Self { x, y, width: self.right().min(width) - x, height: self.bottom().min(height) - y }
    }
}

/// Merge overlapping or touching rectangles in place until none do.
pub(crate) fn merge(rects: &mut Vec<PixelRect>) {
    rects.retain(|r| !r.is_empty());
    let mut i = 0;
    while i < rects.len() {
        let mut j = i + 1;
        let mut grew = false;
        while j < rects.len() {
            if rects[i].overlaps_or_touches(&rects[j]) {
                rects[i] = rects[i].union(&rects[j]);
                rects.swap_remove(j);
                grew = true;
            } else {
                j += 1;
            }
        }
        // A grown rectangle may now reach ones it skipped.
        if !grew {
            i += 1;
        }
    }
}

/// Where to draw this frame.
pub struct Target<'a> {
    /// `width * height` pixels, row-major, `0xAARRGGBB` (alpha ignored).
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    /// Pixels per logical unit.
    pub scale: Scale,
    /// What to repaint: everything that changed since this buffer was last
    /// drawn. Pixels outside are already right.
    pub regions: &'a [PixelRect],
}

/// A window's software framebuffer. Insert it with the window (or before
/// its first configure) to draw every frame yourself.
#[derive(Default)]
pub struct Framebuffer {
    /// Damage requested since the last frame.
    requested: Vec<PixelRect>,
    request_all: bool,
    pub(crate) shm: Option<Shm>,
    /// The buffer being drawn during `Redraw`, with its regions.
    pub(crate) back: Option<usize>,
    pub(crate) regions: Vec<PixelRect>,
    /// Something changed since the last frame shown (or the buffers are
    /// new): a frame is due. Each buffer's pending list says what to repaint
    /// when it is the one drawn.
    pub(crate) wanted: bool,
    /// Draw at this scale whatever the display's, and let the compositor
    /// stretch the result.
    pub(crate) fixed_scale: Option<Scale>,
}

impl Framebuffer {
    /// A framebuffer drawn at `scale` (`Scale::ONE`: one pixel per logical
    /// unit) whatever the display's: for content with a fixed resolution,
    /// such as a game's, which the compositor then scales.
    pub fn at_scale(scale: Scale) -> Self {
        Self { fixed_scale: Some(scale), ..Self::default() }
    }
    /// Repaint `rect` (buffer pixels) at the next frame.
    pub fn request(&mut self, rect: PixelRect) {
        if !rect.is_empty() {
            self.requested.push(rect);
        }
    }
    /// Repaint everything at the next frame.
    pub fn request_all(&mut self) {
        self.request_all = true;
    }
    /// The buffers' size in pixels, once allocated.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.shm.as_ref().map(|s| (s.width, s.height))
    }
    /// The buffers' scale, once allocated.
    pub fn scale(&self) -> Scale {
        self.shm.as_ref().map_or(Scale::ONE, |s| s.scale)
    }
    /// The back buffer and what to repaint: `Some` only while handling
    /// `Redraw` for this window.
    pub fn target(&mut self) -> Option<Target<'_>> {
        let index = self.back?;
        let shm = self.shm.as_mut()?;
        let (width, height, scale) = (shm.width, shm.height, shm.scale);
        Some(Target { pixels: shm.frame(index), width, height, scale, regions: &self.regions })
    }

    /// Fold requested damage into both buffers' pending lists.
    pub(crate) fn take_requests(&mut self) {
        let Some(shm) = &mut self.shm else { return };
        let all = std::mem::take(&mut self.request_all);
        if !all && self.requested.is_empty() {
            return;
        }
        self.wanted = true;
        for pending in &mut shm.pending {
            if all {
                pending.clear();
                pending.push(PixelRect::new(0, 0, shm.width, shm.height));
            } else {
                pending.extend(self.requested.iter().map(|r| r.clip(shm.width, shm.height)));
            }
        }
        self.requested.clear();
    }
    /// If a frame is due and a buffer is free, stage that buffer and what it
    /// needs repainted as the back buffer.
    pub(crate) fn begin(&mut self) -> Option<usize> {
        if !self.wanted {
            return None;
        }
        let shm = self.shm.as_mut()?;
        let index = (0..2).find(|&i| !shm.busy[i])?;
        merge(&mut shm.pending[index]);
        self.regions.clear();
        self.regions.append(&mut shm.pending[index]);
        self.back = Some(index);
        self.wanted = false;
        Some(index)
    }
}

/// Two `XRGB8888` buffers in one memfd mapping.
pub(crate) struct Shm {
    pixels: *mut u32,
    bytes: usize,
    pub width: u32,
    pub height: u32,
    pub scale: Scale,
    pub buffers: [WlBuffer; 2],
    pub busy: [bool; 2],
    pub pending: [Vec<PixelRect>; 2],
    /// Tells this allocation's buffer releases from an older one's.
    pub generation: u32,
    _fd: OwnedFd,
}
// SAFETY: `pixels` is this value's own mapping, reached only through
// `&mut self`.
unsafe impl Send for Shm {}
unsafe impl Sync for Shm {}

/// What a buffer's release names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BufferId {
    pub window: static_ecs::Entity,
    pub generation: u32,
    pub index: u8,
}

impl Shm {
    pub fn new(shm: &WlShm, qh: &QueueHandle<State>, window: static_ecs::Entity, generation: u32, width: u32, height: u32, scale: Scale) -> Self {
        let frame = width as usize * height as usize * 4;
        let bytes = frame * 2;
        let fd = rustix::fs::memfd_create("static-window", rustix::fs::MemfdFlags::CLOEXEC).expect("memfd_create");
        rustix::fs::ftruncate(&fd, bytes as u64).expect("ftruncate");
        // SAFETY: a fresh shared mapping of the whole memfd, unmapped on drop.
        let pixels = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                bytes,
                rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                rustix::mm::MapFlags::SHARED,
                &fd,
                0,
            )
        }
        .expect("mmap") as *mut u32;
        let pool = shm.create_pool(fd.as_fd(), bytes as i32, qh, ());
        let buffer = |index: u8| {
            let id = BufferId { window, generation, index };
            pool.create_buffer(index as i32 * frame as i32, width as i32, height as i32, width as i32 * 4, wl_shm::Format::Xrgb8888, qh, id)
        };
        let buffers = [buffer(0), buffer(1)];
        pool.destroy();
        let whole = PixelRect::new(0, 0, width, height);
        Self { pixels, bytes, width, height, scale, buffers, busy: [false; 2], pending: [vec![whole], vec![whole]], generation, _fd: fd }
    }
    fn frame(&mut self, index: usize) -> &mut [u32] {
        let len = self.width as usize * self.height as usize;
        // SAFETY: frame `index` lies inside the mapping, and `&mut self`
        // makes this the only view of it.
        unsafe { std::slice::from_raw_parts_mut(self.pixels.add(index * len), len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        // SAFETY: the mapping made in `new`.
        unsafe {
            let _ = rustix::mm::munmap(self.pixels.cast(), self.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_joins_overlapping_and_touching_rects_only() {
        let mut rects = vec![
            PixelRect::new(0, 0, 10, 10),
            PixelRect::new(50, 50, 5, 5),
            PixelRect::new(10, 0, 10, 10),
            PixelRect::new(5, 5, 0, 9),
            // Reached only once the first two have joined.
            PixelRect::new(19, 9, 2, 2),
        ];
        merge(&mut rects);
        rects.sort_by_key(|r| r.x);
        assert_eq!(rects, [PixelRect::new(0, 0, 21, 11), PixelRect::new(50, 50, 5, 5)]);
    }

    #[test]
    fn clipping_keeps_rects_inside_the_buffer() {
        assert_eq!(PixelRect::new(90, 90, 20, 20).clip(100, 100), PixelRect::new(90, 90, 10, 10));
        assert!(PixelRect::new(120, 0, 5, 5).clip(100, 100).is_empty());
    }
}
