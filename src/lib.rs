//! DOM/SVG rendering and user interaction, ported from script.js.
//!
//! A single [`App`] lives in a thread-local cell. Every event closure and the
//! `requestAnimationFrame` loop reach it through [`with_app`]. Because JS is
//! single-threaded and our events never fire re-entrantly, the `RefCell`
//! borrow is always exclusive for the duration of one handler.

mod raft;
mod state;
mod util;

use raft::{Body, Model, ServerState, ELECTION_TIMEOUT, NUM_SERVERS};
use state::State;
use std::cell::RefCell;
use std::rc::Rc;
use util::{circle_coord, clamp, INF};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlInputElement, MouseEvent};

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const ARC_WIDTH: f64 = 5.0;
const MESSAGE_RADIUS: f64 = 8.0;

const TERM_COLORS: &[&str] = &[
    "#66c2a5", "#fc8d62", "#8da0cb", "#e78ac3", "#a6d854", "#ffd92f",
];

struct RingSpec {
    cx: f64,
    cy: f64,
    r: f64,
}
const RING: RingSpec = RingSpec {
    cx: 210.0,
    cy: 210.0,
    r: 150.0,
};

struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
const LOGS: Rect = Rect {
    x: 430.0,
    y: 50.0,
    width: 320.0,
    height: 270.0,
};

#[derive(Clone, Copy)]
struct CircleSpec {
    cx: f64,
    cy: f64,
    r: f64,
}

#[derive(Clone, Copy)]
enum SAction {
    Stop,
    Resume,
    Restart,
    Timeout,
    Request,
}

const SERVER_ACTIONS: &[(&str, SAction)] = &[
    ("stop", SAction::Stop),
    ("resume", SAction::Resume),
    ("restart", SAction::Restart),
    ("time out", SAction::Timeout),
    ("request", SAction::Request),
];

// ---------- DOM helpers ----------

fn document() -> Document {
    web_sys::window().unwrap().document().unwrap()
}

fn svg_el(tag: &str) -> Element {
    document().create_element_ns(Some(SVG_NS), tag).unwrap()
}

fn html_el(tag: &str) -> Element {
    document().create_element(tag).unwrap()
}

fn set_attr(el: &Element, name: &str, value: &str) {
    el.set_attribute(name, value).unwrap();
}

fn set_circle(el: &Element, c: &CircleSpec) {
    set_attr(el, "cx", &c.cx.to_string());
    set_attr(el, "cy", &c.cy.to_string());
    set_attr(el, "r", &c.r.to_string());
}

fn by_id(id: &str) -> Element {
    document().get_element_by_id(id).unwrap()
}

fn input(id: &str) -> HtmlInputElement {
    by_id(id).dyn_into::<HtmlInputElement>().unwrap()
}

fn query(parent: &Element, sel: &str) -> Option<Element> {
    parent.query_selector(sel).unwrap()
}

// ---------- geometry ----------

fn server_spec(id: u32) -> CircleSpec {
    let c = circle_coord((id as f64 - 1.0) / NUM_SERVERS as f64, RING.cx, RING.cy, RING.r);
    CircleSpec {
        cx: c.x,
        cy: c.y,
        r: 30.0,
    }
}

fn arc_spec(spec: &CircleSpec, fraction: f64) -> String {
    let radius = spec.r + ARC_WIDTH / 2.0;
    let end = circle_coord(fraction, spec.cx, spec.cy, radius);
    let mut s = format!("M {},{}", spec.cx, spec.cy - radius);
    if fraction > 0.5 {
        s.push_str(&format!(
            " A {},{} 0 0,1 {},{} M {},{}",
            radius,
            radius,
            spec.cx,
            spec.cy + radius,
            spec.cx,
            spec.cy + radius
        ));
    }
    s.push_str(&format!(" A {},{} 0 0,1 {},{}", radius, radius, end.x, end.y));
    s
}

fn message_spec(from: u32, to: u32, frac: f64) -> CircleSpec {
    let f = server_spec(from);
    let t = server_spec(to);
    let total = ((t.cx - f.cx).powi(2) + (t.cy - f.cy).powi(2)).sqrt();
    let travel = total - f.r - t.r;
    let frac = (f.r / total) + frac * (travel / total);
    CircleSpec {
        cx: f.cx + (t.cx - f.cx) * frac,
        cy: f.cy + (t.cy - f.cy) * frac,
        r: MESSAGE_RADIUS,
    }
}

fn message_arrow_spec(from: u32, to: u32, frac: f64) -> String {
    let f = server_spec(from);
    let t = server_spec(to);
    let total = ((t.cx - f.cx).powi(2) + (t.cy - f.cy).powi(2)).sqrt();
    let travel = total - f.r - t.r;
    let frac_s = ((f.r + MESSAGE_RADIUS) / total) + frac * (travel / total);
    let frac_h = ((f.r + 2.0 * MESSAGE_RADIUS) / total) + frac * (travel / total);
    format!(
        "M {},{} L {},{}",
        f.cx + (t.cx - f.cx) * frac_s,
        f.cy + (t.cy - f.cy) * frac_s,
        f.cx + (t.cx - f.cx) * frac_h,
        f.cy + (t.cy - f.cy) * frac_h
    )
}

