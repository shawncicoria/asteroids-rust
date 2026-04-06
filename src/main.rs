// Asteroids – classic 1980s arcade game in a Rust TUI
// Controls: Arrow keys or WASD to steer/thrust/brake, SPACE to fire, X to stop, P to pause, Q to quit
//
// Extra CLI flags:
//   --trace  <file>   record every input action + RNG seed to a JSON file
//   --replay <file>   replay a previously recorded trace in the TUI
//   --speed  <f>      replay speed multiplier (default 1.0; 0.5 = half speed)

use std::{
    f64::consts::PI,
    fs,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{
        canvas::{Canvas, Line as CLine, Points},
        Block, Borders, Paragraph,
    },
    Frame, Terminal,
};

// ─────────────────────────── constants ───────────────────────────

const W: f64 = 200.0; // game-space width
const H: f64 = 100.0; // game-space height

const THRUST_ACCEL: f64 = 0.22;
const MAX_SPEED: f64 = 5.5;
const FRICTION: f64 = 0.988;
const ROT_SPEED: f64 = 4.5 * PI / 180.0; // radians per frame
const BULLET_SPEED: f64 = 7.5;
const BULLET_LIFE: u32 = 52;
const MAX_BULLETS: usize = 4;
const SHOOT_CD: u32 = 12; // frames between shots
const INVINCIBLE: u32 = 150; // frames of invincibility after spawn

// ─────────────────────────── asteroid ────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    Large,
    Medium,
    Small,
}

impl Size {
    fn radius(self) -> f64 {
        match self {
            Size::Large => 8.0,
            Size::Medium => 4.5,
            Size::Small => 2.2,
        }
    }
    fn score(self) -> u32 {
        match self {
            Size::Large => 20,
            Size::Medium => 50,
            Size::Small => 100,
        }
    }
    fn split(self) -> Option<Size> {
        match self {
            Size::Large => Some(Size::Medium),
            Size::Medium => Some(Size::Small),
            Size::Small => None,
        }
    }
    fn speed(self) -> f64 {
        match self {
            Size::Large => 0.55,
            Size::Medium => 1.05,
            Size::Small => 1.8,
        }
    }
}

struct Rock {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    size: Size,
    rot: f64,   // current orientation (radians)
    rot_v: f64, // rotation speed (radians/frame)
    verts: Vec<(f64, f64)>, // unit-circle vertices (normalised to radius 1)
}

impl Rock {
    fn new(x: f64, y: f64, vx: f64, vy: f64, size: Size, rng: &mut impl Rng) -> Self {
        let n = rng.gen_range(8..=12usize);
        let rot_v = (rng.gen::<f64>() - 0.5) * 3.0 * PI / 180.0;
        let verts = (0..n)
            .map(|i| {
                let base = 2.0 * PI * i as f64 / n as f64;
                let jitter = (rng.gen::<f64>() - 0.5) * PI / n as f64;
                let a = base + jitter;
                let r = 0.6 + rng.gen::<f64>() * 0.4;
                (r * a.cos(), r * a.sin())
            })
            .collect();
        Rock {
            x,
            y,
            vx,
            vy,
            size,
            rot: rng.gen::<f64>() * 2.0 * PI,
            rot_v,
            verts,
        }
    }

    fn update(&mut self) {
        self.x = (self.x + self.vx).rem_euclid(W);
        self.y = (self.y + self.vy).rem_euclid(H);
        self.rot = (self.rot + self.rot_v).rem_euclid(2.0 * PI);
    }

    fn world_verts(&self) -> Vec<(f64, f64)> {
        let r = self.size.radius();
        let (s, c) = self.rot.sin_cos();
        self.verts
            .iter()
            .map(|(px, py)| {
                (self.x + r * (px * c - py * s), self.y + r * (px * s + py * c))
            })
            .collect()
    }
}

// ─────────────────────────── ship ────────────────────────────────

struct Ship {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    angle: f64, // radians; 0 = pointing up (+y), clockwise positive
    inv: u32,   // remaining invincibility frames
}

impl Ship {
    fn spawn() -> Self {
        Ship {
            x: W / 2.0,
            y: H / 2.0,
            vx: 0.0,
            vy: 0.0,
            angle: 0.0,
            inv: INVINCIBLE,
        }
    }

    fn update(&mut self) {
        self.x = (self.x + self.vx).rem_euclid(W);
        self.y = (self.y + self.vy).rem_euclid(H);
        self.vx *= FRICTION;
        self.vy *= FRICTION;
        self.inv = self.inv.saturating_sub(1);
    }

    fn thrust(&mut self) {
        let (sa, ca) = self.angle.sin_cos();
        self.vx += THRUST_ACCEL * sa;
        self.vy += THRUST_ACCEL * ca;
        let spd = (self.vx * self.vx + self.vy * self.vy).sqrt();
        if spd > MAX_SPEED {
            let k = MAX_SPEED / spd;
            self.vx *= k;
            self.vy *= k;
        }
    }

    /// Brake: apply reverse thrust opposite to current velocity direction.
    fn brake(&mut self) {
        let spd = (self.vx * self.vx + self.vy * self.vy).sqrt();
        if spd < 0.05 {
            self.vx = 0.0;
            self.vy = 0.0;
            return;
        }
        let decel = (THRUST_ACCEL * 1.5).min(spd);
        self.vx -= decel * self.vx / spd;
        self.vy -= decel * self.vy / spd;
    }

    /// Full stop.
    fn stop(&mut self) {
        self.vx = 0.0;
        self.vy = 0.0;
    }

    fn rotate(&mut self, dir: f64) {
        self.angle = (self.angle + dir * ROT_SPEED).rem_euclid(2.0 * PI);
    }

    fn fire(&self) -> Bullet {
        let (sa, ca) = self.angle.sin_cos();
        Bullet {
            x: self.x + 4.0 * sa,
            y: self.y + 4.0 * ca,
            vx: self.vx + BULLET_SPEED * sa,
            vy: self.vy + BULLET_SPEED * ca,
            life: BULLET_LIFE,
        }
    }

