//! Flappy bird as a plugin: physics, pipes, scoring, and a CPU rasterizer
//! drawing into whatever pixel buffer the window hands it.
use static_ecs::prelude::*;

pub const WIDTH: usize = 432;
pub const HEIGHT: usize = 768;
const GROUND: f32 = HEIGHT as f32 - 96.0;
const BIRD_X: f32 = 120.0;
const BIRD_SIZE: f32 = 34.0;
const GRAVITY: f32 = 1800.0;
const FLAP: f32 = -560.0;
const PIPE_WIDTH: f32 = 78.0;
const PIPE_GAP: f32 = 190.0;
const PIPE_SPEED: f32 = 170.0;
const PIPE_EVERY: f32 = 1.45;

/// A frame step of `dt` seconds.
pub struct Tick(pub f32);
impl Schedule for Tick {
    type In = Ref<Self>;
}
/// Rasterize the current state into the canvas.
pub struct Draw;
impl Schedule for Draw {}

pub struct Bird;
pub struct Pipe {
    scored: bool,
}
pub struct Pos(f32, f32);
pub struct Vel(f32, f32);
/// A pipe pair's opening, by its center height.
pub struct Gap(f32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Ready,
    Playing,
    Dead,
}
pub struct Game {
    pub state: State,
    pub score: u32,
    pub best: u32,
    /// Set by input between frames, consumed by the next tick.
    pub flap: bool,
    rng: u64,
}

/// The window's pixel buffer (XRGB8888) for the frame being drawn.
pub struct Canvas {
    pixels: *mut u32,
    width: usize,
    height: usize,
}
// SAFETY: the pointer is the mapped buffer the window points it at before
// every `Draw`; only the render system touches it, while the window waits.
unsafe impl Send for Canvas {}
unsafe impl Sync for Canvas {}
impl Canvas {
    pub fn new() -> Self {
        Self {
            pixels: std::ptr::null_mut(),
            width: 0,
            height: 0,
        }
    }
    /// # Safety
    /// `pixels` holds `width * height` writable pixels until the next call.
    pub unsafe fn target(&mut self, pixels: *mut u32, width: usize, height: usize) {
        (self.pixels, self.width, self.height) = (pixels, width, height);
    }
    fn pixels(&mut self) -> &mut [u32] {
        if self.pixels.is_null() {
            return &mut [];
        }
        // SAFETY: `target`'s contract.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.width * self.height) }
    }
}

pub struct Flappy;
impl Plugin for Flappy {
    type Components = (Bird, Pipe, Pos, Vel, Gap, Game, Canvas);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            schedule(
                Tick,
                (
                    flap::<S>,
                    gravity,
                    motion,
                    spawn_pipes::<S>,
                    cull::<S>,
                    score,
                    collide,
                )
                    .chain(),
            ),
            schedule(Draw, render),
        )
    }
}

/// The world's starting entities: the bird, the game, and its canvas.
pub fn spawn<S: Storage>(world: &mut World<S>, seed: u64) {
    world.spawn_bundle((Bird, Pos(BIRD_X, GROUND * 0.45), Vel(0.0, 0.0)));
    world.spawn_bundle((
        Game {
            state: State::Ready,
            score: 0,
            best: 0,
            flap: false,
            rng: seed | 1,
        },
        Canvas::new(),
    ));
}

fn flap<S: Storage>(
    _: &Tick,
    mut game: Single<&mut Game>,
    mut bird: Single<(&mut Pos, &mut Vel), With<Bird>>,
    mut pipes: Query<Entity, With<Pipe>>,
    mut commands: Commands<S>,
) {
    if !std::mem::take(&mut game.flap) {
        return;
    }
    let (pos, vel) = &mut *bird;
    match game.state {
        State::Ready | State::Playing => {
            game.state = State::Playing;
            vel.1 = FLAP;
        }
        State::Dead => {
            // Restart: clear the pipes, park the bird.
            for pipe in &mut pipes {
                commands.despawn(pipe);
            }
            (pos.1, vel.1) = (GROUND * 0.45, 0.0);
            (game.state, game.score) = (State::Ready, 0);
        }
    }
}

