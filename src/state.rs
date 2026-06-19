//! Time-travel layer over a [`Model`]. Wraps the model in a checkpoint history
//! so the timeline slider can scrub backward (`rewind`) and forward
//! (`advance`/`seek`). `fork` discards the future from the current point.
//! Ported from state.js. The (unused) timer-scheduling feature is omitted.

use crate::raft::Model;
use crate::util::greatest_lower;
use serde::{Deserialize, Serialize};

#[allow(dead_code)] // serialization API kept from state.js; not yet wired to UI
#[derive(Serialize, Deserialize)]
struct Export {
    checkpoints: Vec<Model>,
    max_time: f64,
}

pub struct State {
    pub current: Model,
    checkpoints: Vec<Model>,
    max_time: f64,
    #[allow(dead_code)] // used by clear()
    initial: Model,
}

impl State {
    pub fn new(mut initial: Model) -> State {
        initial.time = 0.0;
        State {
            current: initial.clone(),
            checkpoints: Vec::new(),
            max_time: 0.0,
            initial,
        }
    }

    pub fn get_max_time(&self) -> f64 {
        self.max_time
    }

    /// Index of the latest checkpoint at or before `time`.
    fn prev(&self, time: f64) -> usize {
        let i = greatest_lower(&self.checkpoints, |m| m.time > time);
        if i < 0 {
            0
        } else {
            i as usize
        }
    }

    pub fn base(&self) -> &Model {
        &self.checkpoints[self.prev(self.current.time)]
    }

    pub fn init(&mut self) {
        self.checkpoints.push(self.current.clone());
    }

    pub fn fork(&mut self) {
        let i = self.prev(self.current.time);
        while self.checkpoints.len() > i + 1 {
            self.checkpoints.pop();
        }
        self.max_time = self.current.time;
    }

    pub fn rewind(&mut self, time: f64) {
        let i = self.prev(time);
        self.current = self.checkpoints[i].clone();
        self.current.time = time;
    }

    pub fn save(&mut self) {
        self.checkpoints.push(self.current.clone());
    }

    pub fn advance(&mut self, time: f64) {
        self.max_time = time;
        self.current.time = time;
        if self.run_update() {
            self.checkpoints.push(self.current.clone());
        }
    }

    /// The single `state.updater` from script.js, inlined: tick the model, then
    /// report whether it changed enough (ignoring `time`) to warrant a new
    /// checkpoint.
    fn run_update(&mut self) -> bool {
        self.current.update();
        let idx = self.prev(self.current.time);
        let saved = self.current.time;
        self.current.time = self.checkpoints[idx].time;
        let same = self.current == self.checkpoints[idx];
        self.current.time = saved;
        !same
    }

    pub fn seek(&mut self, time: f64) {
        if time <= self.max_time {
            self.rewind(time);
        } else {
            self.advance(time);
        }
    }

    #[allow(dead_code)]
    pub fn export_to_string(&self) -> String {
        serde_json::to_string(&Export {
            checkpoints: self.checkpoints.clone(),
            max_time: self.max_time,
        })
        .unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn import_from_string(&mut self, s: &str) {
        if let Ok(o) = serde_json::from_str::<Export>(s) {
            self.checkpoints = o.checkpoints;
            self.max_time = o.max_time;
            if let Some(first) = self.checkpoints.first() {
                self.current = first.clone();
            }
            self.current.time = 0.0;
        }
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.checkpoints.clear();
        self.current = self.initial.clone();
        self.current.time = 0.0;
        self.max_time = 0.0;
    }
}