    /// Triangle vertices in world space.
    /// Local frame: nose=(0,3.5), left=(-2,-2.2), right=(2,-2.2)
    fn tri(&self) -> [(f64, f64); 3] {
        let (sa, ca) = self.angle.sin_cos();
        let rot = |lx: f64, ly: f64| (self.x + ca * lx + sa * ly, self.y - sa * lx + ca * ly);
        [rot(0.0, 3.5), rot(-2.0, -2.2), rot(2.0, -2.2)]
    }

    fn radius(&self) -> f64 {
        2.5
    }
}

// ─────────────────────────── bullet / particle ───────────────────

struct Bullet {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    life: u32,
}

struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    life: u32,
}

impl Bullet {
    fn update(&mut self) {
        self.x = (self.x + self.vx).rem_euclid(W);
        self.y = (self.y + self.vy).rem_euclid(H);
        self.life = self.life.saturating_sub(1);
    }
}

impl Particle {
    fn update(&mut self) {
        self.x = (self.x + self.vx).rem_euclid(W);
        self.y = (self.y + self.vy).rem_euclid(H);
        self.life = self.life.saturating_sub(1);
    }
}

// ─────────────────────────── game state ──────────────────────────

#[derive(PartialEq, Eq, Debug)]
enum Phase {
    Title,
    Playing,
    Paused,
    Dead,
}

struct Game {
    phase: Phase,
    ship: Ship,
    rocks: Vec<Rock>,
    bullets: Vec<Bullet>,
    sparks: Vec<Particle>,
    score: u32,
    hi: u32,
    lives: u32,
    level: u32,
    cooldown: u32,
    thrust_on: bool,         // used only by renderer for exhaust flame
    brake_on: bool,          // used only by renderer
    // Key-held timestamps: refreshed on every Press/Repeat event.
    // A key is considered held as long as its timestamp is < KEY_HOLD ms old.
    // This works even on terminals that never send Release events.
    left_ts: Option<Instant>,
    right_ts: Option<Instant>,
    up_ts: Option<Instant>,
    down_ts: Option<Instant>,
    rng: StdRng,
    /// Monotonically increasing tick counter (used by the trace recorder).
    tick_count: u64,
}

impl Game {
    /// Create a game with a fixed RNG seed – used by tests and trace replay.
    fn new_seeded(seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let rocks = wave(1, &mut rng);
        Game {
            phase: Phase::Title,
            ship: Ship::spawn(),
            rocks,
            bullets: Vec::new(),
            sparks: Vec::new(),
            score: 0,
            hi: 0,
            lives: 3,
            level: 1,
            cooldown: 0,
            thrust_on: false,
            brake_on: false,
            left_ts: None,
            right_ts: None,
            up_ts: None,
            down_ts: None,
            rng,
            tick_count: 0,
        }
    }

    fn reset(&mut self) {
        self.score = 0;
        self.lives = 3;
        self.level = 1;
        self.ship = Ship::spawn();
        self.rocks = wave(1, &mut self.rng);
        self.bullets.clear();
        self.sparks.clear();
        self.left_ts = None;
        self.right_ts = None;
        self.up_ts = None;
        self.down_ts = None;
        self.phase = Phase::Playing;
    }

    fn respawn(&mut self) {
        self.ship = Ship::spawn();
        self.bullets.clear();
        self.left_ts = None;
        self.right_ts = None;
        self.up_ts = None;
        self.down_ts = None;
    }

    fn next_level(&mut self) {
        self.level += 1;
        self.ship = Ship::spawn();
        self.bullets.clear();
        self.rocks = wave(self.level, &mut self.rng);
    }

    fn shoot(&mut self) {
        if self.phase == Phase::Playing
            && self.cooldown == 0
            && self.bullets.len() < MAX_BULLETS
        {
            self.bullets.push(self.ship.fire());
            self.cooldown = SHOOT_CD;
        }
    }

    fn tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        // 100 ms: longer than any normal key-repeat interval (~33 ms),
        // so a held key always refreshes in time; a released key times out quickly.
        const KEY_HOLD: Duration = Duration::from_millis(100);
        let held = |ts: Option<Instant>| ts.map_or(false, |t| t.elapsed() < KEY_HOLD);

        if held(self.left_ts)  { self.ship.rotate(-1.0); }
        if held(self.right_ts) { self.ship.rotate( 1.0); }
        self.thrust_on = held(self.up_ts);
        self.brake_on  = held(self.down_ts);
        if self.thrust_on { self.ship.thrust(); }
        if self.brake_on  { self.ship.brake();  }
        self.cooldown = self.cooldown.saturating_sub(1);
        self.tick_count += 1;

        self.ship.update();

        for b in &mut self.bullets {
            b.update();
        }
        self.bullets.retain(|b| b.life > 0);

        for r in &mut self.rocks {
            r.update();
        }

        for s in &mut self.sparks {
            s.update();
        }
        self.sparks.retain(|s| s.life > 0);

        self.bullet_rock_collisions();

        if self.ship.inv == 0 {
            self.ship_rock_collision();
        }

