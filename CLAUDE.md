# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A browser-based visualization of the Raft consensus algorithm (inspired by "The Secret Lives of Data"). Pure static site: vanilla JS + jQuery + Bootstrap 3, rendered as a live-updating SVG. No build step, no package manager, no test suite, no server.

## Running it

Open `index.html` in a browser. That's it. Scripts load in dependency order (see `index.html`): `util.js` → `raft.js` → `state.js` → `script.js`.

Dependencies are git submodules. After cloning:
```
git submodule update --init --recursive
```
This pulls `bootstrap-slider` and `bootstrap-contextmenu`. `jquery/` and `bootstrap-3.1.1/` are vendored directly.

Code style is enforced informally via jshint directives at the top of each file (`'use strict'`, browser/jquery globals). There is no linter wired up to run.

## Architecture

Three layers, deliberately separated:

1. **`raft.js`** — the Raft protocol logic, pure and UI-agnostic. Operates on a `model` (`{servers, messages, time}`). All protocol behavior lives in `raft.rules.*` (the periodically-applied state-machine rules: `startNewElection`, `becomeLeader`, `sendRequestVote`, `sendAppendEntries`, `advanceCommitIndex`) and the message handlers. `raft.update(model)` is the single tick: it applies every rule to every server, then delivers any messages whose `recvTime <= model.time`. User-triggered actions (`raft.stop`, `resume`, `restart`, `timeout`, `clientRequest`, `drop`, plus demo helpers like `setupLogReplicationScenario`) also live here.

2. **`state.js`** (`makeState`) — time-travel layer. Wraps the model in a checkpoint history so the timeline slider can scrub backward (`rewind`) and forward (`advance`/`seek`). `fork()` discards the future from the current point (used before every user action so interactions branch cleanly). State can be serialized via `exportToString`/`importFromString` (used by the `record`/`replay` localStorage helpers in `script.js`). The hook between layers is `state.updater`, defined at the bottom of `script.js`: it calls `raft.update` and decides whether the model changed enough to warrant a new checkpoint.

3. **`script.js`** — everything DOM/SVG. The `render.*` functions (`servers`, `messages`, `logs`, `clock`) redraw the SVG from `state.current`. A `requestAnimationFrame` loop (the `step` function) advances model time by wall-time-elapsed divided by the speed factor, then re-renders. Keyboard shortcuts and context menus map to the `raft.*` actions. `playback` manages pause/resume.

### Things that will bite you

- **Time is in microseconds.** All the constants in `raft.js` (`RPC_TIMEOUT`, `ELECTION_TIMEOUT`, latencies) and `model.time` are microsecond values. The UI divides by `1e6` to show seconds. `util.Inf` (`1e300`, not `Infinity`) is used for "never" because `JSON.stringify(Infinity)` is `null`, which would break checkpoint serialization.

- **Rendering is checkpoint-diffed.** `render.update` compares `state.current` against `state.base()` (the last checkpoint) via `util.equals` to skip redundant server/message redraws (`serversSame`/`messagesSame`). If you add fields to the model that should trigger a redraw, make sure `util.equals` sees them.

- **`NUM_SERVERS` is fixed at 5** and baked into both protocol math (majority = `Math.floor(NUM_SERVERS/2)+1`) and SVG layout (servers placed around a ring via `util.circleCoord`).

- **The speed slider is logarithmic.** `speedSliderTransform` does `10^v`; the displayed value is "1/Nx" (slower than real time). The slider is reversed.

- **Every user action follows the same dance:** `state.fork()` → mutate via a `raft.*` function → `state.save()` → `render.update()`. Preserve this ordering when adding new interactions, or the timeline history will desync.
