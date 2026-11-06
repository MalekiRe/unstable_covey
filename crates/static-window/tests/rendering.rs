//! Software rendering against the scripted compositor: the first frame drawn
//! during the configure roundtrip, damage-only repaints across two buffers,
//! waiting for a buffer, animation frames, and scale changes.
mod support;

use std::time::Duration;

use static_ecs::prelude::*;
use static_window::*;
use support::compositor::{Compositor, Damage, Globals};

/// The color the test renderer paints, and the damage `Step` requests.
#[derive(Default)]
struct Paint {
    color: u32,
    request: Vec<PixelRect>,
    /// Regions each `Redraw` repainted, with the buffer size.
    redraws: Vec<((u32, u32), Vec<PixelRect>)>,
}

macro_rules! test_app {
    () => {
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Paint>()
            .add_systems(Step, |mut paint: ResMut<Paint>, mut fbs: Query<&mut Framebuffer>| {
                let request = std::mem::take(&mut paint.request);
                fbs.each(|mut fb| request.iter().for_each(|&r| fb.request(r)));
            })
            .add_systems(label::<Redraw>(), |redraw: &Redraw, mut paint: ResMut<Paint>, mut fbs: Query<&mut Framebuffer>| {
                let mut fb = fbs.get_mut(redraw.window).unwrap();
                let target = fb.target().expect("a back buffer during Redraw");
                for r in target.regions {
                    for y in r.y..r.y + r.height {
                        let row = (y * target.width) as usize;
                        target.pixels[row + r.x as usize..row + (r.x + r.width) as usize].fill(paint.color);
                    }
                }
                let entry = ((target.width, target.height), target.regions.to_vec());
                paint.redraws.push(entry);
            })
            .build()
    };
}

const NOW: Option<Duration> = Some(Duration::ZERO);
const RED: u32 = 0xFFFF_0000;
const GREEN: u32 = 0xFF00_FF00;
const BLUE: u32 = 0xFF00_00FF;

fn whole(w: u32, h: u32) -> Vec<PixelRect> {
    vec![PixelRect::new(0, 0, w, h)]
}

fn paint<S: Storage + Stores<Paint>>(world: &mut World<S>) -> Mut<'_, Paint> {
    world.resource_mut::<Paint>().unwrap()
}

/// Open a 64×32 framebuffer window painted red and map it.
macro_rules! mapped {
    ($globals:expr) => {{
        let (compositor, socket) = Compositor::spawn($globals);
        let mut lp = WindowLoop::from_socket(socket).unwrap();
        let mut app = test_app!();
        paint(&mut app.world).color = RED;
        let window = app.world.spawn_bundle((Window::new("fb").size(64, 32), Framebuffer::default()));
        lp.step(&mut app, NOW);
        compositor.with(|s, _| s.configure(0, 0, 0, &[]));
        lp.step(&mut app, NOW);
        (compositor, lp, app, window)
    }};
}

#[test]
fn the_first_frame_is_drawn_during_the_configure_roundtrip() {
    let (compositor, socket) = Compositor::spawn(Globals::default());
    let mut lp = WindowLoop::from_socket(socket).unwrap();
    let mut app = test_app!();
    paint(&mut app.world).color = RED;
    app.world.spawn_bundle((Window::new("fb").size(64, 32), Framebuffer::default()));
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws, [((64, 32), whole(64, 32))], "drawn before any configure");
    compositor.with(|s, _| {
        let commits = &s.surface_of(0).commits;
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].buffer, None, "but not attached before it");
        s.configure(0, 0, 0, &[]);
    });
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws.len(), 1, "the size held: no second draw");
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        let commit = surface.commits.last().unwrap();
        let buffer = commit.buffer.expect("the predrawn frame");
        assert_eq!((s.buffers[buffer].width, s.buffers[buffer].height), (64, 32), "no placeholder");
        assert_eq!(commit.damage, [Damage { x: 0, y: 0, width: 64, height: 32 }]);
        assert!(s.pixels(buffer).iter().all(|&p| p == RED));
        assert_eq!(surface.viewport_destination, Some((64, 32)));
    });
}

#[test]
fn a_different_configured_size_is_drawn_again_before_mapping() {
    let (compositor, socket) = Compositor::spawn(Globals::default());
    let mut lp = WindowLoop::from_socket(socket).unwrap();
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("fb").size(64, 32), Framebuffer::default()));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 100, 50, &[]));
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws.last().unwrap(), &((100, 50), whole(100, 50)));
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        let buffers: Vec<_> = surface.commits.iter().filter_map(|c| c.buffer).collect();
        assert_eq!(buffers.len(), 1, "one mapping commit");
        assert_eq!((s.buffers[buffers[0]].width, s.buffers[buffers[0]].height), (100, 50));
    });
}