        if self.rocks.is_empty() {
            self.next_level();
        }
    }

    fn bullet_rock_collisions(&mut self) {
        let mut b_hit = vec![false; self.bullets.len()];
        let mut r_hit = vec![false; self.rocks.len()];
        // Collect hits first so we can call self.explode afterwards
        let mut hits: Vec<(f64, f64, Size)> = Vec::new();

        for (bi, b) in self.bullets.iter().enumerate() {
            for (ri, r) in self.rocks.iter().enumerate() {
                if b_hit[bi] || r_hit[ri] {
                    continue;
                }
                if wdist(b.x, b.y, r.x, r.y) < r.size.radius() {
                    b_hit[bi] = true;
                    r_hit[ri] = true;
                    self.score += r.size.score();
                    hits.push((r.x, r.y, r.size));
                }
            }
        }

        kill(&mut self.bullets, &b_hit);
        kill(&mut self.rocks, &r_hit);
        if self.score > self.hi {
            self.hi = self.score;
        }

        for (x, y, size) in hits {
            self.explode(x, y, 10);
            if let Some(smaller) = size.split() {
                for _ in 0..2 {
                    let ang = self.rng.gen::<f64>() * 2.0 * PI;
                    let spd = smaller.speed() * (0.7 + self.rng.gen::<f64>() * 0.6);
                    let rock = Rock::new(
                        x,
                        y,
                        ang.cos() * spd,
                        ang.sin() * spd,
                        smaller,
                        &mut self.rng,
                    );
                    self.rocks.push(rock);
                }
            }
        }
    }

    fn ship_rock_collision(&mut self) {
        let (sx, sy) = (self.ship.x, self.ship.y);
        let hit = self
            .rocks
            .iter()
            .any(|r| wdist(sx, sy, r.x, r.y) < r.size.radius() + self.ship.radius());
        if hit {
            self.explode(sx, sy, 20);
            if self.lives > 1 {
                self.lives -= 1;
                self.respawn();
            } else {
                self.lives = 0;
                self.phase = Phase::Dead;
            }
        }
    }

    fn explode(&mut self, x: f64, y: f64, n: usize) {
        for _ in 0..n {
            let ang = self.rng.gen::<f64>() * 2.0 * PI;
            let spd = self.rng.gen::<f64>() * 2.8 + 0.3;
            let life = self.rng.gen_range(15..45u32);
            self.sparks.push(Particle {
                x,
                y,
                vx: ang.cos() * spd,
                vy: ang.sin() * spd,
                life,
            });
        }
    }
}

// ─────────────────────────── helpers ─────────────────────────────

/// Shortest wrapped distance between two points in the toroidal game space.
fn wdist(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let dx = {
        let d = (x2 - x1).rem_euclid(W);
        if d > W / 2.0 { d - W } else { d }
    };
    let dy = {
        let d = (y2 - y1).rem_euclid(H);
        if d > H / 2.0 { d - H } else { d }
    };
    (dx * dx + dy * dy).sqrt()
}

/// Remove elements of `v` where `dead[i]` is true.
fn kill<T>(v: &mut Vec<T>, dead: &[bool]) {
    let mut i = 0;
    v.retain(|_| {
        let keep = !dead[i];
        i += 1;
        keep
    });
}

/// Spawn a wave of large asteroids, safely away from the centre.
fn wave(level: u32, rng: &mut impl Rng) -> Vec<Rock> {
    let n = 3 + level as usize;
    (0..n)
        .map(|_| {
            let (x, y) = loop {
                let x = rng.gen::<f64>() * W;
                let y = rng.gen::<f64>() * H;
                if wdist(x, y, W / 2.0, H / 2.0) > 25.0 {
                    break (x, y);
                }
            };
            let ang = rng.gen::<f64>() * 2.0 * PI;
            let spd = Size::Large.speed() * (0.6 + rng.gen::<f64>() * 0.8);
            Rock::new(x, y, ang.cos() * spd, ang.sin() * spd, Size::Large, rng)
        })
        .collect()
}

// ─────────────────────────── trace / replay ──────────────────────

/// A named input action, stored in the trace file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "type", content = "key")]
pub enum TraceAction {
    KeyDown(String),
    KeyUp(String),
    Fire,
    Stop,
    Reset,
    Pause,
    Resume,
    Quit,
}

/// One recorded event: the game tick it happened on + the action.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TraceEvent {
    pub tick: u64,
    pub action: TraceAction,
}

/// The complete trace file: seed + ordered list of events.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Trace {
    pub seed: u64,
    pub events: Vec<TraceEvent>,
}

/// Collects events during a live game session.
pub struct Recorder {
    seed: u64,
    events: Vec<TraceEvent>,
}

impl Recorder {
    fn new(seed: u64) -> Self {
        Recorder { seed, events: Vec::new() }
    }

    fn record(&mut self, tick: u64, action: TraceAction) {
        self.events.push(TraceEvent { tick, action });
    }

    fn save(&self, path: &PathBuf) -> io::Result<()> {
        let trace = Trace { seed: self.seed, events: self.events.clone() };
        let json = serde_json::to_string_pretty(&trace)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(path, json)
    }
}

fn load_trace(path: &PathBuf) -> io::Result<Trace> {
    let json = fs::read_to_string(path)?;
    serde_json::from_str(&json).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
}

// ─────────────────────────── rendering ───────────────────────────