fn rel_time(time: f64, now: f64) -> String {
    if time == INF {
        return "infinity".to_string();
    }
    let sign = if time > now { "+" } else { "" };
    format!("{}{:.3}ms", sign, (time - now) / 1e3)
}

/// Linear slider value -> logarithmic time factor (slower-than-real-time).
fn speed_transform(v: f64) -> f64 {
    let p = 10f64.powf(v);
    if p < 1.0 {
        1.0
    } else {
        p
    }
}

// ---------- the application ----------

struct App {
    state: State,
    paused: bool,
    sliding: bool,
    last_base: Option<Model>,
    last_ts: Option<f64>,
    // keep handler closures alive; cleared/replaced as their nodes rebuild
    server_handlers: Vec<Closure<dyn FnMut(MouseEvent)>>,
    message_handlers: Vec<Closure<dyn FnMut(MouseEvent)>>,
    transient_handlers: Vec<Closure<dyn FnMut(MouseEvent)>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = RefCell::new(None);
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|cell| f(cell.borrow_mut().as_mut().unwrap()))
}

impl App {
    fn new() -> App {
        let mut model = Model::new();
        for i in 1..=NUM_SERVERS {
            let peers: Vec<u32> = (1..=NUM_SERVERS).filter(|&j| j != i).collect();
            model.servers.push(raft::server(i, peers));
        }
        App {
            state: State::new(model),
            paused: false,
            sliding: false,
            last_base: None,
            last_ts: None,
            server_handlers: Vec::new(),
            message_handlers: Vec::new(),
            transient_handlers: Vec::new(),
        }
    }

    // ----- playback -----

    fn toggle_pause(&mut self) {
        if self.paused {
            self.resume();
        } else {
            self.pause();
        }
    }

    fn pause(&mut self) {
        self.paused = true;
        let icon = by_id("time-icon");
        icon.class_list().remove_1("glyphicon-time").ok();
        icon.class_list().add_1("glyphicon-pause").ok();
        set_attr(&by_id("pause"), "class", "paused");
        set_status(true);
        self.render();
    }

    fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            let icon = by_id("time-icon");
            icon.class_list().remove_1("glyphicon-pause").ok();
            icon.class_list().add_1("glyphicon-time").ok();
            set_attr(&by_id("pause"), "class", "resumed");
            set_status(false);
            self.render();
        }
    }

    // ----- actions -----

    fn apply_server(&mut self, a: SAction, si: usize) {
        match a {
            SAction::Stop => self.state.current.stop(si),
            SAction::Resume => self.state.current.resume(si),
            SAction::Restart => self.state.current.restart(si),
            SAction::Timeout => self.state.current.timeout(si),
            SAction::Request => self.state.current.client_request(si),
        }
    }

    fn do_server_action(&mut self, a: SAction, si: usize) {
        self.state.fork();
        self.apply_server(a, si);
        self.state.save();
        self.render();
        self.hide_overlays();
    }

    fn do_drop(&mut self, mi: usize) {
        self.state.fork();
        self.state.current.drop(mi);
        self.state.save();
        self.render();
        self.hide_overlays();
    }

    // ----- rendering -----

    fn render(&mut self) {
        let servers_same;
        let messages_same;
        match &self.last_base {
            Some(prev) => {
                servers_same = prev.servers == self.state.current.servers;
                messages_same = prev.messages == self.state.current.messages;
            }
            None => {
                servers_same = false;
                messages_same = false;
            }
        }
        self.last_base = Some(self.state.base().clone());

        self.render_clock();
        self.render_servers(servers_same);
        self.render_messages(messages_same);
        if !servers_same {
            self.render_logs();
        }
    }

    fn render_clock(&self) {
        if !self.sliding {
            let t = input("time");
            set_attr(t.as_ref(), "max", &self.state.get_max_time().to_string());
            t.set_value_as_number(self.state.current.time);
        }
    }

    fn render_servers(&mut self, servers_same: bool) {
        if !servers_same {
            self.server_handlers.clear();
        }
        let time = self.state.current.time;
        let n = self.state.current.servers.len();
        for idx in 0..n {
            let (id, election_alarm, term, st, is_candidate);
            {
                let s = &self.state.current.servers[idx];
                id = s.id;
                election_alarm = s.election_alarm;
                term = s.term;
                st = s.state;
                is_candidate = s.state == ServerState::Candidate;
            }
            let node = by_id(&format!("server-{}", id));
            let spec = server_spec(id);
            if let Some(path) = query(&node, "path") {
                let frac = clamp((election_alarm - time) / (ELECTION_TIMEOUT * 2.0), 0.0, 1.0);
                set_attr(&path, "d", &arc_spec(&spec, frac));
            }
            if servers_same {
                continue;
            }
            if let Some(t) = query(&node, "text.term") {
                t.set_text_content(Some(&term.to_string()));
            }
            set_attr(&node, "class", &format!("server {}", st.as_str()));
            if let Some(bg) = query(&node, "circle.background") {
                let fill = if st == ServerState::Stopped {
                    "gray".to_string()
                } else {
                    TERM_COLORS[(term as usize) % TERM_COLORS.len()].to_string()
                };
                set_attr(&bg, "style", &format!("fill: {}", fill));
            }
            if let Some(votes) = query(&node, ".votes") {
                votes.set_inner_html("");
                if is_candidate {
                    for pi in 0..n {
                        let peer = &self.state.current.servers[pi];
                        let coord = circle_coord(
                            (peer.id as f64 - 1.0) / NUM_SERVERS as f64,
                            spec.cx,
                            spec.cy,
                            spec.r * 5.0 / 8.0,
                        );
                        let granted = *self.state.current.servers[idx]
                            .vote_granted
                            .get(&peer.id)
                            .unwrap_or(&false);
                        let cls = if peer.id == id || granted {
                            "have"
                        } else if peer.voted_for == Some(id) && peer.term == term {
                            "coming"
                        } else {
                            "no"
                        };
                        let c = svg_el("circle");
                        set_attr(&c, "cx", &coord.x.to_string());
                        set_attr(&c, "cy", &coord.y.to_string());
                        set_attr(&c, "r", "5");
                        set_attr(&c, "class", cls);
                        votes.append_child(&c).unwrap();
                    }
                }
            }
            self.attach_server_handlers(&node, idx);
        }
    }

    fn attach_server_handlers(&mut self, node: &Element, si: usize) {
        // left click -> detail modal
        let click = Closure::wrap(Box::new(move |e: MouseEvent| {
            e.prevent_default();
            with_app(|app| app.show_server_modal(si));
        }) as Box<dyn FnMut(MouseEvent)>);
        node.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())
            .unwrap();
        self.server_handlers.push(click);

        // right click -> context menu
        let menu = Closure::wrap(Box::new(move |e: MouseEvent| {
            e.prevent_default();
            with_app(|app| app.show_server_menu(si, e.client_x() as f64, e.client_y() as f64));
        }) as Box<dyn FnMut(MouseEvent)>);
        node.add_event_listener_with_callback("contextmenu", menu.as_ref().unchecked_ref())
            .unwrap();
        self.server_handlers.push(menu);
    }

    fn render_messages(&mut self, messages_same: bool) {
        let messages_group = by_id("messages");
        if !messages_same {
            self.message_handlers.clear();
            messages_group.set_inner_html("");
            let count = self.state.current.messages.len();
            for i in 0..count {
                let (dir, typ, is_reply) = {
                    let m = &self.state.current.messages[i];
                    (m.direction_str(), m.type_str(), m.is_reply())
                };
                let a = svg_el("a");
                set_attr(&a, "id", &format!("message-{}", i));
                set_attr(&a, "class", &format!("message {} {}", dir, typ));
                a.append_child(&svg_el("circle")).unwrap();
                let dirpath = svg_el("path");
                set_attr(&dirpath, "class", "message-direction");
                a.append_child(&dirpath).unwrap();
                if is_reply {
                    let sp = svg_el("path");
                    set_attr(&sp, "class", "message-success");
                    a.append_child(&sp).unwrap();
                }
                messages_group.append_child(&a).unwrap();

                let click = Closure::wrap(Box::new(move |e: MouseEvent| {
                    e.prevent_default();
                    with_app(|app| app.show_message_modal(i));
                }) as Box<dyn FnMut(MouseEvent)>);
                a.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())
                    .unwrap();
                self.message_handlers.push(click);

                let menu = Closure::wrap(Box::new(move |e: MouseEvent| {
                    e.prevent_default();
                    with_app(|app| app.show_message_menu(i, e.client_x() as f64, e.client_y() as f64));
                }) as Box<dyn FnMut(MouseEvent)>);
                a.add_event_listener_with_callback("contextmenu", menu.as_ref().unchecked_ref())
                    .unwrap();
                self.message_handlers.push(menu);
            }
        }
        let time = self.state.current.time;
        let paused = self.paused;
        let count = self.state.current.messages.len();
        for i in 0..count {
            let (from, to, send_time, recv_time, is_reply, typ, ok) = {
                let m = &self.state.current.messages[i];
                let ok = match m.body {
                    Body::RequestVoteRep { granted } => granted,
                    Body::AppendEntriesRep { success, .. } => success,
                    _ => false,
                };
                (m.from, m.to, m.send_time, m.recv_time, m.is_reply(), m.type_str(), ok)
            };
            let frac = (time - send_time) / (recv_time - send_time);
            let s = message_spec(from, to, frac);
            let node = by_id(&format!("message-{}", i));
            if let Some(circle) = query(&node, "circle") {
                set_circle(&circle, &s);
            }
            if is_reply {
                if let Some(sp) = query(&node, "path.message-success") {
                    let mut d = format!("M {},{} L {},{}", s.cx - s.r, s.cy, s.cx + s.r, s.cy);
                    if ok {
                        d.push_str(&format!(
                            " M {},{} L {},{}",
                            s.cx,
                            s.cy - s.r,
                            s.cx,
                            s.cy + s.r
                        ));
                    }
                    set_attr(&sp, "d", &d);
                }
            }
            if let Some(dir) = query(&node, "path.message-direction") {
                if paused {
                    set_attr(
                        &dir,
                        "style",
                        &format!("marker-end:url(#TriangleOutS-{})", typ),
                    );
                    set_attr(&dir, "d", &message_arrow_spec(from, to, frac));
                } else {
                    set_attr(&dir, "style", "");
                    set_attr(&dir, "d", "M 0,0");
                }
            }
        }
    }

    fn render_logs(&self) {
        const LABEL_WIDTH: f64 = 25.0;
        const INDEX_HEIGHT: f64 = 25.0;
        let group = document().query_selector(".logs").unwrap().unwrap();
        group.set_inner_html("");

        let bg = svg_el("rect");
        set_attr(&bg, "id", "logsbg");
        set_attr(&bg, "x", &LOGS.x.to_string());
        set_attr(&bg, "y", &LOGS.y.to_string());
        set_attr(&bg, "width", &LOGS.width.to_string());
        set_attr(&bg, "height", &LOGS.height.to_string());
        group.append_child(&bg).unwrap();

        let height = (LOGS.height - INDEX_HEIGHT) / NUM_SERVERS as f64;
        let leader = self.state.current.leader_index();

        let index_x = LOGS.x + LABEL_WIDTH + LOGS.width * 0.05;
        let index_y = LOGS.y + 2.0 * height / 6.0;
        let index_w = LOGS.width * 0.9;
        let indexes = svg_el("g");
        set_attr(&indexes, "id", "log-indexes");
        group.append_child(&indexes).unwrap();
        for index in 1..=10 {
            let t = svg_el("text");
            set_attr(
                &t,
                "x",
                &(index_x + (index as f64 - 0.5) * index_w / 11.0).to_string(),
            );
            set_attr(&t, "y", &index_y.to_string());
            t.set_text_content(Some(&index.to_string()));
            indexes.append_child(&t).unwrap();
        }

        let entry_x = LOGS.x + LABEL_WIDTH + LOGS.width * 0.05;
        let entry_w = LOGS.width * 0.9;
        let cell_h = 2.0 * height / 3.0;
        let log_entry_x = |index: f64| entry_x + (index - 1.0) * entry_w / 11.0;

        for s in &self.state.current.servers {
            let row_y = LOGS.y + INDEX_HEIGHT + height * s.id as f64 - 5.0 * height / 6.0;
            let log = svg_el("g");
            set_attr(&log, "id", &format!("log-S{}", s.id));
            group.append_child(&log).unwrap();

            let label = svg_el("text");
            label.set_text_content(Some(&format!("S{}", s.id)));
            set_attr(&label, "class", &format!("serverid {}", s.state.as_str()));
            set_attr(&label, "x", &(entry_x - LABEL_WIDTH * 4.0 / 5.0).to_string());
            set_attr(&label, "y", &(row_y + cell_h / 2.0).to_string());
            log.append_child(&label).unwrap();

            for index in 1..=10 {
                let cell = svg_el("rect");
                set_attr(&cell, "x", &log_entry_x(index as f64).to_string());
                set_attr(&cell, "y", &row_y.to_string());
                set_attr(&cell, "width", &(entry_w / 11.0).to_string());
                set_attr(&cell, "height", &cell_h.to_string());
                set_attr(&cell, "class", "log");
                log.append_child(&cell).unwrap();
            }

            for (i, entry) in s.log.iter().enumerate() {
                let index = i + 1;
                let committed = index <= s.commit_index;
                let g = svg_el("g");
                set_attr(
                    &g,
                    "class",
                    &format!("entry {}", if committed { "committed" } else { "uncommitted" }),
                );
                let rect = svg_el("rect");
                set_attr(&rect, "x", &log_entry_x(index as f64).to_string());
                set_attr(&rect, "y", &row_y.to_string());
                set_attr(&rect, "width", &(entry_w / 11.0).to_string());
                set_attr(&rect, "height", &cell_h.to_string());
                set_attr(&rect, "stroke-dasharray", if committed { "1 0" } else { "5 5" });
                set_attr(
                    &rect,
                    "style",
                    &format!("fill: {}", TERM_COLORS[(entry.term as usize) % TERM_COLORS.len()]),
                );
                g.append_child(&rect).unwrap();
                let txt = svg_el("text");
                set_attr(&txt, "x", &(log_entry_x(index as f64) + entry_w / 22.0).to_string());
                set_attr(&txt, "y", &(row_y + cell_h / 2.0).to_string());
                txt.set_text_content(Some(&entry.term.to_string()));
                g.append_child(&txt).unwrap();
                log.append_child(&g).unwrap();
            }

            if let Some(li) = leader {
                if self.state.current.servers[li].id != s.id {
                    let leader_s = &self.state.current.servers[li];
                    let mi = *leader_s.match_index.get(&s.id).unwrap_or(&0);
                    let ni = *leader_s.next_index.get(&s.id).unwrap_or(&1);
                    let circle = svg_el("circle");
                    set_attr(&circle, "cx", &log_entry_x(mi as f64 + 1.0).to_string());
                    set_attr(&circle, "cy", &(row_y + cell_h).to_string());
                    set_attr(&circle, "r", "5");
                    log.append_child(&circle).unwrap();
                    let x = log_entry_x(ni as f64 + 0.5);
                    let path = svg_el("path");
                    set_attr(&path, "style", "marker-end:url(#TriangleOutM); stroke: black");
                    set_attr(
                        &path,
                        "d",
                        &format!(
                            "M {},{} L {},{}",
                            x,
                            row_y + cell_h + cell_h / 3.0,
                            x,
                            row_y + cell_h + cell_h / 6.0
                        ),
                    );
                    set_attr(&path, "stroke-width", "3");
                    log.append_child(&path).unwrap();
                }
            }
        }
    }

    // ----- modals & context menus -----

    fn show_server_modal(&mut self, si: usize) {
        self.pause();
        let m = by_id("modal-details");
        let model_time = self.state.current.time;
        let s = &self.state.current.servers[si];
        if let Some(t) = query(&m, ".modal-title") {
            t.set_text_content(Some(&format!("Server {}", s.id)));
        }
        if let Some(d) = query(&m, ".modal-dialog") {
            d.class_list().remove_1("modal-sm").ok();
            d.class_list().add_1("modal-lg").ok();
        }

        let mut rows = String::new();
        rows.push_str("<tr><th>peer</th><th>next index</th><th>match index</th><th>vote granted</th><th>RPC due</th><th>heartbeat due</th></tr>");
        for &peer in &s.peers {
            rows.push_str(&format!(
                "<tr><td>S{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                peer,
                s.next_index[&peer],
                s.match_index[&peer],
                s.vote_granted[&peer],
                rel_time(s.rpc_due[&peer], model_time),
                rel_time(s.heartbeat_due[&peer], model_time),
            ));
        }
        let voted = s
            .voted_for
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string());
        let body_html = format!(
            "<dl class=\"dl-horizontal\">\
               <dt>state</dt><dd>{}</dd>\
               <dt>currentTerm</dt><dd>{}</dd>\
               <dt>votedFor</dt><dd>{}</dd>\
               <dt>commitIndex</dt><dd>{}</dd>\
               <dt>electionAlarm</dt><dd>{}</dd>\
               <dt>peers</dt>\
               <dd><table class=\"table table-condensed\">{}</table></dd>\
             </dl>",
            s.state.as_str(),
            s.term,
            voted,
            s.commit_index,
            rel_time(s.election_alarm, model_time),
            rows,
        );
        if let Some(b) = query(&m, ".modal-body") {
            b.set_inner_html(&body_html);
        }
        self.build_modal_footer_server(&m, si);
        show_modal(&m);
    }

    fn build_modal_footer_server(&mut self, m: &Element, si: usize) {
        self.transient_handlers.clear();
        let footer = query(m, ".modal-footer").unwrap();
        footer.set_inner_html("");
        for &(label, action) in SERVER_ACTIONS {
            let btn = html_el("button");
            set_attr(&btn, "type", "button");
            set_attr(&btn, "class", "btn btn-default");
            btn.set_text_content(Some(label));
            let cb = Closure::wrap(Box::new(move |_e: MouseEvent| {
                with_app(|app| app.do_server_action(action, si));
            }) as Box<dyn FnMut(MouseEvent)>);
            btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
                .unwrap();
            self.transient_handlers.push(cb);
            footer.append_child(&btn).unwrap();
        }
    }

    fn show_message_modal(&mut self, mi: usize) {
        self.pause();
        let m = by_id("modal-details");
        let now = self.state.current.time;
        let msg = &self.state.current.messages[mi];
        if let Some(d) = query(&m, ".modal-dialog") {
            d.class_list().remove_1("modal-lg").ok();
            d.class_list().add_1("modal-sm").ok();
        }
        if let Some(t) = query(&m, ".modal-title") {
            t.set_text_content(Some(&format!("{} {}", msg.type_str(), msg.direction_str())));
        }
        let mut html = format!(
            "<dl class=\"dl-horizontal\">\
               <dt>from</dt><dd>S{}</dd>\
               <dt>to</dt><dd>S{}</dd>\
               <dt>sent</dt><dd>{}</dd>\
               <dt>deliver</dt><dd>{}</dd>\
               <dt>term</dt><dd>{}</dd>",
            msg.from,
            msg.to,
            rel_time(msg.send_time, now),
            rel_time(msg.recv_time, now),
            msg.term,
        );
        match &msg.body {
            Body::RequestVoteReq {
                last_log_term,
                last_log_index,
            } => {
                html.push_str(&format!(
                    "<dt>lastLogIndex</dt><dd>{}</dd><dt>lastLogTerm</dt><dd>{}</dd>",
                    last_log_index, last_log_term
                ));
            }
            Body::RequestVoteRep { granted } => {
                html.push_str(&format!("<dt>granted</dt><dd>{}</dd>", granted));
            }
            Body::AppendEntriesReq {
                prev_index,
                prev_term,
                entries,
                commit_index,
            } => {
                let e: Vec<String> = entries.iter().map(|x| x.term.to_string()).collect();
                html.push_str(&format!(
                    "<dt>prevIndex</dt><dd>{}</dd><dt>prevTerm</dt><dd>{}</dd>\
                     <dt>entries</dt><dd>[{}]</dd><dt>commitIndex</dt><dd>{}</dd>",
                    prev_index,
                    prev_term,
                    e.join(" "),
                    commit_index
                ));
            }
            Body::AppendEntriesRep {
                success,
                match_index,
            } => {
                html.push_str(&format!(
                    "<dt>success</dt><dd>{}</dd><dt>matchIndex</dt><dd>{}</dd>",
                    success, match_index
                ));
            }
        }
        html.push_str("</dl>");
        if let Some(b) = query(&m, ".modal-body") {
            b.set_inner_html(&html);
        }

        self.transient_handlers.clear();
        let footer = query(&m, ".modal-footer").unwrap();
        footer.set_inner_html("");
        let btn = html_el("button");
        set_attr(&btn, "type", "button");
        set_attr(&btn, "class", "btn btn-default");
        btn.set_text_content(Some("drop"));
        let cb = Closure::wrap(Box::new(move |_e: MouseEvent| {
            with_app(|app| app.do_drop(mi));
        }) as Box<dyn FnMut(MouseEvent)>);
        btn.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .unwrap();
        self.transient_handlers.push(cb);
        footer.append_child(&btn).unwrap();
        show_modal(&m);
    }

    fn show_server_menu(&mut self, si: usize, x: f64, y: f64) {
        self.transient_handlers.clear();
        let ul = document()
            .query_selector("#context-menu ul")
            .unwrap()
            .unwrap();
        ul.set_inner_html("");
        for &(label, action) in SERVER_ACTIONS {
            let li = html_el("li");
            let a = html_el("a");
            set_attr(&a, "href", "#");
            a.set_text_content(Some(label));
            let cb = Closure::wrap(Box::new(move |e: MouseEvent| {
                e.prevent_default();
                with_app(|app| app.do_server_action(action, si));
            }) as Box<dyn FnMut(MouseEvent)>);
            a.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
                .unwrap();
            self.transient_handlers.push(cb);
            li.append_child(&a).unwrap();
            ul.append_child(&li).unwrap();
        }
        show_context_menu(x, y);
    }

    fn show_message_menu(&mut self, mi: usize, x: f64, y: f64) {
        self.transient_handlers.clear();
        let ul = document()
            .query_selector("#context-menu ul")
            .unwrap()
            .unwrap();
        ul.set_inner_html("");
        let li = html_el("li");
        let a = html_el("a");
        set_attr(&a, "href", "#");
        a.set_text_content(Some("drop"));
        let cb = Closure::wrap(Box::new(move |e: MouseEvent| {
            e.prevent_default();
            with_app(|app| app.do_drop(mi));
        }) as Box<dyn FnMut(MouseEvent)>);
        a.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .unwrap();
        self.transient_handlers.push(cb);
        li.append_child(&a).unwrap();
        ul.append_child(&li).unwrap();
        show_context_menu(x, y);
    }

    fn hide_overlays(&self) {
        hide_modals();
        hide_context_menu();
    }

    // ----- animation tick -----

    fn step(&mut self, ts: f64) {
        if !self.paused {
            if let Some(last) = self.last_ts {
                if ts - last < 500.0 {
                    let wall_micros = (ts - last) * 1000.0;
                    let speed = speed_transform(input("speed").value_as_number());
                    let model_micros = self.state.current.time + wall_micros / speed;
                    self.state.seek(model_micros);
                    self.render();
                }
            }
        }
        self.last_ts = Some(ts);
    }
}