#[test]
fn each_buffer_repaints_only_what_changed_since_it_was_drawn() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    let first = compositor.with(|s, _| s.surface_of(0).buffer.unwrap());
    // The second buffer has never been drawn: all of it.
    let r1 = PixelRect::new(2, 2, 4, 4);
    paint(&mut app.world).color = GREEN;
    paint(&mut app.world).request.push(r1);
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws.last().unwrap(), &((64, 32), whole(64, 32)));
    // The first buffer comes back: it lacks r1 and the new r2.
    let r2 = PixelRect::new(40, 20, 8, 4);
    compositor.with(move |s, _| s.release(first));
    paint(&mut app.world).color = BLUE;
    paint(&mut app.world).request.push(r2);
    lp.step(&mut app, NOW);
    let mut regions = paint(&mut app.world).redraws.last().unwrap().1.clone();
    regions.sort_by_key(|r| r.x);
    assert_eq!(regions, [r1, r2]);
    compositor.with(move |s, _| {
        let commit = s.surface_of(0).commits.last().unwrap().clone();
        assert_eq!(commit.buffer, Some(first));
        let mut damage = commit.damage.clone();
        damage.sort_by_key(|d| d.x);
        assert_eq!(damage, [Damage { x: 2, y: 2, width: 4, height: 4 }, Damage { x: 40, y: 20, width: 8, height: 4 }]);
        let pixels = s.pixels(first);
        assert_eq!(pixels[2 * 64 + 2], BLUE);
        assert_eq!(pixels[20 * 64 + 40], BLUE);
        assert_eq!(pixels[0], RED, "outside the damage: untouched");
    });
}

#[test]
fn with_both_buffers_busy_the_frame_waits_for_a_release() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    paint(&mut app.world).request.push(PixelRect::new(0, 0, 1, 1));
    lp.step(&mut app, NOW);
    let commits = compositor.with(|s, _| s.surface_of(0).commits.len());
    paint(&mut app.world).request.push(PixelRect::new(5, 5, 1, 1));
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits, "nowhere to draw");
    let redraws = paint(&mut app.world).redraws.len();
    compositor.with(|s, _| {
        let buffer = s.surface_of(0).buffer.unwrap();
        s.release(buffer);
    });
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws.len(), redraws + 1);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits + 1);
}

#[test]
fn an_animating_window_ticks_on_frame_callbacks() {
    let (compositor, mut lp, mut app, window) = mapped!(Globals::default());
    app.world.insert(window, Animating);
    lp.step(&mut app, NOW);
    let commits = compositor.with(|s, _| {
        let commit = s.surface_of(0).commits.last().unwrap();
        assert_eq!((commit.buffer, commit.frame_callbacks), (None, 1), "a frame request with nothing new to show");
        s.surface_of(0).commits.len()
    });
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits, "one callback outstanding at a time");
    compositor.with(|s, _| assert_eq!(s.fire_frames(0, 16), 1));
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits + 1);
    // Not animating: the callback that fires is the last.
    app.world.remove::<Animating>(window);
    compositor.with(|s, _| s.fire_frames(0, 32));
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits + 1);
}

#[test]
fn a_scale_change_reallocates_at_the_pixel_size() {
    let (compositor, mut lp, mut app, window) = mapped!(Globals::default());
    compositor.with(|s, _| s.preferred_scale(0, 180));
    lp.step(&mut app, NOW);
    assert_eq!(paint(&mut app.world).redraws.last().unwrap(), &((96, 48), whole(96, 48)));
    assert_eq!(app.world.get::<Framebuffer>(window).unwrap().scale(), Scale(180));
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        let buffer = surface.buffer.unwrap();
        assert_eq!((s.buffers[buffer].width, s.buffers[buffer].height), (96, 48));
        assert_eq!(surface.viewport_destination, Some((64, 32)));
    });
}

#[test]
fn the_viewport_is_sent_again_only_when_the_size_changes() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    let first = compositor.with(|s, _| {
        assert!(s.surface_of(0).commits.last().unwrap().geometry, "the first frame sets the viewport");
        s.surface_of(0).buffer.unwrap()
    });
    paint(&mut app.world).request.push(PixelRect::new(0, 0, 4, 4));
    lp.step(&mut app, NOW);
    compositor.with(move |s, _| s.release(first));
    paint(&mut app.world).request.push(PixelRect::new(8, 0, 4, 4));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        let commits = &s.surface_of(0).commits;
        let later = &commits[commits.len() - 2..];
        assert!(later.iter().all(|c| c.buffer.is_some() && !c.geometry), "later frames at the same size: {later:?}");
        let configure = s.configure(0, 80, 40, &[]);
        assert!(configure > 0);
    });
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        assert!(surface.commits.last().unwrap().geometry);
        assert_eq!(surface.viewport_destination, Some((80, 40)));
    });
}

#[test]
fn a_fixed_scale_framebuffer_keeps_its_pixels_and_the_viewport_stretches_them() {
    let (compositor, socket) = Compositor::spawn(Globals::default());
    let mut lp = WindowLoop::from_socket(socket).unwrap();
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("fixed").size(64, 32), Framebuffer::at_scale(Scale::ONE)));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.configure(0, 0, 0, &[]);
        s.preferred_scale(0, 240);
    });
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        let buffer = surface.buffer.unwrap();
        assert_eq!((s.buffers[buffer].width, s.buffers[buffer].height), (64, 32), "drawn at its own scale");
        assert_eq!(surface.viewport_destination, Some((64, 32)));
    });
}

#[test]
fn an_unchanged_framebuffer_commits_nothing() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    let commits = compositor.with(|s, _| s.surface_of(0).commits.len());
    for _ in 0..10 {
        lp.step(&mut app, NOW);
    }
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits);
    assert_eq!(paint(&mut app.world).redraws.len(), 1);
}