fn gravity(tick: &Tick, game: Single<&Game>, mut bird: Query<&mut Vel, With<Bird>>) {
    if game.state == State::Playing {
        for mut vel in &mut bird {
            vel.1 += GRAVITY * tick.0;
        }
    }
}

fn motion(tick: &Tick, game: Single<&Game>, mut moving: Query<(&mut Pos, &Vel)>) {
    if game.state == State::Playing {
        moving.each(|(mut pos, vel)| {
            pos.0 += vel.0 * tick.0;
            pos.1 += vel.1 * tick.0;
        });
    }
}

fn spawn_pipes<S: Storage>(
    tick: &Tick,
    mut game: Single<&mut Game>,
    mut since: Local<f32>,
    mut commands: Commands<S>,
) {
    if game.state != State::Playing {
        *since = PIPE_EVERY;
        return;
    }
    *since += tick.0;
    if *since >= PIPE_EVERY {
        *since -= PIPE_EVERY;
        let margin = 70.0 + PIPE_GAP / 2.0;
        let gap = margin + game.random() * (GROUND - 2.0 * margin);
        commands.spawn((
            Pipe { scored: false },
            Pos(WIDTH as f32, 0.0),
            Vel(-PIPE_SPEED, 0.0),
            Gap(gap),
        ));
    }
}

fn cull<S: Storage>(
    _: &Tick,
    mut pipes: Query<(Entity, &Pos), With<Pipe>>,
    mut commands: Commands<S>,
) {
    for (pipe, pos) in &mut pipes {
        if pos.0 < -PIPE_WIDTH {
            commands.despawn(pipe);
        }
    }
}

fn score(_: &Tick, mut game: Single<&mut Game>, mut pipes: Query<(&mut Pipe, &Pos)>) {
    for (mut pipe, pos) in &mut pipes {
        if !pipe.scored && pos.0 + PIPE_WIDTH < BIRD_X {
            pipe.scored = true;
            game.score += 1;
            game.best = game.best.max(game.score);
        }
    }
}

fn collide(
    _: &Tick,
    mut game: Single<&mut Game>,
    bird: Single<&Pos, With<Bird>>,
    mut pipes: Query<(&Pos, &Gap), With<Pipe>>,
) {
    if game.state != State::Playing {
        return;
    }
    let (x, y) = (bird.0, bird.1);
    let half = BIRD_SIZE / 2.0 - 3.0;
    let hit_pipe = pipes.iter_mut().any(|(pipe, gap)| {
        x + half > pipe.0
            && x - half < pipe.0 + PIPE_WIDTH
            && (y - half < gap.0 - PIPE_GAP / 2.0 || y + half > gap.0 + PIPE_GAP / 2.0)
    });
    if hit_pipe || y + half > GROUND || y - half < 0.0 {
        game.state = State::Dead;
    }
}

impl Game {
    /// Uniform in [0, 1).
    fn random(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }
}

const SKY_TOP: u32 = 0x4ec0ca;
const SKY_BOTTOM: u32 = 0xa8e4ea;
const PIPE: u32 = 0x73bf2e;
const PIPE_EDGE: u32 = 0x3d6e1a;
const GROUND_TOP: u32 = 0x73bf2e;
const DIRT: u32 = 0xded895;
const BIRD: u32 = 0xf8d038;
const BEAK: u32 = 0xf06a2c;
const WHITE: u32 = 0xffffff;
const BLACK: u32 = 0x202020;
const RED: u32 = 0xd23c3c;

