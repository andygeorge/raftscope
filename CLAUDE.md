# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A browser-based visualization of the Raft consensus algorithm (inspired by "The Secret Lives of Data"). Originally vanilla JS + jQuery + Bootstrap 3; now a full **Rust → WebAssembly** rewrite that renders a live-updating SVG via raw `web-sys` DOM calls (no UI framework). Bootstrap 3.1.1 CSS and `style.css` are kept for visual identity; all JavaScript dependencies (jQuery, Bootstrap JS, bootstrap-slider, bootstrap-contextmenu) are gone.

## Running it

This is a Trunk-bundled WASM app, not a static-open-in-browser site.

```
rustup target add wasm32-unknown-unknown
cargo install trunk        # or use a prebuilt trunk binary

trunk serve                # dev server with hot reload
trunk build --release      # production bundle into dist/
```

`index.html` is the Trunk entry: SVG skeleton (defs/markers, `#ring`, `#pause`, `#messages`, `#servers`, `.logs`), native range inputs (`#time`, `#speed`), modals (`#modal-details`, `#modal-help`), `#context-menu`, and `<link data-trunk rel="rust" />` to pull in the WASM. Bootstrap CSS and `style.css` are linked both via `data-trunk rel="copy-*"` (for the bundle) and plain `<link>` (for dev).

`cargo test` runs the pure protocol/state layers natively (no browser needed) — see the cfg-gated `util::random`.

## Architecture

Three layers, deliberately separated (ports of the original `raft.js`/`state.js`/`script.js`):

1. **`src/raft.rs`** — the Raft protocol logic, pure Rust, no `web-sys`. Operates on a `Model { time, servers, messages }`. Protocol behavior lives in the periodically-applied state-machine rules (`start_new_election`, `become_leader`, `send_request_vote`, `send_append_entries`, `advance_commit_index`) and the message handlers. `Model::update(&mut self)` is the single tick: it applies every rule to every server, then delivers any messages whose `recv_time <= time`. User-triggered actions (`stop`, `resume`, `resume_all`, `restart`, `timeout`, `client_request`, `drop`, plus demo helpers like `setup_log_replication_scenario`) also live here. Ships `#[cfg(test)] mod tests`.

2. **`src/state.rs`** (`State`) — time-travel layer, pure Rust. Wraps the model in a checkpoint history so the timeline slider can scrub backward (`rewind`) and forward (`advance`/`seek`). `fork()` discards the future from the current point (used before every user action so interactions branch cleanly). `export_to_string`/`import_from_string` serialize checkpoints via serde — ported but not yet wired to the UI (`#[allow(dead_code)]`). The JS `state.updater` hook is inlined as `State::run_update`: it calls `Model::update` and decides whether the model changed enough (ignoring `time`) to warrant a new checkpoint.

3. **`src/lib.rs`** — everything DOM/SVG via `web-sys`. The `render_*` functions (`render_servers`, `render_messages`, `render_logs`, `render_clock`) imperatively redraw the SVG from `state.current`. A `requestAnimationFrame` loop (the `step` function) advances model time by wall-time-elapsed divided by the speed factor, then re-renders. Keyboard shortcuts and context menus map to the `raft` actions. The app lives in a `thread_local APP: RefCell<Option<App>>` cell, accessed via `with_app()`; `App` retains event-handler `Closure`s so they outlive attachment.

### Things that will bite you

- **Time is `f64` microseconds.** All the constants in `raft.rs` (`RPC_TIMEOUT`, `ELECTION_TIMEOUT`, latencies) and `Model::time` are microsecond values. The UI divides by `1e6` to show seconds. `util::INF` (`1e300`, not `f64::INFINITY`) is used for "never" because `serde_json` serializes `Infinity` to `null`, which would corrupt checkpoint serialization.

- **SVG `className` is read-only.** It is an `SVGAnimatedString`; calling `set_class_name` on an SVG element throws. Always set classes with `set_attr(&node, "class", ...)`.

- **Peer maps are `BTreeMap<u32, T>`**, keyed by 1-based peer id. `BTreeMap` (not `HashMap`) keeps iteration order deterministic, which keeps `PartialEq`-based checkpoint diffing stable.

- **Protocol rules operate by server index**, e.g. `fn rule(model: &mut Model, i: usize)`, not by holding a `&mut Server` — so a rule can read peers while mutating one server without fighting the borrow checker.

- **`NUM_SERVERS` is fixed at 5** and baked into both protocol math (majority = `NUM_SERVERS/2 + 1`) and SVG layout (servers placed around a ring via `util::circle_coord`).

- **The speed slider is logarithmic.** `speed_transform` does `max(1, 10^v)`; the displayed value is "1/Nx" (slower than real time). The slider is reversed (`direction: rtl`).

- **Every user action follows the same dance:** `state.fork()` → mutate via a `raft` function → `state.save()` → re-render. Preserve this ordering when adding new interactions, or the timeline history will desync.

- **`random()` is cfg-gated.** `js_sys::Math::random` on `wasm32`, a `thread_local` xorshift on the host so `cargo test` is deterministic without a browser.
