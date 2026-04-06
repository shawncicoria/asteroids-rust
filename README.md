# Asteroids

A faithful recreation of the classic 1980s vector-graphics arcade game, running entirely in your terminal using Rust and a TUI (text-user-interface) renderer.

```
 * A S T E R O I D S *

    SCORE 002350   HI 004100   ♦ ♦ ♦   LEVEL 3

         /\
        /  \          ◇ ◇ ◇
       / ·· \
      /______\     ◇       ◇

          ·  ·  ·  ·  ·  ·  (bullet)
```

Ships rotate and thrust with Newtonian physics, asteroids split into smaller fragments when shot, and the field wraps toroidally (objects that leave one edge reappear on the opposite side).

---

## Requirements

| Tool | Version |
|------|---------|
| Rust + Cargo | 1.70+ (stable) |
| A terminal with 80×24 or larger | xterm, iTerm2, Windows Terminal, etc. |

---

## Running

```bash
git clone https://github.com/shawncicoria/asteroids-rust
cd asteroids-rust
cargo run --release --bin asteroids
```

### Controls

| Key | Action |
|-----|--------|
| `←` / `A`  | Rotate left |
| `→` / `D`  | Rotate right |
| `↑` / `W`  | Thrust forward |
| `↓` / `S`  | Brake (retro-fire) |
| `X`         | Full stop (zero velocity instantly) |
| `Space`     | Fire |
| `P`         | Pause / resume |
| `Q`         | Quit |

Arrow keys and WASD are fully interchangeable. The game runs at ~60 fps; smooth control requires a terminal that sends key-repeat events while a key is held (all common modern terminals do).

### Scoring

| Target | Points |
|--------|--------|
| Large asteroid  | 20 |
| Medium asteroid | 50 |
| Small asteroid  | 100 |

Each level adds one extra large asteroid to the opening wave (Level 1 → 4 rocks, Level 2 → 5, …).

---

## Trace Recording and Replay

The game includes a built-in input recorder that captures every action alongside the RNG seed used for that session. Recorded traces can be replayed at any speed — useful for debugging, regression testing, or reviewing a run in slow motion.

### Record a session

```bash
cargo run --release --bin asteroids -- --trace my_run.json
```

Play normally. When you press **Q** the complete session is written to `my_run.json`.

### Replay a session

```bash
cargo run --release --bin asteroids -- --replay my_run.json
```

Because the RNG seed is captured, asteroid positions and split trajectories are identical to the original run.

### Replay in slow motion

```bash
# Half speed
cargo run --release --bin asteroids -- --replay my_run.json --speed 0.5

# Quarter speed (good for inspecting collision frames)
cargo run --release --bin asteroids -- --replay my_run.json --speed 0.25

# Double speed
cargo run --release --bin asteroids -- --replay my_run.json --speed 2.0
```

`--speed` is a multiplier: `1.0` = real time, `0.5` = half speed, `2.0` = double speed.

### Trace file format

Trace files are plain JSON and human-readable:

```json
{
  "seed": 13516843298451823104,
  "events": [
    { "tick": 0,  "action": { "type": "Reset" } },
    { "tick": 12, "action": { "type": "KeyDown", "key": "right" } },
    { "tick": 18, "action": { "type": "KeyUp",   "key": "right" } },
    { "tick": 25, "action": { "type": "Fire" } },
    { "tick": 60, "action": { "type": "Quit" } }
  ]
}
```

You can hand-edit a trace to construct a minimal reproduction of any scenario, then feed it directly into a unit test.

---

## Tests

```bash
cargo test
```

43 unit tests cover game logic without starting the TUI:

- **Ship physics** – rotation, angle wrapping, thrust direction, angle-dependent thrust, speed cap, friction, brake, full stop
- **Screen wrapping** – ship, bullets, and `wdist` wraparound distance calculations
- **Bullets** – firing, velocity inheritance from ship, cooldown, 4-bullet cap, expiry
- **Collisions** – large → 2 medium → 2 small split chain; one bullet hits exactly one asteroid; per-size scoring; hi-score persistence across resets
- **Ship death** – life loss, respawn at centre, game-over on last life, invincibility window
- **Level progression** – clearing the field advances the level; wave size scales with level
- **Determinism** – same seed produces identical asteroid layouts; different seeds differ
- **Trace serialisation** – JSON round-trip for all action variants

Run a single named test:

```bash
cargo test bullet_destroys_large
```

### Writing a test from a trace

1. Record a session that demonstrates the bug or scenario:
   ```bash
   cargo run --bin asteroids -- --trace repro.json
   ```
2. Trim the trace file down to the minimum events that reproduce the issue.
3. Add a test that creates a seeded game, injects the same events tick-by-tick, and asserts the expected outcome. Use `Game::new_seeded(seed)` with the seed from the trace file.

---

## Architecture

All game logic lives in `src/main.rs` (single-file for now):

| Section | What it does |
|---------|-------------|
| `Size`, `Rock` | Asteroid data, random irregular polygon generation, toroidal update |
| `Ship` | Newtonian physics: rotate, thrust, brake, stop, fire, triangle vertices |
| `Bullet`, `Particle` | Projectile and explosion-spark lifetimes |
| `Game` | Central state machine: tick loop, collision detection, level management |
| `wdist`, `kill`, `wave` | Pure helper functions (testable without game state) |
| `TraceAction`, `Trace`, `Recorder` | Serialisable input events, recording, replay |
| `draw` | ratatui canvas rendering (Braille characters for sub-cell resolution) |
| `handle_key`, `run_loop`, `run_replay` | Event loop, key-hold timestamp logic, replay engine |
| `#[cfg(test)] mod tests` | 43 unit tests |

### Key design decisions

**Key-hold timestamps instead of booleans** — direction keys store `Option<Instant>` rather than `bool`. Each press/repeat refreshes the timestamp; `tick()` considers a key held while its timestamp is < 100 ms old. This works correctly on terminals that never send key-release events (the majority of terminals outside kitty/WezTerm).

**Seeded RNG** — `Game::new_seeded(u64)` uses `StdRng` so any run can be exactly reproduced given its seed. The production binary generates a random seed from OS entropy via `rand::random()`.

**Toroidal space** — positions wrap with `rem_euclid`; collision distances use the shortest path across any wrap boundary so objects near opposite edges still interact correctly.

---

## Contributing

1. **Fork and branch** off `main`.  Branch names should describe the change: `fix/bullet-wrap`, `feat/hyperspace`, etc.
2. **Keep game logic and rendering separate.** The `Game` struct and its methods should never import ratatui or crossterm. Tests must be able to construct and tick a `Game` without a terminal.
3. **Test everything tickable.** If new behaviour can be exercised by calling `game.tick()` or a `Ship`/`Rock` method directly, add a `#[cfg(test)]` test for it. Aim to keep the test suite at zero failures and zero warnings.
4. **Record a trace for complex changes.** If you're changing physics constants or collision logic, record a before/after trace and include both in the PR description so reviewers can replay and compare.
5. **Run before opening a PR:**
   ```bash
   cargo test
   cargo clippy -- -D warnings
   cargo fmt --check
   ```
6. **Commit messages** — one short subject line (≤ 72 chars), then a body explaining *why*, not just what. Reference any relevant trace files or test names.

---

## License

MIT — see [LICENSE](LICENSE).