// ---------- overlay helpers (DOM-only, no App access) ----------

fn show_modal(m: &Element) {
    if let Some(html) = m.dyn_ref::<web_sys::HtmlElement>() {
        html.style().set_property("display", "block").ok();
    }
    let doc = document();
    if doc.get_element_by_id("rs-backdrop").is_none() {
        let backdrop = html_el("div");
        set_attr(&backdrop, "id", "rs-backdrop");
        set_attr(&backdrop, "class", "modal-backdrop in");
        doc.body().unwrap().append_child(&backdrop).unwrap();
    }
}

fn hide_modals() {
    let doc = document();
    for id in ["modal-details", "modal-help"] {
        if let Some(el) = doc.get_element_by_id(id) {
            if let Some(html) = el.dyn_ref::<web_sys::HtmlElement>() {
                html.style().set_property("display", "none").ok();
            }
        }
    }
    if let Some(b) = doc.get_element_by_id("rs-backdrop") {
        b.remove();
    }
}

fn show_context_menu(x: f64, y: f64) {
    let menu = by_id("context-menu");
    if let Some(html) = menu.dyn_ref::<web_sys::HtmlElement>() {
        let style = html.style();
        style.set_property("position", "fixed").ok();
        style.set_property("left", &format!("{}px", x)).ok();
        style.set_property("top", &format!("{}px", y)).ok();
        style.set_property("z-index", "2000").ok();
        style.set_property("display", "block").ok();
    }
    if let Some(ul) = query(&menu, "ul") {
        if let Some(html) = ul.dyn_ref::<web_sys::HtmlElement>() {
            html.style().set_property("display", "block").ok();
        }
    }
}