fn render(
    mut canvas: Single<&mut Canvas>,
    game: Single<&Game>,
    bird: Single<&Pos, With<Bird>>,
    mut pipes: Query<(&Pos, &Gap), With<Pipe>>,
) {
    let (w, h) = (canvas.width, canvas.height);
    let px = canvas.pixels();
    if px.is_empty() {
        return;
    }
    let mut paint = Painter { px, w, h };
    // Sky: one gradient color per row.
    let ground = GROUND as usize;
    for y in 0..ground.min(h) {
        let color = lerp(SKY_TOP, SKY_BOTTOM, y as f32 / ground as f32);
        paint.px[y * w..(y + 1) * w].fill(color);
    }
    for (pos, gap) in pipes.iter_mut() {
        let x = pos.0 as i32;
        let top = (gap.0 - PIPE_GAP / 2.0) as i32;
        let bottom = (gap.0 + PIPE_GAP / 2.0) as i32;
        let pw = PIPE_WIDTH as i32;
        paint.rect(x, 0, pw, top, PIPE_EDGE);
        paint.rect(x + 4, 0, pw - 8, top - 4, PIPE);
        paint.rect(x - 6, top - 30, pw + 12, 30, PIPE_EDGE);
        paint.rect(x - 2, top - 26, pw + 4, 22, PIPE);
        paint.rect(x, bottom, pw, ground as i32 - bottom, PIPE_EDGE);
        paint.rect(x + 4, bottom + 4, pw - 8, ground as i32 - bottom - 4, PIPE);
        paint.rect(x - 6, bottom, pw + 12, 30, PIPE_EDGE);
        paint.rect(x - 2, bottom + 4, pw + 4, 22, PIPE);
    }
    paint.rect(0, ground as i32, w as i32, 12, GROUND_TOP);
    paint.rect(0, ground as i32 + 12, w as i32, h as i32, DIRT);
    // The bird: a body, an eye, a beak; red once dead.
    let (bx, by) = (bird.0 as i32, bird.1 as i32);
    let r = (BIRD_SIZE / 2.0) as i32;
    let body = if game.state == State::Dead { RED } else { BIRD };
    paint.disc(bx, by, r, BLACK);
    paint.disc(bx, by, r - 3, body);
    paint.disc(bx + 7, by - 6, 6, WHITE);
    paint.disc(bx + 9, by - 6, 3, BLACK);
    paint.rect(bx + r - 4, by + 1, 14, 7, BEAK);
    // Score, and the best score below it when not playing.
    paint.number(game.score, w as i32 / 2, 60, 8);
    if game.state != State::Playing && game.best > 0 {
        paint.number(game.best, w as i32 / 2, 150, 4);
    }
}

struct Painter<'a> {
    px: &'a mut [u32],
    w: usize,
    h: usize,
}
impl Painter<'_> {
    fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        let x0 = x.clamp(0, self.w as i32) as usize;
        let x1 = (x + width).clamp(0, self.w as i32) as usize;
        let y0 = y.clamp(0, self.h as i32) as usize;
        let y1 = (y + height).clamp(0, self.h as i32) as usize;
        if x0 >= x1 {
            return;
        }
        for y in y0..y1 {
            self.px[y * self.w + x0..y * self.w + x1].fill(color);
        }
    }
    fn disc(&mut self, cx: i32, cy: i32, r: i32, color: u32) {
        for dy in -r..=r {
            let half = (((r * r - dy * dy) as f32).sqrt()) as i32;
            self.rect(cx - half, cy + dy, 2 * half + 1, 1, color);
        }
    }
    /// A centered number in a 3x5 pixel font scaled by `scale`, outlined.
    fn number(&mut self, value: u32, cx: i32, y: i32, scale: i32) {
        let digits = value.to_string();
        let advance = 4 * scale;
        let x0 = cx - (digits.len() as i32 * advance - scale) / 2;
        for (outline, color) in [(true, BLACK), (false, WHITE)] {
            for (i, digit) in digits.bytes().enumerate() {
                let glyph = FONT[(digit - b'0') as usize];
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..3 {
                        if bits >> (2 - col) & 1 == 1 {
                            let (gx, gy) = (
                                x0 + i as i32 * advance + col * scale,
                                y + row as i32 * scale,
                            );
                            let pad = if outline { scale / 3 } else { 0 };
                            self.rect(gx - pad, gy - pad, scale + 2 * pad, scale + 2 * pad, color);
                        }
                    }
                }
            }
        }
    }
}