fn draw(f: &mut Frame, g: &Game) {
    let area = f.area();

    // 1-line header + canvas fills the rest
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let (hdr_area, canvas_area) = (chunks[0], chunks[1]);

    // ── header bar ──────────────────────────────────────────────
    let lives_str: String = "♦ ".repeat(g.lives as usize);
    let hdr = format!(
        " SCORE {:06}   HI {:06}   {}  LEVEL {}",
        g.score, g.hi, lives_str, g.level
    );
    f.render_widget(
        Paragraph::new(hdr)
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Left),
        hdr_area,
    );

    // ── game canvas ─────────────────────────────────────────────
    let canvas = Canvas::default()
        .block(Block::default().borders(Borders::NONE))
        .x_bounds([0.0, W])
        .y_bounds([0.0, H])
        .paint(|ctx| {
            // ── asteroids ──
            for rock in &g.rocks {
                let vv = rock.world_verts();
                let n = vv.len();
                for i in 0..n {
                    let (x1, y1) = vv[i];
                    let (x2, y2) = vv[(i + 1) % n];
                    // Skip edges that jump across the wrap boundary
                    if (x1 - x2).abs() < W / 2.0 && (y1 - y2).abs() < H / 2.0 {
                        ctx.draw(&CLine { x1, y1, x2, y2, color: Color::White });
                    }
                }
            }

            // ── ship ──
            if g.phase == Phase::Playing {
                // Blink while invincible (4 on, 2 off per 6-frame cycle)
                let visible = g.ship.inv == 0 || g.ship.inv % 6 < 4;
                if visible {
                    let [a, b, c] = g.ship.tri();
                    let col = Color::Cyan;
                    ctx.draw(&CLine { x1: a.0, y1: a.1, x2: b.0, y2: b.1, color: col });
                    ctx.draw(&CLine { x1: b.0, y1: b.1, x2: c.0, y2: c.1, color: col });
                    ctx.draw(&CLine { x1: c.0, y1: c.1, x2: a.0, y2: a.1, color: col });

                    // Rear exhaust flame when thrusting forward
                    if g.thrust_on {
                        let (sa, ca) = g.ship.angle.sin_cos();
                        let tip = (g.ship.x - sa * 5.8, g.ship.y - ca * 5.8);
                        let bl = (
                            g.ship.x - sa * 2.3 - ca * 1.1,
                            g.ship.y - ca * 2.3 + sa * 1.1,
                        );
                        let br = (
                            g.ship.x - sa * 2.3 + ca * 1.1,
                            g.ship.y - ca * 2.3 - sa * 1.1,
                        );
                        ctx.draw(&CLine {
                            x1: bl.0, y1: bl.1, x2: tip.0, y2: tip.1,
                            color: Color::Yellow,
                        });
                        ctx.draw(&CLine {
                            x1: br.0, y1: br.1, x2: tip.0, y2: tip.1,
                            color: Color::Yellow,
                        });
                    }
                    // Nose retro-fire when braking
                    if g.brake_on {
                        let (sa, ca) = g.ship.angle.sin_cos();
                        let tip = (g.ship.x + sa * 5.0, g.ship.y + ca * 5.0);
                        let bl = (
                            g.ship.x + sa * 2.5 - ca * 0.9,
                            g.ship.y + ca * 2.5 + sa * 0.9,
                        );
                        let br = (
                            g.ship.x + sa * 2.5 + ca * 0.9,
                            g.ship.y + ca * 2.5 - sa * 0.9,
                        );
                        ctx.draw(&CLine {
                            x1: bl.0, y1: bl.1, x2: tip.0, y2: tip.1,
                            color: Color::Red,
                        });
                        ctx.draw(&CLine {
                            x1: br.0, y1: br.1, x2: tip.0, y2: tip.1,
                            color: Color::Red,
                        });
                    }
                }
            }

            // ── bullets ──
            let bpts: Vec<(f64, f64)> = g.bullets.iter().map(|b| (b.x, b.y)).collect();
            if !bpts.is_empty() {
                ctx.draw(&Points { coords: &bpts, color: Color::White });
            }

            // ── explosion sparks ──
            let spts: Vec<(f64, f64)> = g.sparks.iter().map(|s| (s.x, s.y)).collect();
            if !spts.is_empty() {
                ctx.draw(&Points { coords: &spts, color: Color::Yellow });
            }

            // ── text overlays ──
            match g.phase {
                Phase::Title => {
                    ctx.print(62.0, 72.0, "* A S T E R O I D S *");
                    ctx.print(60.0, 60.0, "ENTER or SPACE  -  start game");
                    ctx.print(22.0, 50.0,
                        "Arrows/WASD: rotate  Up: thrust  Down: brake  X: stop  SPACE: fire  P: pause  Q: quit");
                    ctx.print(72.0, 38.0, "Good luck!");
                }
                Phase::Dead => {
                    ctx.print(72.0, 62.0, "GAME  OVER");
                    ctx.print(55.0, 50.0, "ENTER or SPACE  -  play again");
                }
                Phase::Paused => {
                    ctx.print(83.0, 56.0, "PAUSED");
                    ctx.print(72.0, 46.0, "P  -  resume");
                }
                Phase::Playing => {}
            }
        });

    f.render_widget(canvas, canvas_area);
}

// ─────────────────────────── CLI / main ──────────────────────────

struct Cli {
    trace_out: Option<PathBuf>,
    replay_in: Option<PathBuf>,
    speed: f64,
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1);
    let mut cli = Cli { trace_out: None, replay_in: None, speed: 1.0 };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trace"  => cli.trace_out = Some(PathBuf::from(
                args.next().ok_or("--trace requires a file path")?)),
            "--replay" => cli.replay_in = Some(PathBuf::from(
                args.next().ok_or("--replay requires a file path")?)),
            "--speed"  => cli.speed = args.next()
                .ok_or("--speed requires a number")?
                .parse::<f64>()
                .map_err(|_| "--speed must be a positive number")?,
            other => return Err(format!("Unknown argument: {other}")),
        }
    }
    if cli.speed <= 0.0 { return Err("--speed must be > 0".into()); }
    Ok(cli)
}