fn hide_context_menu() {
    if let Some(menu) = document().get_element_by_id("context-menu") {
        if let Some(html) = menu.dyn_ref::<web_sys::HtmlElement>() {
            html.style().set_property("display", "none").ok();
        }
    }
}

fn request_animation_frame(f: &Closure<dyn FnMut(f64)>) {
    web_sys::window()
        .unwrap()
        .request_animation_frame(f.as_ref().unchecked_ref())
        .unwrap();
}

// ---------- bootstrap wiring ----------

#[wasm_bindgen(start)]
pub fn run() {
    console_error_panic_hook::set_once();

    let mut app = App::new();
    // initial static SVG geometry that the JS set up once
    setup_static_svg();
    app.state.init();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|app| app.render());

    setup_keyboard();
    setup_action_buttons();
    setup_sliders();
    setup_global_dismiss();
    setup_raf();
}

fn set_status(paused: bool) {
    let doc = document();
    if let Some(pill) = doc.get_element_by_id("status-pill") {
        if paused {
            pill.class_list().add_1("paused").ok();
        } else {
            pill.class_list().remove_1("paused").ok();
        }
    }
    if let Some(txt) = doc.get_element_by_id("status-text") {
        txt.set_text_content(Some(if paused { "paused" } else { "running" }));
    }
}

