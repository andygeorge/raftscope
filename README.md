# raftscope

A browser visualization of the [Raft consensus algorithm](https://raft.github.io/),
inspired by [The Secret Lives of Data](http://thesecretlivesofdata.com/raft/).
Originally vanilla JS; now a **Rust → WebAssembly** rewrite that renders a
live SVG via `web-sys` (no UI framework).

Still a decent space heater while your browser re-renders SVG at 60fps.

## Run it

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk

trunk serve   # dev server with hot reload at http://localhost:8080
```

```sh
trunk build --release   # production bundle into dist/
cargo test              # pure protocol/state tests, no browser needed
```

## Controls

Drag the **timeline** slider to scrub through history; the **speed** slider
sets the simulation rate. Click a server or in-flight message for details,
right-click for actions. Keyboard shortcuts:

| Key | Action |
|-----|--------|
| `space` / `.` | pause / resume |
| `c` | client request to the leader |
| `r` | restart the leader |
| `t` | spread election timers (avoid split vote) |
| `a` | align election timers (force split vote) |
| `l` | set up a log-replication scenario |
| `b` | resume all servers |
| `f` | fork playback, discarding the future |
| `?` | help |

## Architecture

Three layers (ports of the original `raft.js` / `state.js` / `script.js`):

- **`src/raft.rs`** — the Raft protocol, pure Rust. `Model::update` is one tick.
- **`src/state.rs`** — time-travel checkpoint history behind the timeline slider.
- **`src/lib.rs`** — all DOM/SVG rendering and input handling via `web-sys`.

See `CLAUDE.md` for the gotchas worth knowing before you touch the code.

## Deploy

Pushing to `main` triggers `.github/workflows/deploy.yml`, which builds with
Trunk and publishes to GitHub Pages
([andygeorge.github.io/raftscope](https://andygeorge.github.io/raftscope/)).
Set **Settings → Pages → Source** to **GitHub Actions** once to enable it.
