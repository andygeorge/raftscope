//! The Raft protocol logic, pure and UI-agnostic. Ported from raft.js.
//!
//! Everything operates on a [`Model`]. Rules and message handlers take the
//! model plus a *server index* (rather than a `&mut Server`) so that we can
//! mutate a server's fields and push to `model.messages` in the same call
//! without tripping the borrow checker — the JS relies on shared mutation
//! that Rust forbids.

use crate::util::{self, INF};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RPC_TIMEOUT: f64 = 50000.0;
pub const MIN_RPC_LATENCY: f64 = 10000.0;
pub const MAX_RPC_LATENCY: f64 = 15000.0;
pub const ELECTION_TIMEOUT: f64 = 100000.0;
pub const NUM_SERVERS: u32 = 5;
pub const BATCH_SIZE: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ServerState {
    Stopped,
    Follower,
    Candidate,
    Leader,
}

impl ServerState {
    /// CSS class fragment, matching the JS string values.
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerState::Stopped => "stopped",
            ServerState::Follower => "follower",
            ServerState::Candidate => "candidate",
            ServerState::Leader => "leader",
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct LogEntry {
    pub term: u64,
    pub value: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum Body {
    RequestVoteReq { last_log_term: u64, last_log_index: usize },
    RequestVoteRep { granted: bool },
    AppendEntriesReq {
        prev_index: usize,
        prev_term: u64,
        entries: Vec<LogEntry>,
        commit_index: usize,
    },
    AppendEntriesRep { success: bool, match_index: usize },
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Message {
    pub from: u32,
    pub to: u32,
    pub term: u64,
    pub send_time: f64,
    pub recv_time: f64,
    pub body: Body,
}

impl Message {
    pub fn type_str(&self) -> &'static str {
        match self.body {
            Body::RequestVoteReq { .. } | Body::RequestVoteRep { .. } => "RequestVote",
            Body::AppendEntriesReq { .. } | Body::AppendEntriesRep { .. } => "AppendEntries",
        }
    }
    pub fn direction_str(&self) -> &'static str {
        match self.body {
            Body::RequestVoteReq { .. } | Body::AppendEntriesReq { .. } => "request",
            Body::RequestVoteRep { .. } | Body::AppendEntriesRep { .. } => "reply",
        }
    }
    pub fn is_reply(&self) -> bool {
        self.direction_str() == "reply"
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Server {
    pub id: u32,
    pub peers: Vec<u32>,
    pub state: ServerState,
    pub term: u64,
    pub voted_for: Option<u32>,
    pub log: Vec<LogEntry>,
    pub commit_index: usize,
    pub election_alarm: f64,
    pub vote_granted: BTreeMap<u32, bool>,
    pub match_index: BTreeMap<u32, usize>,
    pub next_index: BTreeMap<u32, usize>,
    pub rpc_due: BTreeMap<u32, f64>,
    pub heartbeat_due: BTreeMap<u32, f64>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct Model {
    pub time: f64,
    pub servers: Vec<Server>,
    pub messages: Vec<Message>,
}

fn make_map<T: Clone>(peers: &[u32], value: T) -> BTreeMap<u32, T> {
    peers.iter().map(|&p| (p, value.clone())).collect()
}

fn make_election_alarm(now: f64) -> f64 {
    now + (util::random() + 1.0) * ELECTION_TIMEOUT
}

/// Term of the log entry at 1-based `index`, or 0 if out of range.
fn log_term(log: &[LogEntry], index: usize) -> u64 {
    if index < 1 || index > log.len() {
        0
    } else {
        log[index - 1].term
    }
}

pub fn server(id: u32, peers: Vec<u32>) -> Server {
    Server {
        id,
        state: ServerState::Follower,
        term: 1,
        voted_for: None,
        log: Vec::new(),
        commit_index: 0,
        election_alarm: make_election_alarm(0.0),
        vote_granted: make_map(&peers, false),
        match_index: make_map(&peers, 0usize),
        next_index: make_map(&peers, 1usize),
        rpc_due: make_map(&peers, 0.0),
        heartbeat_due: make_map(&peers, 0.0),
        peers,
    }
}

impl Model {
    pub fn new() -> Model {
        Model {
            time: 0.0,
            servers: Vec::new(),
            messages: Vec::new(),
        }
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.servers.iter().position(|s| s.id == id)
    }

    fn send_message(&mut self, mut message: Message) {
        message.send_time = self.time;
        message.recv_time =
            self.time + MIN_RPC_LATENCY + util::random() * (MAX_RPC_LATENCY - MIN_RPC_LATENCY);
        self.messages.push(message);
    }

    fn step_down(&mut self, si: usize, term: u64) {
        let s = &mut self.servers[si];
        s.term = term;
        s.state = ServerState::Follower;
        s.voted_for = None;
        if s.election_alarm <= self.time || s.election_alarm == INF {
            s.election_alarm = make_election_alarm(self.time);
        }
    }

    // ----- periodically-applied rules -----

    fn start_new_election(&mut self, si: usize) {
        let s = &mut self.servers[si];
        if (s.state == ServerState::Follower || s.state == ServerState::Candidate)
            && s.election_alarm <= self.time
        {
            s.election_alarm = make_election_alarm(self.time);
            s.term += 1;
            s.voted_for = Some(s.id);
            s.state = ServerState::Candidate;
            s.vote_granted = make_map(&s.peers, false);
            s.match_index = make_map(&s.peers, 0usize);
            s.next_index = make_map(&s.peers, 1usize);
            s.rpc_due = make_map(&s.peers, 0.0);
            s.heartbeat_due = make_map(&s.peers, 0.0);
        }
    }

    fn send_request_vote(&mut self, si: usize, peer: u32) {
        let (ready, term, last_log_term, last_log_index) = {
            let s = &self.servers[si];
            (
                s.state == ServerState::Candidate && s.rpc_due[&peer] <= self.time,
                s.term,
                log_term(&s.log, s.log.len()),
                s.log.len(),
            )
        };
        if ready {
            let id = self.servers[si].id;
            self.servers[si].rpc_due.insert(peer, self.time + RPC_TIMEOUT);
            self.send_message(Message {
                from: id,
                to: peer,
                term,
                send_time: 0.0,
                recv_time: 0.0,
                body: Body::RequestVoteReq {
                    last_log_term,
                    last_log_index,
                },
            });
        }
    }

    fn become_leader(&mut self, si: usize) {
        let s = &mut self.servers[si];
        let votes = s.vote_granted.values().filter(|&&v| v).count();
        if s.state == ServerState::Candidate
            && votes + 1 > (NUM_SERVERS / 2) as usize
        {
            s.state = ServerState::Leader;
            s.next_index = make_map(&s.peers, s.log.len() + 1);
            s.rpc_due = make_map(&s.peers, INF);
            s.heartbeat_due = make_map(&s.peers, 0.0);
            s.election_alarm = INF;
        }
    }

    fn send_append_entries(&mut self, si: usize, peer: u32) {
        let payload = {
            let s = &self.servers[si];
            let due = s.state == ServerState::Leader
                && (s.heartbeat_due[&peer] <= self.time
                    || (s.next_index[&peer] <= s.log.len() && s.rpc_due[&peer] <= self.time));
            if !due {
                None
            } else {
                let prev_index = s.next_index[&peer] - 1;
                let mut last_index = (prev_index + BATCH_SIZE).min(s.log.len());
                if s.match_index[&peer] + 1 < s.next_index[&peer] {
                    last_index = prev_index;
                }
                Some((
                    s.id,
                    s.term,
                    prev_index,
                    log_term(&s.log, prev_index),
                    s.log[prev_index..last_index].to_vec(),
                    s.commit_index.min(last_index),
                ))
            }
        };
        if let Some((id, term, prev_index, prev_term, entries, commit_index)) = payload {
            self.send_message(Message {
                from: id,
                to: peer,
                term,
                send_time: 0.0,
                recv_time: 0.0,
                body: Body::AppendEntriesReq {
                    prev_index,
                    prev_term,
                    entries,
                    commit_index,
                },
            });
            let s = &mut self.servers[si];
            s.rpc_due.insert(peer, self.time + RPC_TIMEOUT);
            s.heartbeat_due
                .insert(peer, self.time + ELECTION_TIMEOUT / 2.0);
        }
    }

    fn advance_commit_index(&mut self, si: usize) {
        let s = &mut self.servers[si];
        let mut match_indexes: Vec<usize> = s.match_index.values().copied().collect();
        match_indexes.push(s.log.len());
        match_indexes.sort_unstable();
        let n = match_indexes[(NUM_SERVERS / 2) as usize];
        if s.state == ServerState::Leader && log_term(&s.log, n) == s.term {
            s.commit_index = s.commit_index.max(n);
        }
    }

    // ----- message handlers -----

    fn handle_request_vote_request(&mut self, si: usize, msg: &Message) {
        let (last_log_term, last_log_index) = match msg.body {
            Body::RequestVoteReq {
                last_log_term,
                last_log_index,
            } => (last_log_term, last_log_index),
            _ => unreachable!(),
        };
        if self.servers[si].term < msg.term {
            self.step_down(si, msg.term);
        }
        let mut granted = false;
        {
            let s = &self.servers[si];
            let my_last_term = log_term(&s.log, s.log.len());
            if s.term == msg.term
                && (s.voted_for.is_none() || s.voted_for == Some(msg.from))
                && (last_log_term > my_last_term
                    || (last_log_term == my_last_term && last_log_index >= s.log.len()))
            {
                granted = true;
            }
        }
        if granted {
            let s = &mut self.servers[si];
            s.voted_for = Some(msg.from);
            s.election_alarm = make_election_alarm(self.time);
        }
        let term = self.servers[si].term;
        self.send_message(Message {
            from: msg.to,
            to: msg.from,
            term,
            send_time: 0.0,
            recv_time: 0.0,
            body: Body::RequestVoteRep { granted },
        });
    }

    fn handle_request_vote_reply(&mut self, si: usize, msg: &Message) {
        let granted = match msg.body {
            Body::RequestVoteRep { granted } => granted,
            _ => unreachable!(),
        };
        if self.servers[si].term < msg.term {
            self.step_down(si, msg.term);
        }
        let s = &mut self.servers[si];
        if s.state == ServerState::Candidate && s.term == msg.term {
            s.rpc_due.insert(msg.from, INF);
            s.vote_granted.insert(msg.from, granted);
        }
    }

    fn handle_append_entries_request(&mut self, si: usize, msg: &Message) {
        let (prev_index, prev_term, entries, leader_commit) = match &msg.body {
            Body::AppendEntriesReq {
                prev_index,
                prev_term,
                entries,
                commit_index,
            } => (*prev_index, *prev_term, entries.clone(), *commit_index),
            _ => unreachable!(),
        };
        let mut success = false;
        let mut match_index = 0usize;
        if self.servers[si].term < msg.term {
            self.step_down(si, msg.term);
        }
        if self.servers[si].term == msg.term {
            let s = &mut self.servers[si];
            s.state = ServerState::Follower;
            s.election_alarm = make_election_alarm(self.time);
            if prev_index == 0
                || (prev_index <= s.log.len() && log_term(&s.log, prev_index) == prev_term)
            {
                success = true;
                let mut index = prev_index;
                for e in &entries {
                    index += 1;
                    if log_term(&s.log, index) != e.term {
                        while s.log.len() > index - 1 {
                            s.log.pop();
                        }
                        s.log.push(e.clone());
                    }
                }
                match_index = index;
                s.commit_index = s.commit_index.max(leader_commit);
            }
        }
        let term = self.servers[si].term;
        self.send_message(Message {
            from: msg.to,
            to: msg.from,
            term,
            send_time: 0.0,
            recv_time: 0.0,
            body: Body::AppendEntriesRep {
                success,
                match_index,
            },
        });
    }

    fn handle_append_entries_reply(&mut self, si: usize, msg: &Message) {
        let (success, reply_match) = match msg.body {
            Body::AppendEntriesRep {
                success,
                match_index,
            } => (success, match_index),
            _ => unreachable!(),
        };
        if self.servers[si].term < msg.term {
            self.step_down(si, msg.term);
        }
        let s = &mut self.servers[si];
        if s.state == ServerState::Leader && s.term == msg.term {
            if success {
                let m = s.match_index[&msg.from].max(reply_match);
                s.match_index.insert(msg.from, m);
                s.next_index.insert(msg.from, reply_match + 1);
            } else {
                let n = s.next_index[&msg.from].saturating_sub(1).max(1);
                s.next_index.insert(msg.from, n);
            }
            s.rpc_due.insert(msg.from, 0.0);
        }
    }

    fn handle_message(&mut self, si: usize, msg: &Message) {
        if self.servers[si].state == ServerState::Stopped {
            return;
        }
        match msg.body {
            Body::RequestVoteReq { .. } => self.handle_request_vote_request(si, msg),
            Body::RequestVoteRep { .. } => self.handle_request_vote_reply(si, msg),
            Body::AppendEntriesReq { .. } => self.handle_append_entries_request(si, msg),
            Body::AppendEntriesRep { .. } => self.handle_append_entries_reply(si, msg),
        }
    }

    /// One simulation tick: apply every rule to every server, then deliver any
    /// messages whose `recv_time` has arrived.
    pub fn update(&mut self) {
        let n = self.servers.len();
        for si in 0..n {
            self.start_new_election(si);
            self.become_leader(si);
            self.advance_commit_index(si);
            let peers = self.servers[si].peers.clone();
            for peer in peers {
                self.send_request_vote(si, peer);
                self.send_append_entries(si, peer);
            }
        }
        let mut deliver = Vec::new();
        let mut keep = Vec::new();
        for m in self.messages.drain(..) {
            if m.recv_time <= self.time {
                deliver.push(m);
            } else if m.recv_time < INF {
                keep.push(m);
            }
            // recv_time == INF messages are dropped, matching raft.js
        }
        self.messages = keep;
        for m in deliver {
            if let Some(si) = self.index_of(m.to) {
                self.handle_message(si, &m);
            }
        }
    }

    // ----- user-triggered actions -----

    pub fn stop(&mut self, si: usize) {
        let s = &mut self.servers[si];
        s.state = ServerState::Stopped;
        s.election_alarm = 0.0;
    }

    pub fn resume(&mut self, si: usize) {
        let s = &mut self.servers[si];
        s.state = ServerState::Follower;
        s.election_alarm = make_election_alarm(self.time);
    }

    pub fn resume_all(&mut self) {
        for si in 0..self.servers.len() {
            self.resume(si);
        }
    }

    pub fn restart(&mut self, si: usize) {
        self.stop(si);
        self.resume(si);
    }

    pub fn drop(&mut self, msg_index: usize) {
        if msg_index < self.messages.len() {
            self.messages.remove(msg_index);
        }
    }

    pub fn timeout(&mut self, si: usize) {
        {
            let s = &mut self.servers[si];
            s.state = ServerState::Follower;
            s.election_alarm = 0.0;
        }
        self.start_new_election(si);
    }

    pub fn client_request(&mut self, si: usize) {
        let s = &mut self.servers[si];
        if s.state == ServerState::Leader {
            let term = s.term;
            s.log.push(LogEntry {
                term,
                value: "v".to_string(),
            });
        }
    }

    pub fn spread_timers(&mut self) {
        let mut timers: Vec<f64> = self
            .servers
            .iter()
            .filter(|s| s.election_alarm > self.time && s.election_alarm < INF)
            .map(|s| s.election_alarm)
            .collect();
        timers.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if timers.len() > 1 && timers[1] - timers[0] < MAX_RPC_LATENCY {
            if timers[0] > self.time + MAX_RPC_LATENCY {
                for s in &mut self.servers {
                    if s.election_alarm == timers[0] {
                        s.election_alarm -= MAX_RPC_LATENCY;
                    }
                }
            } else {
                for s in &mut self.servers {
                    if s.election_alarm > timers[0] && s.election_alarm < timers[0] + MAX_RPC_LATENCY
                    {
                        s.election_alarm += MAX_RPC_LATENCY;
                    }
                }
            }
        }
    }

    pub fn align_timers(&mut self) {
        self.spread_timers();
        let mut timers: Vec<f64> = self
            .servers
            .iter()
            .filter(|s| s.election_alarm > self.time && s.election_alarm < INF)
            .map(|s| s.election_alarm)
            .collect();
        timers.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if timers.len() > 1 {
            let (t0, t1) = (timers[0], timers[1]);
            for s in &mut self.servers {
                if s.election_alarm == t1 {
                    s.election_alarm = t0;
                }
            }
        }
    }

    pub fn setup_log_replication_scenario(&mut self) {
        self.restart(1);
        self.restart(2);
        self.restart(3);
        self.restart(4);
        self.timeout(0);
        self.start_new_election(0);
        for i in 1..5 {
            self.servers[i].term = 2;
            self.servers[i].voted_for = Some(1);
        }
        let peers = self.servers[0].peers.clone();
        self.servers[0].vote_granted = make_map(&peers, true);
        self.stop(2);
        self.stop(3);
        self.stop(4);
        self.become_leader(0);
        self.client_request(0);
        self.client_request(0);
        self.client_request(0);
    }

    /// Index of the leader with the highest term, if any.
    pub fn leader_index(&self) -> Option<usize> {
        let mut leader = None;
        let mut term = 0;
        for (i, s) in self.servers.iter().enumerate() {
            if s.state == ServerState::Leader && s.term > term {
                leader = Some(i);
                term = s.term;
            }
        }
        leader
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster() -> Model {
        let mut m = Model::new();
        for i in 1..=NUM_SERVERS {
            let peers: Vec<u32> = (1..=NUM_SERVERS).filter(|&j| j != i).collect();
            m.servers.push(server(i, peers));
        }
        m
    }

    fn run(m: &mut Model, ticks: usize, dt: f64) {
        for _ in 0..ticks {
            m.time += dt;
            m.update();
        }
    }

    #[test]
    fn elects_a_leader() {
        let mut m = cluster();
        run(&mut m, 5000, 1000.0);
        assert!(m.leader_index().is_some(), "no leader emerged from a cold start");
    }

    #[test]
    fn log_replication_scenario_replicates_to_majority() {
        let mut m = cluster();
        m.setup_log_replication_scenario();
        let li = m.leader_index().expect("scenario should produce a leader");
        assert_eq!(m.servers[li].log.len(), 3, "leader should hold 3 client entries");

        m.resume_all();
        run(&mut m, 40000, 1000.0);

        // The 3 entries are from a prior term, so they may stay uncommitted
        // (Raft only commits current-term entries) — but they must replicate.
        let replicated = m.servers.iter().filter(|s| s.log.len() == 3).count();
        assert!(
            replicated >= 3,
            "log not replicated to a majority (only {})",
            replicated
        );
    }

    #[test]
    fn leader_commits_current_term_entries() {
        let mut m = cluster();
        run(&mut m, 5000, 1000.0);
        // Feed the current leader repeatedly so that whoever holds power appends
        // an entry in its own term and can advance the commit index.
        for _ in 0..10 {
            if let Some(li) = m.leader_index() {
                m.client_request(li);
            }
            run(&mut m, 2000, 1000.0);
        }
        let li = m.leader_index().expect("a leader should exist");
        assert!(
            m.servers[li].commit_index >= 1,
            "leader committed nothing (commit_index = {})",
            m.servers[li].commit_index
        );
    }
}