fn setup_static_svg() {
    // ring
    let ring = by_id("ring");
    set_attr(&ring, "cx", &RING.cx.to_string());
    set_attr(&ring, "cy", &RING.cy.to_string());
    set_attr(&ring, "r", &RING.r.to_string());
    // pause glyph transform
    set_attr(
        &by_id("pause"),
        "transform",
        &format!(
            "translate({}, {}) scale({})",
            RING.cx,
            RING.cy,
            RING.r / 3.5
        ),
    );
    // server <g> skeletons
    let servers_group = by_id("servers");
    for id in 1..=NUM_SERVERS {
        let spec = server_spec(id);
        let g = svg_el("g");
        set_attr(&g, "id", &format!("server-{}", id));
        set_attr(&g, "class", "server");

        let label = svg_el("text");
        set_attr(&label, "class", "serverid");
        label.set_text_content(Some(&format!("S{}", id)));
        let lc = circle_coord(
            (id as f64 - 1.0) / NUM_SERVERS as f64,
            RING.cx,
            RING.cy,
            RING.r + 50.0,
        );
        set_attr(&label, "x", &lc.x.to_string());
        set_attr(&label, "y", &lc.y.to_string());
        g.append_child(&label).unwrap();

        let a = svg_el("a");
        let bg = svg_el("circle");
        set_attr(&bg, "class", "background");
        set_circle(&bg, &spec);
        a.append_child(&bg).unwrap();
        let votes = svg_el("g");
        set_attr(&votes, "class", "votes");
        a.append_child(&votes).unwrap();
        let path = svg_el("path");
        set_attr(&path, "style", &format!("stroke-width: {}", ARC_WIDTH));
        a.append_child(&path).unwrap();
        let term = svg_el("text");
        set_attr(&term, "class", "term");
        set_attr(&term, "x", &spec.cx.to_string());
        set_attr(&term, "y", &spec.cy.to_string());
        a.append_child(&term).unwrap();
        g.append_child(&a).unwrap();

        servers_group.append_child(&g).unwrap();
    }
}