fn main() -> io::Result<()> {
    let cli = parse_args().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        eprintln!("usage: asteroids [--trace FILE] [--replay FILE] [--speed FACTOR]");
        std::process::exit(1);
    });

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = if let Some(ref path) = cli.replay_in {
        let trace = load_trace(path)
            .map_err(|e| io::Error::new(e.kind(), format!("Cannot load trace: {e}")))?;
        run_replay(&mut terminal, trace, cli.speed)
    } else {
        run_loop(&mut terminal, cli.trace_out.as_ref())
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

/// Process one key event. Returns `true` to keep running, `false` to quit.
/// Optionally records the action into `rec`.
fn handle_key(
    game: &mut Game,
    key: crossterm::event::KeyEvent,
    rec: Option<&mut Recorder>,
) -> bool {
    let now = Instant::now();
    let tick = game.tick_count;

    match key.kind {
        KeyEventKind::Press | KeyEventKind::Repeat => match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                if let Some(r) = rec { r.record(tick, TraceAction::Quit); }
                return false;
            }

            KeyCode::Enter | KeyCode::Char(' ')
                if matches!(game.phase, Phase::Title | Phase::Dead) =>
            {
                if let Some(r) = rec { r.record(tick, TraceAction::Reset); }
                game.reset();
            }

            KeyCode::Char('p') | KeyCode::Char('P') => match game.phase {
                Phase::Playing => {
                    if let Some(r) = rec { r.record(tick, TraceAction::Pause); }
                    game.phase = Phase::Paused;
                }
                Phase::Paused => {
                    if let Some(r) = rec { r.record(tick, TraceAction::Resume); }
                    game.phase = Phase::Playing;
                }
                _ => {}
            },

            // Direction keys: stamp the timestamp so tick() sees them as held.
            KeyCode::Left  | KeyCode::Char('a') | KeyCode::Char('A') => {
                if game.left_ts.is_none() {
                    if let Some(r) = rec { r.record(tick, TraceAction::KeyDown("left".into())); }
                }
                game.left_ts = Some(now);
            }
            KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('D') => {
                if game.right_ts.is_none() {
                    if let Some(r) = rec { r.record(tick, TraceAction::KeyDown("right".into())); }
                }
                game.right_ts = Some(now);
            }
            KeyCode::Up    | KeyCode::Char('w') | KeyCode::Char('W') => {
                if game.up_ts.is_none() {
                    if let Some(r) = rec { r.record(tick, TraceAction::KeyDown("up".into())); }
                }
                game.up_ts = Some(now);
            }
            KeyCode::Down  | KeyCode::Char('s') | KeyCode::Char('S') => {
                if game.down_ts.is_none() {
                    if let Some(r) = rec { r.record(tick, TraceAction::KeyDown("down".into())); }
                }
                game.down_ts = Some(now);
            }

            KeyCode::Char('x') | KeyCode::Char('X') if game.phase == Phase::Playing => {
                if let Some(r) = rec { r.record(tick, TraceAction::Stop); }
                game.ship.stop();
            }
            KeyCode::Char(' ') => {
                if let Some(r) = rec { r.record(tick, TraceAction::Fire); }
                game.shoot();
            }
            _ => {}
        },

        // Release: clear immediately on terminals that send release events.
        // On terminals that don't, the 100 ms timeout in tick() handles it.
        KeyEventKind::Release => match key.code {
            KeyCode::Left  | KeyCode::Char('a') | KeyCode::Char('A') => {
                if let Some(r) = rec { r.record(tick, TraceAction::KeyUp("left".into())); }
                game.left_ts = None;
            }
            KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(r) = rec { r.record(tick, TraceAction::KeyUp("right".into())); }
                game.right_ts = None;
            }
            KeyCode::Up    | KeyCode::Char('w') | KeyCode::Char('W') => {
                if let Some(r) = rec { r.record(tick, TraceAction::KeyUp("up".into())); }
                game.up_ts = None;
            }
            KeyCode::Down  | KeyCode::Char('s') | KeyCode::Char('S') => {
                if let Some(r) = rec { r.record(tick, TraceAction::KeyUp("down".into())); }
                game.down_ts = None;
            }
            _ => {}
        },
    }
    true
}

/// Inject a trace action directly into game state (used by replay engine).
fn apply_trace_action(game: &mut Game, action: &TraceAction) -> bool {
    let held_now = Some(Instant::now());
    match action {
        TraceAction::Quit    => return false,
        TraceAction::Reset   => game.reset(),
        TraceAction::Fire    => game.shoot(),
        TraceAction::Stop    => { if game.phase == Phase::Playing { game.ship.stop(); } }
        TraceAction::Pause   => { if game.phase == Phase::Playing  { game.phase = Phase::Paused; } }
        TraceAction::Resume  => { if game.phase == Phase::Paused   { game.phase = Phase::Playing; } }
        TraceAction::KeyDown(k) => match k.as_str() {
            "left"  => game.left_ts  = held_now,
            "right" => game.right_ts = held_now,
            "up"    => game.up_ts    = held_now,
            "down"  => game.down_ts  = held_now,
            _ => {}
        },
        TraceAction::KeyUp(k) => match k.as_str() {
            "left"  => game.left_ts  = None,
            "right" => game.right_ts = None,
            "up"    => game.up_ts    = None,
            "down"  => game.down_ts  = None,
            _ => {}
        },
    }
    true
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    trace_path: Option<&PathBuf>,
) -> io::Result<()> {
    let seed: u64 = rand::random();
    let mut game = Game::new_seeded(seed);

    let mut rec = trace_path.map(|_| Recorder::new(seed));

    let frame_time = Duration::from_millis(16); // ~60 fps
    let mut next_tick = Instant::now();

    loop {
        // Drain every event already queued (non-blocking).
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if !handle_key(&mut game, key, rec.as_mut()) {
                    if let (Some(r), Some(p)) = (&rec, trace_path) {
                        r.save(p)?;
                    }
                    return Ok(());
                }
            }
        }

        let now = Instant::now();
        if now >= next_tick {
            game.tick();
            terminal.draw(|f| draw(f, &game))?;
            next_tick += frame_time;
            if next_tick < Instant::now() {
                next_tick = Instant::now() + frame_time;
            }
        } else {
            let wait = next_tick - now;
            if event::poll(wait)? {
                if let Event::Key(key) = event::read()? {
                    if !handle_key(&mut game, key, rec.as_mut()) {
                        if let (Some(r), Some(p)) = (&rec, trace_path) {
                            r.save(p)?;
                        }
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Replay a saved trace in the TUI at the requested speed multiplier.
/// `speed` > 1 = faster, `speed` < 1 = slower.
fn run_replay(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    trace: Trace,
    speed: f64,
) -> io::Result<()> {
    let mut game = Game::new_seeded(trace.seed);
    // Start in Playing state immediately so the replay is visible.
    game.reset();

    let base_frame = Duration::from_millis(16); // 60 fps reference
    // Scale: speed=2 → half the wait, speed=0.5 → double the wait
    let frame_time = base_frame.div_f64(speed);
    let mut next_tick = Instant::now();

    let mut event_iter = trace.events.iter().peekable();

    loop {
        // Drain real keyboard events so the user can quit with Q.
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.kind, KeyEventKind::Press) {
                    if matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q')) {
                        return Ok(());
                    }
                }
            }
        }

        let now = Instant::now();
        if now >= next_tick {
            // Inject any trace events scheduled for this tick.
            while event_iter
                .peek()
                .map_or(false, |e| e.tick <= game.tick_count)
            {
                let ev = event_iter.next().unwrap();
                if !apply_trace_action(&mut game, &ev.action) {
                    return Ok(());
                }
            }

            game.tick();
            terminal.draw(|f| draw(f, &game))?;
            next_tick += frame_time;
            if next_tick < Instant::now() {
                next_tick = Instant::now() + frame_time;
            }

            // Stop when all trace events have been replayed and a few extra ticks pass.
            if event_iter.peek().is_none()
                && game.tick_count > trace.events.last().map_or(0, |e| e.tick) + 120
            {
                return Ok(());
            }
        } else {
            let wait = next_tick - now;
            event::poll(wait)?; // just wait; real events handled next iteration
        }
    }
}

