//! The clipboard (the selection), as text. Setting it offers our text until
//! another client takes the selection; reading it asks the owner for the
//! text through a pipe the loop drains, then emits [`ClipboardText`].
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use static_ecs::prelude::*;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::{Connection, Dispatch, QueueHandle, event_created_child};

use crate::wayland::State;

/// Text types, best first. Toolkits still offer the X11 names.
const MIMES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"];

/// The clipboard: a resource. [`set`](Self::set) takes effect at the end of
/// the step; [`request`](Self::request) answers with [`ClipboardText`].
#[derive(Debug, Default)]
pub struct Clipboard {
    pub(crate) set: Option<String>,
    pub(crate) requested: bool,
}
impl Clipboard {
    /// Put `text` on the clipboard.
    pub fn set(&mut self, text: impl Into<String>) {
        self.set = Some(text.into());
    }
    /// Ask for the clipboard's text.
    pub fn request(&mut self) {
        self.requested = true;
    }
}

/// The clipboard's text, answering [`Clipboard::request`]; empty if the
/// clipboard holds no text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardText {
    pub text: String,
}
impl Schedule for ClipboardText {
    type In = Ref<Self>;
}

/// Selection state on the protocol side.
#[derive(Default)]
pub(crate) struct Selection {
    pub device: Option<WlDataDevice>,
    /// What the current selection offers, if it is text.
    offer: Option<(WlDataOffer, &'static str)>,
    /// An offer announced but not yet made the selection.
    pending: Option<(WlDataOffer, Option<&'static str>)>,
    /// Our own selection, while we hold it.
    pub source: Option<(WlDataSource, Arc<str>)>,
    /// A read in progress: the pipe and what arrived so far.
    pub reading: Option<(OwnedFd, Vec<u8>)>,
    /// Text ready to emit.
    pub received: Option<String>,
}

impl Selection {
    /// Offer `text` as the selection, quoting the input event `serial`.
    pub fn set(&mut self, manager: &WlDataDeviceManager, qh: &QueueHandle<State>, text: String, serial: u32) {
        let Some(device) = &self.device else { return };
        let text: Arc<str> = text.into();
        let source = manager.create_data_source(qh, text.clone());
        for mime in MIMES {
            source.offer(mime.into());
        }
        device.set_selection(Some(&source), serial);
        if let Some((old, _)) = self.source.replace((source, text)) {
            old.destroy();
        }
    }
    /// Start reading the selection's text. Our own is answered at once.
    pub fn request(&mut self, conn: &Connection) {
        if let Some((_, text)) = &self.source {
            self.received = Some(text.to_string());
            return;
        }
        let Some((offer, mime)) = &self.offer else {
            self.received = Some(String::new());
            return;
        };
        let Ok((read, write)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK) else {
            self.received = Some(String::new());
            return;
        };
        offer.receive((*mime).into(), write.as_fd());
        // The owner writes once our request reaches it; our end of the
        // write side closes now, so EOF means it is done.
        let _ = conn.flush();
        drop(write);
        self.reading = Some((read, Vec::new()));
    }
    /// Read what the pipe has; on EOF the text is ready.
    pub fn drain(&mut self) {
        let Some((pipe, bytes)) = &mut self.reading else { return };
        let mut chunk = [0u8; 4096];
        loop {
            match rustix::io::read(&*pipe, &mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(rustix::io::Errno::AGAIN) => return,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => break,
            }
        }
        let (_, bytes) = self.reading.take().unwrap();
        self.received = Some(String::from_utf8_lossy(&bytes).into_owned());
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: wayland_client::protocol::wl_data_device_manager::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let selection = &mut state.selection;
        match event {
            wl_data_device::Event::DataOffer { id } => {
                if let Some((old, _)) = selection.pending.replace((id, None)) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Selection { id } => {
                if let Some((old, _)) = selection.offer.take() {
                    old.destroy();
                }
                let pending = selection.pending.take();
                match (id, pending) {
                    (Some(id), Some((offer, Some(mime)))) if id == offer => selection.offer = Some((offer, mime)),
                    (_, Some((offer, _))) => offer.destroy(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(state: &mut Self, offer: &WlDataOffer, event: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let wl_data_offer::Event::Offer { mime_type } = event else { return };
        if let Some((pending, best)) = &mut state.selection.pending
            && pending == offer
            && let Some(rank) = MIMES.iter().position(|&m| m == mime_type)
            && best.is_none_or(|b| MIMES.iter().position(|&m| m == b).is_some_and(|r| rank < r))
        {
            *best = Some(MIMES[rank]);
        }
    }
}

impl Dispatch<WlDataSource, Arc<str>> for State {
    fn event(state: &mut Self, source: &WlDataSource, event: wl_data_source::Event, text: &Arc<str>, _: &Connection, _: &QueueHandle<Self>) {
        match event {
            // Another client pastes: write our text to its pipe. A thread,
            // so a large selection never blocks the loop on a slow reader.
            wl_data_source::Event::Send { fd, .. } => {
                let text = text.clone();
                std::thread::spawn(move || {
                    let _ = rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::empty());
                    let mut rest = text.as_bytes();
                    while !rest.is_empty() {
                        match rustix::io::write(&fd, rest) {
                            Ok(n) => rest = &rest[n..],
                            Err(rustix::io::Errno::INTR) => {}
                            Err(_) => break,
                        }
                    }
                });
            }
            // Someone else took the selection.
            wl_data_source::Event::Cancelled => {
                if state.selection.source.as_ref().is_some_and(|(s, _)| s == source) {
                    state.selection.source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}