fn dispatch_key(app: &mut App, k: &str) {
    let leader = app.state.current.leader_index();
    match k {
        " " | "." => {
            hide_modals();
            app.toggle_pause();
        }
        "c" => {
            if let Some(li) = leader {
                app.state.fork();
                app.state.current.client_request(li);
                app.state.save();
                app.render();
                hide_modals();
            }
        }
        "r" => {
            if let Some(li) = leader {
                app.state.fork();
                app.state.current.restart(li);
                app.state.save();
                app.render();
                hide_modals();
            }
        }
        "t" => {
            app.state.fork();
            app.state.current.spread_timers();
            app.state.save();
            app.render();
            hide_modals();
        }
        "a" => {
            app.state.fork();
            app.state.current.align_timers();
            app.state.save();
            app.render();
            hide_modals();
        }
        "l" => {
            app.state.fork();
            app.pause();
            app.state.current.setup_log_replication_scenario();
            app.state.save();
            app.render();
            hide_modals();
        }
        "b" => {
            app.state.fork();
            app.state.current.resume_all();
            app.state.save();
            app.render();
            hide_modals();
        }
        "f" => {
            app.state.fork();
            app.render();
            hide_modals();
        }
        "?" => {
            app.pause();
            show_modal(&by_id("modal-help"));
        }
        _ => {}
    }
}

