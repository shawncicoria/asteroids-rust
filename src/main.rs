// Asteroids – classic 1980s arcade game in a Rust TUI
// Controls: Arrow keys or WASD to steer/thrust, SPACE to fire, P to pause, Q to quit

use std::{
    f64::consts::PI,
    io,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use rand::Rng;
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

#[derive(PartialEq, Eq)]
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
    thrust_on: bool, // used only by renderer for exhaust flame
    left: bool,
    right: bool,
    up: bool,
    rng: rand::rngs::ThreadRng,
}

impl Game {
    fn new() -> Self {
        let mut rng = rand::thread_rng();
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
            left: false,
            right: false,
            up: false,
            rng,
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
        self.phase = Phase::Playing;
    }

    fn respawn(&mut self) {
        self.ship = Ship::spawn();
        self.bullets.clear();
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

        if self.left {
            self.ship.rotate(-1.0);
        }
        if self.right {
            self.ship.rotate(1.0);
        }
        self.thrust_on = self.up;
        if self.up {
            self.ship.thrust();
        }
        self.cooldown = self.cooldown.saturating_sub(1);

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

                    // Exhaust flame when thrusting
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
                    ctx.print(30.0, 50.0,
                        "Arrows/WASD: rotate & thrust     SPACE: fire     P: pause     Q: quit");
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

// ─────────────────────────── main ────────────────────────────────

fn main() -> io::Result<()> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(out))?;

    let result = run_loop(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    let mut game = Game::new();
    let frame_time = Duration::from_millis(16); // ~60 fps
    let mut last = Instant::now();

    loop {
        terminal.draw(|f| draw(f, &game))?;

        let wait = frame_time.saturating_sub(last.elapsed());
        if event::poll(wait)? {
            if let Event::Key(key) = event::read()? {
                match key.kind {
                    KeyEventKind::Press | KeyEventKind::Repeat => match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(()),

                        KeyCode::Enter | KeyCode::Char(' ')
                            if matches!(game.phase, Phase::Title | Phase::Dead) =>
                        {
                            game.reset();
                        }

                        KeyCode::Char('p') | KeyCode::Char('P') => match game.phase {
                            Phase::Playing => game.phase = Phase::Paused,
                            Phase::Paused => game.phase = Phase::Playing,
                            _ => {}
                        },

                        KeyCode::Left | KeyCode::Char('a') | KeyCode::Char('A') => {
                            game.left = true
                        }
                        KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('D') => {
                            game.right = true
                        }
                        KeyCode::Up | KeyCode::Char('w') | KeyCode::Char('W') => {
                            game.up = true
                        }
                        KeyCode::Char(' ') => game.shoot(),
                        _ => {}
                    },

                    KeyEventKind::Release => match key.code {
                        KeyCode::Left | KeyCode::Char('a') | KeyCode::Char('A') => {
                            game.left = false
                        }
                        KeyCode::Right | KeyCode::Char('d') | KeyCode::Char('D') => {
                            game.right = false
                        }
                        KeyCode::Up | KeyCode::Char('w') | KeyCode::Char('W') => {
                            game.up = false
                        }
                        _ => {}
                    },
                }
            }
        }

        if last.elapsed() >= frame_time {
            game.tick();
            last = Instant::now();
        }
    }
}
