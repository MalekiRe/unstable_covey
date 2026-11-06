//! static-window's `examples/software.rs` on winit 0.30 and softbuffer 0.4: a
//! gradient with a square sweeping across it, redrawn every frame.
//! `winit-softbuffer-bench 3` closes after three seconds.
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

struct App {
    seconds: Option<f64>,
    start: Instant,
    last: Instant,
    x: f32,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes().with_title("software (winit)").with_inner_size(LogicalSize::new(480, 200));
        let window = Rc::new(event_loop.create_window(attributes).expect("window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface"));
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if self.seconds.is_some_and(|s| self.start.elapsed().as_secs_f64() >= s) {
                    event_loop.exit();
                    return;
                }
                let size = window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };
                surface.resize(w, h).expect("resize");
                let scale = window.scale_factor() as f32;
                let now = Instant::now();
                let dt = now.duration_since(self.last).as_secs_f32();
                self.last = now;
                let logical_width = size.width as f32 / scale;
                self.x = (self.x + 240.0 * dt) % (logical_width - 40.0).max(1.0);
                let (sx, sy, ss) = ((self.x * scale) as u32, (60.0 * scale) as u32, (40.0 * scale) as u32);
                let mut buffer = surface.buffer_mut().expect("buffer");
                let (w, h) = (size.width, size.height);
                for y in 0..h {
                    for x in 0..w {
                        let inside = (sx..sx + ss).contains(&x) && (sy..sy + ss).contains(&y);
                        let shade = (x * 255 / w) << 8 | (y * 255 / h);
                        buffer[(y * w + x) as usize] = if inside { 0xFFFF_FFFF } else { 0xFF00_0000 | shade };
                    }
                }
                window.pre_present_notify();
                buffer.present().expect("present");
                window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let seconds = std::env::args().nth(1).and_then(|s| s.parse().ok());
    let event_loop = EventLoop::new().expect("event loop");
    let now = Instant::now();
    let mut app = App { seconds, start: now, last: now, x: 0.0, window: None, surface: None };
    event_loop.run_app(&mut app).expect("run");
}