fn setup_keyboard() {
    let cb = Closure::wrap(Box::new(move |e: web_sys::KeyboardEvent| {
        if let Some(target) = e.target() {
            if let Some(el) = target.dyn_ref::<Element>() {
                if el.id() == "title" {
                    return;
                }
            }
        }
        let k = e.key().to_lowercase();
        with_app(|app| dispatch_key(app, k.as_str()));
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback("keyup", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();
}

fn setup_action_buttons() {
    let nodes = document().query_selector_all("[data-action]").unwrap();
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else { continue };
        let Ok(el) = node.dyn_into::<Element>() else { continue };
        let action = el.get_attribute("data-action").unwrap_or_default();
        let cb = Closure::wrap(Box::new(move |e: MouseEvent| {
            e.prevent_default();
            let a = action.clone();
            with_app(|app| dispatch_key(app, a.as_str()));
        }) as Box<dyn FnMut(MouseEvent)>);
        el.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref())
            .unwrap();
        cb.forget();
    }
}

fn setup_sliders() {
    // time slider
    let t = input("time");
    let down = Closure::wrap(Box::new(move |_e: web_sys::PointerEvent| {
        with_app(|app| {
            app.pause();
            app.sliding = true;
        });
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    t.add_event_listener_with_callback("pointerdown", down.as_ref().unchecked_ref())
        .unwrap();
    down.forget();

    let slide = Closure::wrap(Box::new(move |_e: web_sys::Event| {
        with_app(|app| {
            let v = input("time").value_as_number();
            app.state.seek(v);
            app.render();
        });
    }) as Box<dyn FnMut(web_sys::Event)>);
    t.add_event_listener_with_callback("input", slide.as_ref().unchecked_ref())
        .unwrap();
    slide.forget();

    let stop = Closure::wrap(Box::new(move |_e: web_sys::Event| {
        with_app(|app| {
            let v = input("time").value_as_number();
            app.state.seek(v);
            app.sliding = false;
            app.render();
        });
    }) as Box<dyn FnMut(web_sys::Event)>);
    t.add_event_listener_with_callback("change", stop.as_ref().unchecked_ref())
        .unwrap();
    stop.forget();

    // speed slider: just keep the readout in sync
    update_speed_label();
    let sp = input("speed");
    let on_speed = Closure::wrap(Box::new(move |_e: web_sys::Event| {
        update_speed_label();
    }) as Box<dyn FnMut(web_sys::Event)>);
    sp.add_event_listener_with_callback("input", on_speed.as_ref().unchecked_ref())
        .unwrap();
    on_speed.forget();

    // play/pause button
    let btn = by_id("time-button");
    let toggle = Closure::wrap(Box::new(move |e: MouseEvent| {
        e.prevent_default();
        with_app(|app| app.toggle_pause());
    }) as Box<dyn FnMut(MouseEvent)>);
    btn.add_event_listener_with_callback("click", toggle.as_ref().unchecked_ref())
        .unwrap();
    toggle.forget();
}

fn update_speed_label() {
    if let Some(label) = document().get_element_by_id("speed-value") {
        let v = input("speed").value_as_number();
        label.set_text_content(Some(&format!("1/{:.0}x", speed_transform(v))));
    }
}

fn setup_global_dismiss() {
    // any mousedown closes the context menu (unless it's inside the menu)
    let cb = Closure::wrap(Box::new(move |e: MouseEvent| {
        if let Some(target) = e.target() {
            if let Some(el) = target.dyn_ref::<Element>() {
                if el.closest("#context-menu").ok().flatten().is_some() {
                    return;
                }
            }
        }
        hide_context_menu();
    }) as Box<dyn FnMut(MouseEvent)>);
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback("mousedown", cb.as_ref().unchecked_ref())
        .unwrap();
    cb.forget();

    // close buttons / backdrop dismiss for modals
    let cb2 = Closure::wrap(Box::new(move |e: MouseEvent| {
        if let Some(target) = e.target() {
            if let Some(el) = target.dyn_ref::<Element>() {
                let is_close = el.closest("[data-dismiss=\"modal\"]").ok().flatten().is_some();
                let is_backdrop = el.id() == "rs-backdrop";
                if is_close || is_backdrop {
                    hide_modals();
                }
            }
        }
    }) as Box<dyn FnMut(MouseEvent)>);
    web_sys::window()
        .unwrap()
        .add_event_listener_with_callback("click", cb2.as_ref().unchecked_ref())
        .unwrap();
    cb2.forget();
}

fn setup_raf() {
    let f: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>> = Rc::new(RefCell::new(None));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::wrap(Box::new(move |ts: f64| {
        with_app(|app| app.step(ts));
        request_animation_frame(f.borrow().as_ref().unwrap());
    }) as Box<dyn FnMut(f64)>));
    request_animation_frame(g.borrow().as_ref().unwrap());
    // leak g intentionally: the loop runs for the lifetime of the page
    std::mem::forget(g);
}
