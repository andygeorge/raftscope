# raftscope Rust/WASM Rewrite — Design Spec

**Date**: 2026-06-19  
**Author**: clod (for randy)  
**Status**: Approved

---

## Overview

Full rewrite of raftscope (browser-based Raft consensus visualizer) from vanilla JS/jQuery/Bootstrap to Rust compiled to WebAssembly, using Leptos (CSR mode) + Trunk. The 3-layer architecture (protocol logic → time-travel state → rendering) is preserved and strengthened.

---

## Goals

- Preserve all existing functionality: Raft simulation, time-travel slider, speed slider, server/message modals, context menus, keyboard shortcuts, localStorage record/replay
- Drop all JS dependencies (jQuery, Bootstrap JS, bootstrap-slider, bootstrap-contextmenu)
- Pure Rust core (protocol + state) with no WASM surface — unit testable with `cargo test`
- Same visual appearance (SVG ring, message arcs, log visualization, term colors)

## Non-Goals

- Adding new Raft features or protocol variants
- Server-side rendering
- Mobile-first redesign
- Changing the fixed 5-server layout

---

## Technology Stack

| Concern | Choice | Rationale |
|---------|--------|-----------|
| Compile target | `wasm32-unknown-unknown` | Browser execution |
| UI framework | **Leptos (CSR)** | Fine-grained reactivity maps cleanly to model fields; SVG first-class in RSX templates; no VDOM overhead for 60fps updates |
| Build tool | **Trunk** | Zero-config WASM bundling; replaces "open index.html" with `trunk serve` |
| Async/timers | **gloo** | RAF loop, setTimeout, localStorage — idiomatic WASM bindings |
| Serialization | **serde + serde_json** | Checkpoint export/import (localStorage), same as current JSON.stringify |
| CSS | Bootstrap 5 via CDN + minimal custom CSS | Keep visual identity; drop Bootstrap JS entirely |
| Randomness | **getrandom** (WASM feature) | `rand` crate with WASM target support |

---

## Architecture

### File Structure

```
src/
  main.rs              — Trunk entrypoint; mounts Leptos app
  raft.rs              — Pure Rust Raft protocol (no WASM deps)
  state.rs             — Time-travel checkpoint system
  util.rs              — circle_coord, greatest_lower, clamp, arc_spec
  components/
    app.rs             — Root component; global RwSignal<SimState>; RAF loop
    servers.rs         — SVG server circles, election arcs, vote indicators
    messages.rs        — SVG in-flight messages with animated positions
    logs.rs            — Log entry visualization
    clock.rs           — Time slider + speed slider
    modals.rs          — Server modal, message modal, help modal
    context_menu.rs    — Right-click context menu (custom Rust component)
Cargo.toml
index.html             — Trunk entry; links Bootstrap CSS
```

### Layer Separation

```
┌─────────────────────────────────────────────────────┐
│  src/raft.rs  — pure Rust, no web-sys, no leptos    │
│  Model { servers: [Server; 5], messages: Vec<Msg>,  │
│           time: u64 }                               │
│  raft::update(model: &mut Model)                    │
│  raft::stop/resume/restart/timeout/client_request   │
└──────────────────┬──────────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────────┐
│  src/state.rs  — pure Rust, no web-sys              │
│  SimState { current: Model, checkpoints: Vec<Model>,│
│             max_time: u64 }                         │
│  fn fork(), seek(), save(), advance(), rewind()     │
│  fn export_to_string() / import_from_string()       │
└──────────────────┬──────────────────────────────────┘
                   │  RwSignal<SimState>
┌──────────────────▼──────────────────────────────────┐
│  src/components/  — Leptos, web-sys, gloo           │
│  Reads signal → produces SVG view! macro output     │
│  RAF loop: wall-time → model-time → signal.update() │
└─────────────────────────────────────────────────────┘
```

---

## Data Structures (Rust)

### Core Types (`raft.rs`)