// ─────────────────────────── tests ───────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────

    /// A game in Playing phase with no asteroids and ship invincibility cleared,
    /// ready for precise per-tick testing.
    fn bare(seed: u64) -> Game {
        let mut g = Game::new_seeded(seed);
        g.phase = Phase::Playing;
        g.rocks.clear();
        g.ship.inv = 0;
        g
    }

    /// Regular n-gon vertices on the unit circle (used for predictable rocks).
    fn circle_verts(n: usize) -> Vec<(f64, f64)> {
        (0..n)
            .map(|i| {
                let a = 2.0 * PI * i as f64 / n as f64;
                (a.cos(), a.sin())
            })
            .collect()
    }

    /// Place a stationary rock with a known circular shape.
    fn place_rock(g: &mut Game, x: f64, y: f64, size: Size) {
        g.rocks.push(Rock {
            x, y, vx: 0.0, vy: 0.0, size,
            rot: 0.0, rot_v: 0.0,
            verts: circle_verts(8),
        });
    }

    /// Place a stationary bullet at (x, y) with full lifetime.
    fn place_bullet(g: &mut Game, x: f64, y: f64) {
        g.bullets.push(Bullet { x, y, vx: 0.0, vy: 0.0, life: BULLET_LIFE });
    }

    // ── initial state ─────────────────────────────────────────────

    #[test]
    fn game_starts_in_title_phase() {
        let g = Game::new_seeded(1);
        assert_eq!(g.phase, Phase::Title);
    }

    #[test]
    fn ship_spawns_at_centre() {
        let g = Game::new_seeded(1);
        assert!((g.ship.x - W / 2.0).abs() < 1e-9);
        assert!((g.ship.y - H / 2.0).abs() < 1e-9);
    }

    #[test]
    fn ship_starts_stationary() {
        let g = Game::new_seeded(1);
        assert_eq!(g.ship.vx, 0.0);
        assert_eq!(g.ship.vy, 0.0);
    }

    #[test]
    fn reset_moves_to_playing_with_three_lives() {
        let mut g = Game::new_seeded(1);
        g.reset();
        assert_eq!(g.phase, Phase::Playing);
        assert_eq!(g.lives, 3);
        assert_eq!(g.score, 0);
        assert_eq!(g.level, 1);
    }

    // ── rotation ─────────────────────────────────────────────────
    // Physics tests call ship methods directly — no tick() needed, so we
    // avoid the empty-rocks→next_level() side-effect.

    #[test]
    fn right_key_rotates_clockwise() {
        let mut ship = Ship::spawn();
        let before = ship.angle;
        ship.rotate(1.0);
        assert!(ship.angle > before, "angle should increase (clockwise)");
    }

    #[test]
    fn left_key_rotates_counter_clockwise() {
        let mut ship = Ship::spawn();
        ship.angle = PI / 2.0;
        let before = ship.angle;
        ship.rotate(-1.0);
        assert!(ship.angle < before, "angle should decrease (CCW)");
    }

    #[test]
    fn rotation_wraps_at_2pi() {
        let mut ship = Ship::spawn();
        ship.angle = 2.0 * PI - 0.001;
        ship.rotate(1.0);
        assert!(ship.angle < PI, "angle should wrap around to near 0");
    }

    // ── thrust & brake ───────────────────────────────────────────

    #[test]
    fn up_key_thrusts_ship_forward() {
        let mut ship = Ship::spawn();
        ship.angle = 0.0; // pointing up → +y
        ship.thrust();
        assert!(ship.vy > 0.0, "thrust should push ship upward");
        assert!(ship.vx.abs() < 1e-9, "no sideways drift");
    }

    #[test]
    fn thrust_is_angle_dependent() {
        let mut ship = Ship::spawn();
        ship.angle = PI / 2.0; // pointing right → +x
        ship.thrust();
        assert!(ship.vx > 0.0, "thrust should push right");
        assert!(ship.vy.abs() < 1e-6, "minimal vertical drift");
    }

    #[test]
    fn speed_capped_at_max() {
        let mut ship = Ship::spawn();
        ship.angle = 0.0;
        for _ in 0..200 {
            ship.thrust();
        }
        let spd = (ship.vx * ship.vx + ship.vy * ship.vy).sqrt();
        assert!(spd <= MAX_SPEED + 1e-9, "speed must not exceed MAX_SPEED");
    }

    #[test]
    fn friction_slows_ship_over_time() {
        let mut ship = Ship::spawn();
        ship.vx = 3.0;
        for _ in 0..30 {
            ship.update();
        }
        assert!(ship.vx < 3.0, "friction must reduce velocity");
    }

    #[test]
    fn brake_reduces_speed() {
        let mut ship = Ship::spawn();
        ship.vx = 3.0;
        let before = (ship.vx * ship.vx + ship.vy * ship.vy).sqrt();
        ship.brake();
        let after = (ship.vx * ship.vx + ship.vy * ship.vy).sqrt();
        assert!(after < before, "brake must reduce speed");
    }

    #[test]
    fn stop_zeroes_velocity() {
        let mut g = bare(1);
        g.ship.vx = 2.5;
        g.ship.vy = -1.8;
        g.ship.stop();
        assert_eq!(g.ship.vx, 0.0);
        assert_eq!(g.ship.vy, 0.0);
    }

    // ── screen wrap ───────────────────────────────────────────────

    #[test]
    fn ship_wraps_past_right_edge() {
        let mut ship = Ship::spawn();
        ship.x = W - 0.1;
        ship.vx = 1.0;
        ship.update();
        assert!(ship.x < 1.0, "ship should wrap to left side");
    }

    #[test]
    fn ship_wraps_past_top_edge() {
        let mut ship = Ship::spawn();
        ship.y = H - 0.1;
        ship.vy = 1.0;
        ship.update();
        assert!(ship.y < 1.0, "ship should wrap to bottom");
    }

    #[test]
    fn bullet_wraps_horizontally() {
        let mut b = Bullet { x: W - 0.1, y: H / 2.0, vx: 1.0, vy: 0.0, life: BULLET_LIFE };
        b.update();
        assert!(b.x < 1.0, "bullet should wrap");
    }

    // ── wdist helper ─────────────────────────────────────────────

    #[test]
    fn wdist_same_point_is_zero() {
        assert!((wdist(10.0, 10.0, 10.0, 10.0)).abs() < 1e-9);
    }

    #[test]
    fn wdist_takes_shorter_wraparound_path() {
        // Two points near opposite edges are closer via wrap than direct
        let d_direct = wdist(1.0, 50.0, W - 1.0, 50.0);
        assert!(d_direct < W / 2.0, "wrapped distance should be ≈ 2, not ≈ 198");
    }

    #[test]
    fn wdist_symmetric() {
        let a = wdist(5.0, 10.0, 190.0, 90.0);
        let b = wdist(190.0, 90.0, 5.0, 10.0);
        assert!((a - b).abs() < 1e-9);
    }

    // ── bullets ───────────────────────────────────────────────────

    #[test]
    fn fire_creates_one_bullet() {
        let mut g = bare(1);
        g.shoot();
        assert_eq!(g.bullets.len(), 1);
    }

    #[test]
    fn bullet_velocity_includes_ship_velocity() {
        // angle=0 → forward is +y; sin(0)=0, cos(0)=1
        // bullet.vx = ship.vx + BULLET_SPEED * sin(0) = ship.vx + 0
        // bullet.vy = ship.vy + BULLET_SPEED * cos(0) = ship.vy + BULLET_SPEED
        let ship = Ship { x: W/2.0, y: H/2.0, vx: 1.0, vy: 0.5, angle: 0.0, inv: 0 };
        let b = ship.fire();
        assert!((b.vx - ship.vx).abs() < 1e-9, "x carries ship velocity, no extra");
        assert!((b.vy - (ship.vy + BULLET_SPEED)).abs() < 1e-6, "vy = ship.vy + BULLET_SPEED");
    }

    #[test]
    fn shoot_cooldown_limits_fire_rate() {
        let mut g = bare(1);
        g.shoot();
        g.shoot(); // immediate second shot
        assert_eq!(g.bullets.len(), 1, "second shot should be rejected by cooldown");
    }

    #[test]
    fn max_four_bullets_in_flight() {
        let mut g = bare(1);
        for _ in 0..10 {
            g.cooldown = 0;
            g.shoot();
        }
        assert_eq!(g.bullets.len(), MAX_BULLETS);
    }

    #[test]
    fn bullet_expires_after_bullet_life_ticks() {
        let mut b = Bullet { x: W / 2.0, y: H / 2.0, vx: 0.0, vy: 0.0, life: BULLET_LIFE };
        for _ in 0..BULLET_LIFE {
            b.update();
            assert!(b.life > 0 || b.life == 0, "life must not underflow");
        }
        b.update(); // one more tick
        assert_eq!(b.life, 0, "bullet should be expired");
    }

    // ── collisions – bullet / asteroid ───────────────────────────

    #[test]
    fn bullet_destroys_large_rock_spawns_two_medium() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Large);
        place_bullet(&mut g, 100.0, 50.0); // on top of rock
        g.bullet_rock_collisions();
        let mediums: Vec<_> = g.rocks.iter().filter(|r| r.size == Size::Medium).collect();
        assert_eq!(mediums.len(), 2, "large → 2 medium");
        assert_eq!(g.bullets.len(), 0, "bullet consumed");
    }

    #[test]
    fn bullet_destroys_medium_rock_spawns_two_small() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Medium);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions();
        let smalls: Vec<_> = g.rocks.iter().filter(|r| r.size == Size::Small).collect();
        assert_eq!(smalls.len(), 2, "medium → 2 small");
    }

    #[test]
    fn bullet_destroys_small_rock_no_spawn() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Small);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions();
        assert!(g.rocks.is_empty(), "small rock leaves nothing");
    }

    #[test]
    fn score_increases_on_large_hit() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Large);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions();
        assert_eq!(g.score, Size::Large.score());
    }

    #[test]
    fn score_increases_on_medium_hit() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Medium);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions();
        assert_eq!(g.score, Size::Medium.score());
    }

    #[test]
    fn score_increases_on_small_hit() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Small);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions();
        assert_eq!(g.score, Size::Small.score());
    }

    #[test]
    fn hi_score_tracks_max() {
        let mut g = bare(0);
        place_rock(&mut g, 100.0, 50.0, Size::Small);
        place_bullet(&mut g, 100.0, 50.0);
        g.bullet_rock_collisions(); // score = 100, hi = 100
        // Reset drops score to 0 but hi stays
        let hi_before = g.hi;
        g.reset();
        assert_eq!(g.hi, hi_before, "hi score survives reset");
        assert_eq!(g.score, 0);
    }

    #[test]
    fn one_bullet_cannot_destroy_two_rocks() {
        let mut g = bare(0);
        place_rock(&mut g, 50.0, 50.0, Size::Large);
        place_rock(&mut g, 50.0, 50.0, Size::Large); // same spot
        place_bullet(&mut g, 50.0, 50.0);
        g.bullet_rock_collisions();
        // Bullet hits first rock; second rock survives (+ 2 medium from split)
        assert!(g.bullets.is_empty());
        // One large was destroyed (→ 2 medium), one large survives
        let large_count = g.rocks.iter().filter(|r| r.size == Size::Large).count();
        assert_eq!(large_count, 1, "second large rock should survive");
    }

    // ── collisions – ship / asteroid ─────────────────────────────

    #[test]
    fn ship_collision_costs_a_life() {
        let mut g = bare(1);
        let (sx, sy) = (g.ship.x, g.ship.y);
        place_rock(&mut g, sx, sy, Size::Large);
        let lives_before = g.lives;
        g.ship_rock_collision();
        assert_eq!(g.lives, lives_before - 1);
    }

    #[test]
    fn ship_collision_respawns_at_centre() {
        let mut g = bare(1);
        g.ship.x = 10.0;
        g.ship.y = 10.0;
        g.lives = 2;
        place_rock(&mut g, 10.0, 10.0, Size::Large);
        g.ship_rock_collision();
        assert!((g.ship.x - W / 2.0).abs() < 1e-9, "ship respawns at centre");
    }

    #[test]
    fn last_life_collision_triggers_game_over() {
        let mut g = bare(1);
        g.lives = 1;
        let (sx, sy) = (g.ship.x, g.ship.y);
        place_rock(&mut g, sx, sy, Size::Large);
        g.ship_rock_collision();
        assert_eq!(g.phase, Phase::Dead);
        assert_eq!(g.lives, 0);
    }

    #[test]
    fn invincible_ship_survives_collision() {
        let mut g = bare(1);
        g.ship.inv = INVINCIBLE; // make invincible
        g.lives = 1;
        let (sx, sy) = (g.ship.x, g.ship.y);
        place_rock(&mut g, sx, sy, Size::Large);
        g.tick(); // tick() only checks collision when inv == 0
        assert_ne!(g.phase, Phase::Dead, "invincible ship must not die");
    }

    // ── level progression ─────────────────────────────────────────

    #[test]
    fn clearing_all_rocks_advances_level() {
        let mut g = bare(1);
        assert_eq!(g.level, 1);
        // rocks is already empty (bare clears it), so next tick → level 2
        g.tick();
        assert_eq!(g.level, 2);
    }

    #[test]
    fn level_2_spawns_more_rocks() {
        let mut g = bare(1);
        g.tick(); // level advances to 2 and spawns a new wave
        let wave2_count = g.rocks.len();
        // wave(2) = 3 + 2 = 5 rocks
        assert_eq!(wave2_count, 5);
    }

    #[test]
    fn level_counter_increments_on_clear() {
        let mut g = bare(1);
        g.tick(); // → level 2, new wave
        g.rocks.clear();
        g.tick(); // → level 3
        assert_eq!(g.level, 3);
    }

    // ── pausing ───────────────────────────────────────────────────

    #[test]
    fn pause_stops_ship_movement() {
        let mut g = Game::new_seeded(1);
        g.phase = Phase::Paused;
        g.ship.vx = 2.0;
        // tick() returns early when paused — rocks are non-empty, no level change
        g.tick();
        assert!((g.ship.vx - 2.0).abs() < 0.01, "paused game must not update physics");
    }

    // ── trace serde ───────────────────────────────────────────────

    #[test]
    fn trace_round_trips_through_json() {
        let trace = Trace {
            seed: 9999,
            events: vec![
                TraceEvent { tick: 0,  action: TraceAction::Reset },
                TraceEvent { tick: 5,  action: TraceAction::KeyDown("left".into()) },
                TraceEvent { tick: 10, action: TraceAction::KeyUp("left".into()) },
                TraceEvent { tick: 15, action: TraceAction::Fire },
                TraceEvent { tick: 20, action: TraceAction::Stop },
                TraceEvent { tick: 25, action: TraceAction::Quit },
            ],
        };
        let json = serde_json::to_string(&trace).unwrap();
        let back: Trace = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seed, trace.seed);
        assert_eq!(back.events.len(), trace.events.len());
        assert_eq!(back.events[1].action, TraceAction::KeyDown("left".into()));
    }

    #[test]
    fn seeded_game_is_deterministic() {
        // Two games with the same seed must have identical asteroid positions.
        let g1 = Game::new_seeded(12345);
        let g2 = Game::new_seeded(12345);
        assert_eq!(g1.rocks.len(), g2.rocks.len());
        for (r1, r2) in g1.rocks.iter().zip(g2.rocks.iter()) {
            assert!((r1.x - r2.x).abs() < 1e-9);
            assert!((r1.y - r2.y).abs() < 1e-9);
        }
    }

    #[test]
    fn different_seeds_produce_different_layouts() {
        let g1 = Game::new_seeded(1);
        let g2 = Game::new_seeded(2);
        // Extremely unlikely to be identical
        let same = g1.rocks.iter().zip(g2.rocks.iter())
            .all(|(r1, r2)| (r1.x - r2.x).abs() < 1e-9);
        assert!(!same, "different seeds should produce different layouts");
    }
}