const FONT: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b010, 0b010, 0b010],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = ((a >> shift & 0xff) as f32, (b >> shift & 0xff) as f32);
        ((x + (y - x) * t) as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flap<S: Storage, L>(app: &mut App<S, L>)
    where
        L: static_ecs::Dispatch<Tick, World<S>>,
    {
        app.world.single::<&mut Game>().unwrap().flap = true;
        app.emit(&Tick(1.0 / 120.0));
    }

    #[test]
    fn the_bird_flies_scores_dies_and_restarts() {
        let mut app = App::new().add_plugins(Flappy).build();
        spawn(&mut app.world, 7);
        let state = |app: &mut App<_, _>| app.world.single::<&Game>().unwrap().state;
        let bird_y = |app: &mut App<_, _>| {
            app.world
                .query_filtered::<&Pos, With<Bird>>()
                .iter_mut()
                .next()
                .unwrap()
                .1
        };
        // Ready: nothing moves until the first flap.
        let start = bird_y(&mut app);
        app.emit(&Tick(0.5));
        assert_eq!((state(&mut app), bird_y(&mut app)), (State::Ready, start));
        flap(&mut app);
        assert_eq!(state(&mut app), State::Playing);
        // A pipe just past the bird, its gap around it, scores once.
        let y = bird_y(&mut app);
        app.world.spawn_bundle((
            Pipe { scored: false },
            Pos(BIRD_X - PIPE_WIDTH - 1.0, 0.0),
            Vel(-PIPE_SPEED, 0.0),
            Gap(y),
        ));
        app.emit(&Tick(1.0 / 120.0));
        app.emit(&Tick(1.0 / 120.0));
        assert_eq!(app.world.single::<&Game>().unwrap().score, 1);
        // Flapping whenever it sinks below the middle, pipes keep coming.
        for _ in 0..120 * 3 {
            if bird_y(&mut app) > GROUND * 0.55 {
                flap(&mut app);
            } else {
                app.emit(&Tick(1.0 / 120.0));
            }
        }
        assert!(app.world.query::<&Pipe>().iter_mut().count() > 0);
        // Without flapping it falls and hits the ground or a pipe.
        for _ in 0..120 * 4 {
            app.emit(&Tick(1.0 / 120.0));
        }
        assert_eq!(state(&mut app), State::Dead);
        let best = app.world.single::<&Game>().unwrap().best;
        assert!(best >= 1);
        // A flap restarts: no pipes, score reset, best kept.
        flap(&mut app);
        let game = |app: &mut App<_, _>| {
            let game = app.world.single::<&Game>().unwrap();
            (game.state, game.score, game.best)
        };
        assert_eq!(game(&mut app), (State::Ready, 0, best));
        assert_eq!(app.world.query::<&Pipe>().iter_mut().count(), 0);
    }

    #[test]
    fn drawing_paints_sky_ground_and_bird() {
        let mut app = App::new().add_plugins(Flappy).build();
        spawn(&mut app.world, 7);
        let mut pixels = vec![0u32; WIDTH * HEIGHT];
        unsafe {
            app.world
                .single::<&mut Canvas>()
                .unwrap()
                .target(pixels.as_mut_ptr(), WIDTH, HEIGHT)
        };
        app.emit(&Draw);
        let at = |x: usize, y: usize| pixels[y * WIDTH + x];
        assert_eq!(at(WIDTH - 1, 0), SKY_TOP);
        assert_eq!(at(0, HEIGHT - 1), DIRT);
        assert_eq!(at(BIRD_X as usize - 6, (GROUND * 0.45) as usize + 6), BIRD);
    }
}