```rust
const NUM_SERVERS: usize = 5;
const RPC_TIMEOUT: u64 = 50_000;        // microseconds
const MIN_RPC_LATENCY: u64 = 10_000;
const MAX_RPC_LATENCY: u64 = 15_000;
const ELECTION_TIMEOUT: u64 = 100_000;
const BATCH_SIZE: usize = 1;
const INF: u64 = u64::MAX / 2;         // "never" sentinel (serializable)

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub term: u64,
    pub value: String,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ServerState { Follower, Candidate, Leader, Stopped }

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Server {
    pub id: usize,                        // 1-5
    pub peers: Vec<usize>,
    pub state: ServerState,
    pub term: u64,
    pub voted_for: Option<usize>,
    pub log: Vec<LogEntry>,
    pub commit_index: usize,
    pub election_alarm: u64,              // microseconds
    pub vote_granted: [bool; NUM_SERVERS],
    pub match_index: [usize; NUM_SERVERS],
    pub next_index: [usize; NUM_SERVERS],
    pub rpc_due: [u64; NUM_SERVERS],
    pub heartbeat_due: [u64; NUM_SERVERS],
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MessageDirection { Request, Reply }

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MessageType { RequestVote, AppendEntries }

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub direction: MessageDirection,
    pub msg_type: MessageType,
    pub from: usize,
    pub to: usize,
    pub send_time: u64,
    pub recv_time: u64,
    pub term: u64,
    // RequestVote request
    pub last_log_index: Option<usize>,
    pub last_log_term: Option<u64>,
    // RequestVote reply
    pub granted: Option<bool>,
    // AppendEntries request
    pub prev_index: Option<usize>,
    pub prev_term: Option<u64>,
    pub entries: Option<Vec<LogEntry>>,
    pub leader_commit: Option<usize>,
    // AppendEntries reply
    pub success: Option<bool>,
    pub match_index_reply: Option<usize>,
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub time: u64,
    pub servers: Vec<Server>,   // always len 5
    pub messages: Vec<Message>,
}
```

**Note**: Peer maps (`voteGranted`, `matchIndex`, etc.) use fixed-size arrays `[T; NUM_SERVERS]` indexed by `peer_id - 1` (server IDs are 1-based; server 1 → index 0). The slot for a server's own ID is allocated but never written by that server. This avoids HashMap overhead and is cache-friendly.

### State Types (`state.rs`)

```rust
pub struct SimState {
    pub current: Model,
    pub checkpoints: Vec<Model>,
    pub max_time: u64,
}

impl SimState {
    pub fn fork(&mut self)
    pub fn seek(&mut self, time: u64)
    pub fn save(&mut self)
    pub fn advance(&mut self, time: u64) -> bool   // returns true if checkpoint created
    pub fn rewind(&mut self, time: u64)
    pub fn base(&self) -> &Model
    pub fn export_to_string(&self) -> String
    pub fn import_from_string(&mut self, s: &str)
}
```

---

## Rendering Design

### Reactive Signal

```rust
// In app.rs
let sim = create_rw_signal(SimState::new());
provide_context(sim);  // available to all child components
```

All components read `use_context::<RwSignal<SimState>>()` and derive their SVG output reactively. The RAF loop calls `sim.update(|s| s.seek(new_time))`.

### RAF Loop

```rust
// gloo::render::request_animation_frame
let last_ts = store_value(0.0f64);
let closure = Closure::wrap(Box::new(move |timestamp: f64| {
    let last = last_ts.get_value();
    if !playback_paused.get() && last > 0.0 && timestamp - last < 500.0 {
        let wall_micros = ((timestamp - last) * 1000.0) as u64;
        let speed = speed_slider.get();  // 10^v
        let model_micros = (wall_micros as f64 / speed) as u64;
        sim.update(|s| s.seek(s.current.time + model_micros));
    }
    last_ts.set_value(timestamp);
}) as Box<dyn FnMut(f64)>);
```

### SVG Layout Constants

Same as JS:
```rust
const RING_CX: f64 = 210.0;
const RING_CY: f64 = 210.0;
const RING_R: f64 = 150.0;
const LOGS_X: f64 = 430.0;
const LOGS_Y: f64 = 50.0;
```

### Term Colors

```rust
const TERM_COLORS: [&str; 6] = [
    "#66c2a5", "#fc8d62", "#8da0cb",
    "#e78ac3", "#a6d854", "#ffd92f",
];
```

### Component Breakdown

**`<Servers />`**: Reads signal. For each server: circle with term-colored fill, arc path for election timeout, vote indicator dots (if candidate), click handler opening server modal.

**`<Messages />`**: Reads signal. For each message: circle interpolated along line from `circle_coord(from)` to `circle_coord(to)` at `frac = (time - send_time) / (recv_time - send_time)`. Arrows visible only when paused.

**`<Logs />`**: Reads signal. For each server: row of colored log-entry boxes. For leader: `matchIndex` circles and `nextIndex` arrows per follower.

**`<Clock />`**: Two `<input type="range">` elements. Time slider driven by `sim.with(|s| s.current.time)`. Speed slider uses `leptos_use` or manual event binding for logarithmic transform.

**`<ServerModal />`**: Hidden div, shown via signal. Displays all server fields, peer table, action buttons.

**`<MessageModal />`**: Similar, shows typed message fields.

**`<ContextMenu />`**: Custom `<ul>` positioned at cursor, shown on `contextmenu` event. Actions: stop, resume, restart, timeout, request (servers); drop (messages).

---

## User Interactions

All follow the same pattern as JS:
```rust
sim.update(|s| {
    s.fork();
    raft::some_action(&mut s.current, ...);
    s.save();
});
// Leptos re-renders automatically (signal changed)
```

### Keyboard Shortcuts

`gloo::events::EventListener` on `window` for `keydown`. Same keys as JS version:

| Key | Action |
|-----|--------|
| Space / `.` | Toggle playback pause |
| `?` | Show help modal |
| `C` | Client request to leader |
| `R` | Restart leader |
| `T` | Spread timers |
| `A` | Align timers |
| `L` | Setup log replication scenario |
| `B` | Resume all |
| `F` | Fork |

---

## Checkpointing

`SimState::advance()` calls `raft::update(&mut self.current)`, then compares result against `self.base()` using `PartialEq`. If changed, `self.checkpoints.push(self.current.clone())`. Binary search via `greatest_lower_bound` (same as `util.greatestLower` in JS) for `rewind`.

Memory: checkpoints store full model clones. With 5 servers + peer maps as arrays, each model snapshot is ~2-3KB. At ~60 checkpoints/second (one per changed tick), that's ~180KB/second of checkpoint history — acceptable.

---

## Serialization / Record-Replay

```rust
// Export: serialize checkpoints + max_time to JSON, store in localStorage
fn export_to_string(&self) -> String {
    serde_json::to_string(&ExportedState { 
        checkpoints: &self.checkpoints, 
        max_time: self.max_time 
    }).unwrap()
}

// Import: parse and restore
fn import_from_string(&mut self, s: &str) {
    let exported: ExportedState = serde_json::from_str(s).unwrap();
    self.checkpoints = exported.checkpoints;
    self.max_time = exported.max_time;
    self.current = self.checkpoints.last().cloned().unwrap_or_default();
}
```

`gloo::storage::LocalStorage` replaces direct `localStorage` access.

---

## Build / Dev Workflow

```bash
# Install once
cargo install trunk
rustup target add wasm32-unknown-unknown

# Dev server (hot reload)
trunk serve

# Production build
trunk build --release
```

`index.html` becomes the Trunk entry point:
```html
<!DOCTYPE html>
<html>
<head>
  <link data-trunk rel="css" href="style.css" />
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/...bootstrap.min.css" />
</head>
<body>
  <!-- Trunk injects WASM loader here -->
</body>
</html>
```

---

## Testing

`cargo test` runs the pure Rust layers (`raft.rs`, `state.rs`, `util.rs`) natively without WASM:

- Protocol rules: verify state transitions, message generation, quorum math
- Step-down logic: term comparisons, state resets
- Checkpoint: fork/seek/rewind round-trips
- Serialization: export → import → equality

No browser tests in scope for this rewrite.

---

## Migration Notes

- JS `util.Inf` (`1e300`) → Rust `INF: u64 = u64::MAX / 2`. No JSON serialization issue since we use serde with u64.
- JS peer maps (objects keyed by peer ID) → fixed arrays indexed by `peer_id - 1`.
- JS `util.equals` (deep equality) → `#[derive(PartialEq)]` on all model types.
- JS `util.clone` (jQuery deep clone) → `#[derive(Clone)]`.
- JS `Math.random()` → `rand::thread_rng()` with `getrandom` WASM backend.
- JS `JSON.stringify/parse` → `serde_json::to_string / from_str`.
- JS `requestAnimationFrame` → `gloo::render::request_animation_frame` or raw `web_sys::window().request_animation_frame()`.
