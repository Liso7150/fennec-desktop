//! Fennec — native Rust (egui) desktop coding-agent workspace.
//!
//! Everything on screen comes from the two loopback services the suite already
//! ships: the fennec-core agent server (`fennec-agent serve`: tasks, tool
//! events, jail switches) and the Fennec engine (`fennec serve`: chat tokens,
//! model identity, throughput). Both are reached through `fennec_client`, the
//! crate owned by fennec-core, and the workspace list comes from the installer
//! manifest at `fennec-environment-setup/repos.json`. A capability the
//! backends do not expose — subagent registry, artifact store, search sources,
//! test verdicts beyond the commands actually run — is labelled unavailable
//! rather than faked.
// A desktop app must not leave a console window next to it; debug builds keep
// one so `cargo run` still shows stderr.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use eframe::egui;
use fennec_client::engine::{ChatDelta, Engine};
use fennec_client::{git, suite, Core, Outlet};
use fennec_desktop::events::{self, ChatMessage, Event, Role, Task, TaskStatus, Timeline, ToolStatus};
use fennec_desktop::fonts;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

// Colibri Night: warm charcoal ground, terracotta primary, no blue anywhere.
const BG: egui::Color32 = egui::Color32::from_rgb(21, 18, 15);
const PANEL: egui::Color32 = egui::Color32::from_rgb(27, 23, 20);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(40, 33, 26);
const HOVER: egui::Color32 = egui::Color32::from_rgb(53, 34, 24);
const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 50, 42);
const TEXT: egui::Color32 = egui::Color32::from_rgb(242, 238, 232);
const TEXT2: egui::Color32 = egui::Color32::from_rgb(169, 157, 145);
const BRAND: egui::Color32 = egui::Color32::from_rgb(233, 134, 93);
const OK: egui::Color32 = egui::Color32::from_rgb(94, 201, 143);
const WARN: egui::Color32 = egui::Color32::from_rgb(224, 164, 88);
const ERR: egui::Color32 = egui::Color32::from_rgb(239, 97, 92);

fn status_color(s: TaskStatus) -> egui::Color32 {
    match s {
        TaskStatus::Running => BRAND,
        TaskStatus::Complete => OK,
        TaskStatus::NeedsAttention => ERR,
        TaskStatus::Waiting => TEXT2,
    }
}

fn tool_color(s: ToolStatus) -> egui::Color32 {
    match s {
        ToolStatus::Running => BRAND,
        ToolStatus::Complete => OK,
        ToolStatus::Warning => WARN,
        ToolStatus::Failed => ERR,
    }
}

/// What the supervisor thread pushes to the UI. Every field is reported-or-
/// unknown; nothing defaults to a value that merely looks plausible.
#[derive(Clone, Default)]
struct Env {
    core_up: bool,
    core_workspace: String,
    engine_up: bool,
    engine_url: String,
    model: String,
    ctx: String,
    tok_s: String,
    outlets: Vec<Outlet>,
    outlets_known: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Nav {
    Coding,
    Attention,
}

enum UiEvent {
    Task(String, Event),
    Env(Env),
    Sessions(Vec<Task>),
    Line(String),
}

struct App {
    suite: suite::Suite,
    workspaces: Vec<PathBuf>,
    ws: usize,
    core: Core,
    engine: Engine,
    env: Env,
    tasks: Vec<Task>,
    seen_at: HashMap<String, String>,
    selected: String,
    timelines: HashMap<String, Timeline>,
    composer: String,
    allow_write: bool,
    allow_shell: bool,
    allow_web: bool,
    nav: Nav,
    right_open: bool,
    search_open: bool,
    search: String,
    expanded: HashSet<String>,
    preview: Option<events::Artifact>,
    line: String,
    chat_cancel: Option<Arc<AtomicBool>>,
    rx: Receiver<UiEvent>,
    tx: Sender<UiEvent>,
    mono_font: bool,
    ui_font: bool,
    brand: Option<egui::TextureHandle>,
    busy: bool,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let s = suite::discover();
        let workspaces = s.workspaces();
        let core = Core::new();
        let engine = Engine::new();
        let tx2 = tx.clone();
        let core0 = core.clone();
        let engine0 = engine.clone();
        std::thread::spawn(move || loop {
            let mut env = Env { engine_url: engine0.base.clone(), ..Default::default() };
            if let Some(h) = core0.health() {
                env.core_up = true;
                env.core_workspace = h["workspace"].as_str().unwrap_or("").to_string();
                if let Some(o) = core0.outlets() {
                    env.outlets_known = true;
                    env.outlets = o.list().into_iter().cloned().collect();
                }
                let mut tasks = Vec::new();
                for sess in core0.sessions() {
                    let id = sess["id"].as_str().unwrap_or("").to_string();
                    if id.is_empty() {
                        continue;
                    }
                    tasks.push(Task {
                        title: truncate(sess["task"].as_str().unwrap_or("task"), 58),
                        goal: sess["task"].as_str().unwrap_or("").to_string(),
                        updated: String::new(),
                        status: TaskStatus::from_core(sess["status"].as_str().unwrap_or("")),
                        draft: false,
                        id,
                    });
                }
                let _ = tx2.send(UiEvent::Sessions(tasks));
            }
            if let Some(h) = engine0.health() {
                env.engine_up = true;
                if let Some(m) = h["model"].as_str() {
                    env.model = m.to_string();
                }
                if let Some((id, len)) = engine0.model_id() {
                    if env.model.is_empty() {
                        env.model = id;
                    }
                    if len > 0 {
                        env.ctx = format!("{len} tokens");
                    }
                }
                if let Some(mt) = engine0.metrics() {
                    if let Some(t) = mt["tokens_per_second"].as_f64() {
                        env.tok_s = format!("{t:.2} tok/s");
                    }
                }
            }
            let _ = tx2.send(UiEvent::Env(env));
            std::thread::sleep(Duration::from_millis(2000));
        });
        Self {
            suite: s, workspaces, ws: 0, core, engine, env: Env::default(),
            tasks: Vec::new(), seen_at: HashMap::new(), selected: String::new(),
            timelines: HashMap::new(), composer: String::new(),
            allow_write: false, allow_shell: false, allow_web: false,
            nav: Nav::Coding, right_open: true, search_open: false, search: String::new(),
            expanded: HashSet::new(), preview: None, line: String::new(),
            chat_cancel: None, rx, tx, mono_font: false, ui_font: false, brand: None, busy: false,
        }
    }

    fn workspace(&self) -> String {
        self.workspaces.get(self.ws).map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.suite.root.to_string_lossy().into_owned())
    }

    fn workspace_name(&self) -> String {
        self.workspaces.get(self.ws).and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "no workspace found".into())
    }

    fn tl(&mut self, id: &str) -> &mut Timeline {
        self.timelines.entry(id.to_string()).or_default()
    }

    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                UiEvent::Task(id, e) => {
                    let is_tool = matches!(e, Event::Tool(_));
                    let tl = self.timelines.entry(id.clone()).or_default();
                    tl.apply(e);
                    if is_tool {
                        tl.plan = events::plan_from_tools(&tl.tools);
                    }
                    let status = tl.status;
                    self.busy = tl.streaming || tl.tools.iter().any(|c| c.status == ToolStatus::Running);
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                        t.status = status;
                    }
                }
                UiEvent::Env(e) => self.env = e,
                UiEvent::Sessions(list) => {
                    for t in list {
                        let entry = self.seen_at.entry(t.id.clone()).or_insert_with(events::now_stamp).clone();
                        let mut t = t;
                        t.updated = time_ago(&entry);
                        match self.tasks.iter_mut().find(|x| x.id == t.id) {
                            Some(existing) => {
                                existing.status = t.status;
                                existing.updated = t.updated;
                            }
                            None => {
                                self.timelines.entry(t.id.clone()).or_default().status = t.status;
                                self.tasks.push(t);
                            }
                        }
                    }
                    if self.selected.is_empty() {
                        self.selected = self.tasks.first().map(|t| t.id.clone()).unwrap_or_default();
                    }
                }
                UiEvent::Line(s) => self.line = s,
            }
        }
    }

    fn new_draft(&mut self) -> String {
        let id = format!("draft-{}", millis());
        self.tasks.insert(0, Task {
            id: id.clone(),
            title: "New task".into(),
            status: TaskStatus::Waiting,
            updated: "now".into(),
            goal: String::new(),
            draft: true,
        });
        self.selected = id.clone();
        self.composer.clear();
        id
    }

    /// Sending has exactly two real paths, chosen by which backend answers:
    /// the agent loop (tool events, jail switches) or the engine's chat stream.
    /// With neither reachable the prompt stays on screen and the app says so.
    fn send(&mut self) {
        let text = std::mem::take(&mut self.composer);
        if text.trim().is_empty() {
            return;
        }
        self.line.clear();
        if self.env.core_up {
            let workspace = self.workspace();
            match self.core.start(&text, &workspace, self.allow_write, self.allow_shell, self.allow_web) {
                Ok(id) => {
                    let tl = self.tl(&id);
                    tl.messages.push(ChatMessage { role: Role::User, text: text.clone(), time: events::now_stamp() });
                    tl.status = TaskStatus::Running;
                    tl.streaming = true;
                    self.tasks.insert(0, Task {
                        id: id.clone(), title: truncate(&text, 58), status: TaskStatus::Running,
                        updated: "now".into(), goal: text, draft: false,
                    });
                    self.selected = id.clone();
                    self.seen_at.insert(id.clone(), events::now_stamp());
                    self.busy = true;
                    let core = self.core.clone();
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let mut after = 0usize;
                        loop {
                            match core.poll(&id, after) {
                                Ok(v) => {
                                    let steps: Vec<Value> = v["steps"].as_array().cloned().unwrap_or_default();
                                    if !steps.is_empty() {
                                        after += steps.len();
                                    }
                                    for s in &steps {
                                        for e in events::map_step(s) {
                                            let _ = tx.send(UiEvent::Task(id.clone(), e));
                                        }
                                    }
                                    if v["status"].as_str().map(|s| s != "running").unwrap_or(true) {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(UiEvent::Line(format!("Polling stopped: {e}")));
                                    break;
                                }
                            }
                            std::thread::sleep(Duration::from_millis(300));
                        }
                    });
                }
                Err(e) => self.line = format!("Agent server refused the task: {e}"),
            }
            return;
        }
        if self.env.engine_up {
            let id = if self.selected.starts_with("draft-") { self.selected.clone() } else { self.new_draft() };
            let mut messages: Vec<Value> = self.timelines.get(&id).map(|tl| tl.messages.iter()
                .filter(|m| matches!(m.role, Role::User | Role::Assistant))
                .map(|m| json!({"role": if m.role == Role::User { "user" } else { "assistant" }, "content": m.text}))
                .collect()).unwrap_or_default();
            messages.push(json!({"role": "user", "content": text.clone()}));
            {
                let tl = self.tl(&id);
                tl.messages.push(ChatMessage { role: Role::User, text: text.clone(), time: events::now_stamp() });
                tl.streaming = true;
                tl.status = TaskStatus::Running;
            }
            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                t.title = truncate(&text, 58);
                t.goal = text;
                t.status = TaskStatus::Running;
            }
            let engine = self.engine.clone();
            let flag = Arc::new(AtomicBool::new(false));
            self.chat_cancel = Some(flag.clone());
            let (ltx, lrx) = mpsc::channel::<ChatDelta>();
            let tx = self.tx.clone();
            std::thread::spawn(move || engine.chat_stream(&json!(messages), 512, &ltx, &flag));
            std::thread::spawn(move || {
                for d in lrx {
                    let ev = match d {
                        ChatDelta::Reasoning(r) => UiEvent::Task(id.clone(), Event::Message(ChatMessage { role: Role::Progress, text: r, time: events::now_stamp() })),
                        ChatDelta::Content(c) => UiEvent::Task(id.clone(), Event::Partial(c)),
                        ChatDelta::Done => UiEvent::Task(id.clone(), Event::Status(TaskStatus::Complete)),
                        ChatDelta::Failed(e) => UiEvent::Line(format!("Engine stream failed: {e}")),
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
            });
            self.busy = true;
            return;
        }
        let id = if self.selected.is_empty() { self.new_draft() } else { self.selected.clone() };
        self.tl(&id).messages.push(ChatMessage { role: Role::User, text, time: events::now_stamp() });
        self.line = "No backend reachable — start the engine (`fennec serve`) or the agent server (`fennec-agent serve`).".to_string();
    }

    fn stop(&mut self) {
        if let Some(flag) = &self.chat_cancel {
            flag.store(true, Ordering::Relaxed);
        }
        if !self.selected.is_empty() && !self.selected.starts_with("draft-") {
            match self.core.cancel(&self.selected) {
                Ok(()) => self.line = "Cancellation requested.".into(),
                Err(e) => self.line = format!("Cancel failed: {e}"),
            }
        }
        if !self.selected.is_empty() {
            let id = self.selected.clone();
            self.tl(&id).apply(Event::Cancelled);
        }
        self.busy = false;
    }

    /// The shared context-budgeting crate does this for every Fennec surface;
    /// the desktop only reports what it changed.
    fn compact_selected(&mut self) {
        let Some(tl) = self.cur_timeline().cloned() else {
            self.line = "No task selected to compact.".into();
            return;
        };
        let msgs: Vec<Value> = tl.messages.iter()
            .filter(|m| matches!(m.role, Role::User | Role::Assistant))
            .map(|m| json!({"role": if m.role == Role::User { "user" } else { "assistant" }, "content": m.text}))
            .collect();
        let opts = fennec_economy::Options::sized(fennec_economy::DEFAULT_BUDGET_CHARS, fennec_economy::DEFAULT_RESULT_CHARS);
        let (_payload, stats) = fennec_economy::compact(&msgs, &opts, &mut HashMap::new());
        self.line = format!("Compacted {} → {} chars ({} elided, {} deduped).",
            stats.chars_before, stats.chars_after, stats.elided, stats.deduped);
    }

    // ---- panes ----

    fn left_rail(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            brand_mark(ui, &self.brand, 16.0);
            ui.label(egui::RichText::new("Fennec").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(if self.busy { "working" } else { "idle" })
                    .size(11.0).color(if self.busy { BRAND } else { TEXT2 }));
            });
        });
        ui.add_space(2.0);
        ui.label(egui::RichText::new("WORKSPACE").size(10.5).color(TEXT2));
        let paths = self.workspaces.clone();
        let name = self.workspace_name();
        egui::ComboBox::from_id_source("workspace").selected_text(name).width(212.0).show_ui(ui, |ui| {
            for (i, p) in paths.iter().enumerate() {
                let label = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.selectable_value(&mut self.ws, i, label).clicked() {
                    self.line = format!("Workspace: {}", pretty_path(&p.to_string_lossy()));
                }
            }
        });
        if self.workspaces.is_empty() {
            ui.label(egui::RichText::new("No suite repositories found — run `fennec update`.")
                .size(11.0).color(WARN));
        }
        ui.separator();
        if ui.selectable_label(self.nav == Nav::Coding, "Coding").clicked() {
            self.nav = Nav::Coding;
        }
        if ui.selectable_label(self.nav == Nav::Attention, "My Attention").clicked() {
            self.nav = Nav::Attention;
        }
        if ui.selectable_label(false, "New Task").on_hover_text("Ctrl+N").clicked() {
            self.new_draft();
        }
        if ui.selectable_label(self.search_open, "Search").on_hover_text("Ctrl+K").clicked() {
            self.search_open = !self.search_open;
        }
        ui.separator();
        ui.label(egui::RichText::new("RECENT TASKS").size(10.5).color(TEXT2));
        egui::ScrollArea::vertical().id_source("tasks").max_height(340.0).show(ui, |ui| {
            let ids: Vec<String> = self.tasks.iter()
                .filter(|t| match self.nav {
                    Nav::Coding => true,
                    Nav::Attention => matches!(t.status, TaskStatus::NeedsAttention | TaskStatus::Waiting) && !t.draft,
                })
                .map(|t| t.id.clone()).collect();
            let mut pick: Option<String> = None;
            for id in ids {
                let Some(task) = self.tasks.iter().find(|t| t.id == id) else { continue };
                let (title, status, updated) = (task.title.clone(), task.status, task.updated.clone());
                let sel = self.selected == task.id;
                if ui.selectable_label(sel, egui::RichText::new(title).size(12.5).color(if sel { BRAND } else { TEXT })).clicked() {
                    pick = Some(id.clone());
                }
                ui.horizontal(|ui| {
                    ui.colored_label(status_color(status), "●");
                    ui.label(egui::RichText::new(format!("{} · {}", if updated.is_empty() { "-" } else { &updated }, status.label()))
                        .size(10.5).color(TEXT2));
                });
            }
            if let Some(id) = pick {
                self.selected = id;
            }
            if self.tasks.is_empty() {
                ui.label(egui::RichText::new("No sessions yet. Send a prompt to start one.").size(11.5).color(TEXT2));
            }
        });
        ui.separator();
        let ws = self.workspace();
        let ctx = git::repo_context(&ws);
        ui.label(egui::RichText::new(format!("{} · {}", self.workspace_name(), if ctx.branch.is_empty() { "-" } else { &ctx.branch }))
            .monospace().size(11.0).color(TEXT2));
        ui.label(egui::RichText::new(ctx.changed_label()).size(11.0).color(TEXT2));
        ui.label(egui::RichText::new(format!("local mode · {}", self.env.engine_url)).size(10.5).color(TEXT2));
    }

    fn right_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_source("context").show(ui, |ui| {
            ui.label(egui::RichText::new("Environment").size(11.0).color(TEXT2).strong());
            let runtime = format!("Rust · egui · {}", std::env::consts::OS);
            let loop_state = if self.env.core_up { "online".to_string() } else { "offline".to_string() };
            let engine_state = if self.env.engine_up { "online".to_string() } else { "offline".to_string() };
            let model = if self.env.model.is_empty() { "unknown".to_string() } else { self.env.model.clone() };
            let ctx = if self.env.ctx.is_empty() { "unknown".to_string() } else { self.env.ctx.clone() };
            let rate = if self.env.tok_s.is_empty() { "no samples".to_string() } else { self.env.tok_s.clone() };
            let local_ws = self.workspace();
            let work = pretty_path(if self.env.core_workspace.is_empty() { &local_ws } else { &self.env.core_workspace });
            let faces = format!("{} / {}", if self.mono_font { "mono installed" } else { "egui default" }, if self.ui_font { "ui installed" } else { "egui default" });
            let rows: Vec<(&str, String, Option<bool>)> = vec![
                ("Runtime", runtime, None),
                ("Agent loop", loop_state, Some(self.env.core_up)),
                ("Engine", engine_state, Some(self.env.engine_up)),
                ("Model", model, None),
                ("Context", ctx, None),
                ("Throughput", rate, None),
                ("Workspace", work, None),
                ("Fonts", faces, None),
            ];
            egui::Grid::new("env").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                for (k, v, good) in rows {
                    ui.label(egui::RichText::new(k).size(11.5).color(TEXT2));
                    let col = match good {
                        Some(true) => OK,
                        Some(false) => ERR,
                        None => TEXT,
                    };
                    ui.label(egui::RichText::new(v).monospace().size(11.5).color(col));
                    ui.end_row();
                }
            });
            if !self.env.core_up && ui.button("Start agent loop").clicked() {
                let s = self.suite.clone();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    let ok = s.serve(8787);
                    let _ = tx.send(UiEvent::Line(if ok {
                        "Agent loop started.".into()
                    } else {
                        "Could not start the agent loop — install fennec-core and its dependencies, then run `fennec-agent serve`.".into()
                    }));
                });
            }
            if !self.line.is_empty() {
                ui.label(egui::RichText::new(&self.line).size(11.5).color(WARN));
            }
            ui.separator();

            let tl = self.cur_timeline().cloned().unwrap_or_default();

            ui.label(egui::RichText::new("Subagents").size(11.0).color(TEXT2).strong());
            let peers = tl.tools.iter().filter(|c| c.category == "PEER" || c.category == "WEB").count();
            if peers > 0 {
                ui.label(egui::RichText::new(format!("{peers} delegation card(s) in this run; the loop keeps no live subagent registry."))
                    .size(11.5).color(TEXT2));
            } else {
                ui.label(egui::RichText::new("Unavailable — the backend exposes no subagent registry.")
                    .size(11.5).color(TEXT2));
            }
            ui.separator();

            ui.label(egui::RichText::new("Artifacts").size(11.0).color(TEXT2).strong());
            let arts = tl.artifacts();
            if arts.is_empty() {
                ui.label(egui::RichText::new("No files written by this run.").size(11.5).color(TEXT2));
            }
            let mut open: Option<events::Artifact> = None;
            for a in &arts {
                let resp = ui.horizontal(|ui| {
                    ui.colored_label(if a.status == "Failed" { ERR } else { OK }, "●");
                    ui.link(egui::RichText::new(format!("{}  ·  {}", a.name, a.change)).size(11.5))
                });
                if resp.inner.clicked() {
                    open = Some(a.clone());
                }
            }
            if let Some(a) = open {
                self.preview = Some(a);
            }
            ui.separator();

            ui.label(egui::RichText::new("Research").size(11.0).color(TEXT2).strong());
            let web = tl.tools.iter().filter(|c| c.category == "WEB").count();
            if web > 0 {
                ui.label(egui::RichText::new(format!("{web} web_task run(s) — the loop returns results, not a source list."))
                    .size(11.5).color(TEXT2));
            } else {
                ui.label(egui::RichText::new("Unavailable — the backend has no search-source registry.")
                    .size(11.5).color(TEXT2));
            }
            for (ready, text) in outlet_rows(&self.env) {
                ui.horizontal(|ui| {
                    ui.colored_label(if ready { OK } else { TEXT2 }, "●");
                    ui.label(egui::RichText::new(text).size(11.0).color(TEXT2));
                });
            }
            ui.separator();

            ui.label(egui::RichText::new("Verification").size(11.0).color(TEXT2).strong());
            let runs: Vec<(String, ToolStatus, String)> = tl.tools.iter()
                .filter(|c| matches!(c.category.as_str(), "TEST" | "CMD"))
                .map(|c| (c.title.clone(), c.status, c.duration.clone()))
                .collect();
            if runs.is_empty() {
                ui.label(egui::RichText::new("No commands run in this task yet.").size(11.5).color(TEXT2));
            }
            for (title, st, dur) in runs {
                ui.horizontal(|ui| {
                    ui.colored_label(tool_color(st), "●");
                    ui.label(egui::RichText::new(format!("{title} · {}", st.label())).size(11.5).color(TEXT));
                    if !dur.is_empty() {
                        ui.label(egui::RichText::new(dur).monospace().size(10.5).color(TEXT2));
                    }
                });
            }
            ui.separator();
            if ui.button("Compact this task's context").clicked() {
                self.compact_selected();
            }
        });
    }

    fn cur_timeline(&self) -> Option<&Timeline> {
        if self.selected.is_empty() {
            return None;
        }
        self.timelines.get(&self.selected)
    }

    fn center(&mut self, ui: &mut egui::Ui) {
        let (core_up, engine_up, model) = (self.env.core_up, self.env.engine_up, self.env.model.clone());
        // The composer is a bottom panel drawn before the timeline so it
        // reserves its own height: a ScrollArea with auto_shrink off takes the
        // whole central rect, and anything added after it lands below the edge.
        let mut send_now = false;
        egui::TopBottomPanel::bottom("composer")
            .frame(egui::Frame::none().fill(PANEL).rounding(8.0).inner_margin(8.0))
            .show_inside(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.composer)
                    .desired_rows(2).hint_text("Ask Fennec to inspect, edit, test, or explain your code")
                    .desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut self.allow_write, "edits").on_hover_text("the loop's allow_write switch");
                    ui.toggle_value(&mut self.allow_shell, "commands").on_hover_text("the loop's allow_shell switch");
                    ui.toggle_value(&mut self.allow_web, "web").on_hover_text("the loop's allow_web outlet switch");
                    if !core_up {
                        ui.label(egui::RichText::new("jail switches apply to the agent loop").size(10.5).color(TEXT2));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !model.is_empty() {
                            ui.label(egui::RichText::new(&model).monospace().size(11.0).color(TEXT2));
                        }
                        let enabled = !self.composer.trim().is_empty();
                        if ui.add_enabled(enabled, egui::Button::new(egui::RichText::new("Send  ↑").color(egui::Color32::from_rgb(34, 18, 13))).fill(BRAND)).clicked() {
                            send_now = true;
                        }
                    });
                });
            });
        if send_now {
            self.send();
        }
        let ws = self.workspace_name();
        let branch = git::repo_context(&self.workspace()).branch;
        ui.label(egui::RichText::new(format!("{ws} / {}", if branch.is_empty() { "-" } else { &branch }))
            .monospace().size(11.0).color(TEXT2));
        let title = self.tasks.iter().find(|t| t.id == self.selected).map(|t| t.title.clone()).unwrap_or_else(|| "No task selected".into());
        let status = self.cur_timeline().map(|t| t.status).unwrap_or(TaskStatus::Waiting);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).size(19.0).strong().color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Context").clicked() {
                    self.right_open = !self.right_open;
                }
                if self.busy && ui.small_button("Stop").clicked() {
                    self.stop();
                }
                ui.colored_label(status_color(status), format!("● {}", status.label()));
            });
        });
        if !core_up {
            let note = if engine_up {
                "Agent loop offline — direct chat mode: tokens stream from the engine; the tool timeline is unavailable."
            } else {
                "No backend reachable — run `fennec serve` (engine) or `fennec-agent serve` (agent loop)."
            };
            egui::Frame::none().fill(HOVER).rounding(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.label(egui::RichText::new(note).size(12.0).color(WARN));
            });
        }
        ui.separator();
        let id = self.selected.clone();
        let tl = self.timelines.entry(id).or_default().clone();
        egui::ScrollArea::vertical().id_source("timeline").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            if tl.messages.is_empty() && tl.tools.is_empty() && tl.summary.is_empty() {
                ui.label(egui::RichText::new("Nothing has happened in this task yet. The timeline fills as the loop reads, edits, runs and tests.")
                    .size(13.0).color(TEXT2));
            }
            for m in &tl.messages {
                let (who, col) = match m.role {
                    Role::User => ("You", TEXT),
                    Role::Assistant => ("Fennec", BRAND),
                    Role::Progress => ("Agent", TEXT2),
                };
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(who).size(11.5).strong().color(col));
                    ui.label(egui::RichText::new(&m.time).monospace().size(10.5).color(TEXT2));
                });
                render_rich(ui, &m.text, m.role == Role::Progress);
                ui.add_space(10.0);
            }
            if tl.streaming && !tl.streaming_text.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Fennec").size(11.5).strong().color(BRAND));
                    ui.label(egui::RichText::new("streaming").size(10.5).color(TEXT2));
                });
                render_rich(ui, &tl.streaming_text, false);
            }
            if !tl.plan.is_empty() {
                egui::Frame::none().fill(PANEL).rounding(6.0).inner_margin(8.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("Plan").size(12.0).strong().color(TEXT2));
                    for p in &tl.plan {
                        ui.label(egui::RichText::new(format!("{}  {}", if p.done { "✓" } else { "→" }, p.content))
                            .size(12.0).color(if p.done { OK } else { TEXT2 }));
                    }
                });
                ui.add_space(10.0);
            }
            let mut toggle: Option<String> = None;
            for card in &tl.tools {
                let open = self.expanded.contains(&card.id);
                egui::Frame::none().fill(SURFACE).rounding(6.0).inner_margin(egui::Margin::symmetric(8.0, 6.0)).show(ui, |ui| {
                    ui.push_id(&card.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&card.category).monospace().size(10.0).color(BG).background_color(BRAND));
                            ui.label(egui::RichText::new(&card.title).size(12.5).color(TEXT));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !card.duration.is_empty() {
                                    ui.label(egui::RichText::new(&card.duration).monospace().size(10.5).color(TEXT2));
                                }
                                ui.colored_label(tool_color(card.status), format!("● {}", card.status.label()));
                            });
                        });
                        ui.label(egui::RichText::new(&card.detail).monospace().size(11.0).color(TEXT2));
                        if !card.output.is_empty() && expand_button(ui, open).clicked() {
                            toggle = Some(card.id.clone());
                        }
                    });
                });
                if open && !card.output.is_empty() {
                    egui::Frame::none().fill(BG).rounding(6.0).inner_margin(8.0).show(ui, |ui| {
                        egui::ScrollArea::vertical().id_source(format!("out-{}", card.id)).max_height(160.0).show(ui, |ui| {
                            ui.label(egui::RichText::new(&card.output).monospace().size(11.5).color(TEXT));
                        });
                    });
                }
                ui.add_space(6.0);
            }
            if !tl.summary.is_empty() {
                egui::Frame::none().fill(PANEL).rounding(6.0).inner_margin(10.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("Summary").size(11.0).color(TEXT2).strong());
                    ui.label(egui::RichText::new(&tl.summary).size(13.0).color(OK));
                });
            }
            if let Some(k) = toggle {
                if self.expanded.contains(&k) {
                    self.expanded.remove(&k);
                } else {
                    self.expanded.insert(k);
                }
            }
        });
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        if let Some(a) = self.preview.clone() {
            let mut close = false;
            egui::Window::new(format!("{}  ({})", a.name, a.change)).default_width(560.0).show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("status: {}", a.status)).monospace().size(11.0).color(TEXT2));
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(&a.preview).monospace().size(12.0));
                });
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if close {
                self.preview = None;
            }
        }
        if self.search_open {
            let mut close = false;
            egui::Window::new("Search tasks").default_width(420.0).show(ctx, |ui| {
                ui.text_edit_singleline(&mut self.search);
                let q = self.search.to_lowercase();
                let hits: Vec<Task> = self.tasks.iter()
                    .filter(|t| q.is_empty() || t.title.to_lowercase().contains(&q) || t.goal.to_lowercase().contains(&q))
                    .cloned().collect();
                let mut pick: Option<String> = None;
                for t in hits.iter().take(20) {
                    if ui.selectable_label(false, truncate(&t.title, 60)).clicked() {
                        pick = Some(t.id.clone());
                    }
                }
                if hits.is_empty() {
                    ui.label(egui::RichText::new("No matching task.").size(12.0).color(TEXT2));
                }
                if let Some(id) = pick {
                    self.selected = id;
                    close = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
            if close {
                self.search_open = false;
            }
        }
    }
}

fn expand_button(ui: &mut egui::Ui, open: bool) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(if open { "Hide result" } else { "Show result" }).size(10.5).color(TEXT2))
        .fill(HOVER).small())
}

/// Chat text with ``` fences turned into copyable monospace blocks.
fn render_rich(ui: &mut egui::Ui, text: &str, dim: bool) {
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let before = &rest[..open];
        if !before.trim().is_empty() {
            ui.label(egui::RichText::new(before.trim()).size(if dim { 12.0 } else { 13.5 }).color(if dim { TEXT2 } else { TEXT }));
        }
        let after = &rest[open + 3..];
        let (body, tail) = match after.find("```") {
            Some(end) => (&after[..end], &after[end + 3..]),
            None => (after, ""),
        };
        let body = body.trim().to_string();
        egui::Frame::none().fill(SURFACE).rounding(6.0).inner_margin(8.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&body).monospace().size(12.0).color(TEXT));
                let copy = body.clone();
                if ui.small_button("copy").clicked() {
                    ui.output_mut(|o| o.copied_text = copy);
                }
            });
        });
        rest = tail;
    }
    if !rest.trim().is_empty() {
        ui.label(egui::RichText::new(rest.trim()).size(if dim { 12.0 } else { 13.5 }).color(if dim { TEXT2 } else { TEXT }));
    }
}

fn truncate(s: &str, n: usize) -> String {
    let one = s.lines().next().unwrap_or(s).trim();
    if one.chars().count() > n {
        format!("{}…", one.chars().take(n).collect::<String>())
    } else {
        one.to_string()
    }
}

fn millis() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()
}

/// The core's session records carry no timestamps, so the rail reports how long
/// ago this app first saw the session — nothing more precise is known.
fn time_ago(stamp: &str) -> String {
    let now = events::now_stamp();
    let mins = |s: &str| -> Option<i64> {
        let mut it = s.split(':');
        Some(it.next()?.parse::<i64>().ok()? * 60 + it.next()?.parse::<i64>().ok()?)
    };
    match (mins(stamp), mins(&now)) {
        (Some(a), Some(b)) if b >= a => {
            let d = b - a;
            if d < 2 { "now".into() } else if d < 60 { format!("{d}m ago") } else { format!("{}h ago", d / 60) }
        }
        _ => stamp.to_string(),
    }
}

/// One `(installed, line)` row per outlet the agent server reported. The three
/// states are three different claims: never probed, probed but empty, and
/// probed with real data — so an offline loop can never read as "not installed".
fn outlet_rows(env: &Env) -> Vec<(bool, String)> {
    if !env.outlets_known {
        return vec![(false, "Outlets: unknown (agent loop offline)".into())];
    }
    if env.outlets.is_empty() {
        return vec![(false, "Outlets: the agent server named none".into())];
    }
    env.outlets.iter().map(|o| (o.present, format!("{}: {}", o.label, o.detail))).collect()
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if self.brand.is_none() {
            self.brand = brand_texture(ctx);
        }
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::K) {
                self.search_open = !self.search_open;
            }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::N) {
                self.new_draft();
            }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter) {
                self.send();
            }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                self.search_open = false;
                self.preview = None;
            }
        });

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                brand_mark(ui, &self.brand, 14.0);
                ui.label(egui::RichText::new("Fennec").strong().size(13.0));
                ui.label(egui::RichText::new("local coding agent").size(11.5).color(TEXT2));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, col) = match (self.env.core_up, self.env.engine_up) {
                        (true, true) => ("agent loop · engine", OK),
                        (true, false) => ("agent loop · engine offline", WARN),
                        (false, true) => ("direct chat · engine", WARN),
                        (false, false) => ("no backend", ERR),
                    };
                    ui.label(egui::RichText::new(label).size(11.5).color(col));
                    ui.label(egui::RichText::new("local mode").size(11.5).color(TEXT2));
                });
            });
        });

        egui::SidePanel::left("left").resizable(true).default_width(252.0).min_width(210.0).max_width(340.0)
            .show(ctx, |ui| {
                self.left_rail(ui);
            });
        if self.right_open {
            egui::SidePanel::right("right").resizable(true).default_width(326.0).min_width(260.0).max_width(430.0)
                .show(ctx, |ui| {
                    self.right_panel(ui);
                });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            self.center(ui);
        });
        self.overlays(ctx);
        ctx.request_repaint_after(Duration::from_millis(150));
    }
}

/// The branded window/taskbar icon, decoded at build time from build/icon.png.
fn window_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(include_bytes!("../build/icon.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width: w, height: h })
}

/// The same mark as an in-window texture. A glyph would be simpler, but the
/// diamond the rail used is missing from several of the faces `fonts::install`
/// resolves, and a brand that renders as tofu in one font is not a brand.
fn brand_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(include_bytes!("../build/icon.png")).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let pixels = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture("fennec-brand", pixels, egui::TextureOptions::LINEAR))
}

fn brand_mark(ui: &mut egui::Ui, brand: &Option<egui::TextureHandle>, size: f32) {
    if let Some(t) = brand {
        ui.add(egui::Image::from_texture(t).fit_to_exact_size(egui::vec2(size, size)));
    }
}

/// Windows accepts both separators, so a manifest root written with forward
/// slashes and joined by `PathBuf` displays as `D:/Fennec-Suite\fennec-cli`.
/// Display normalises it; nothing on disk is renamed.
fn pretty_path(s: &str) -> String {
    if cfg!(windows) { s.replace('/', "\\") } else { s.to_string() }
}

fn main() -> Result<(), eframe::Error> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1500.0, 960.0]).with_min_inner_size([1180.0, 760.0])
        .with_title("Fennec");
    if let Some(icon) = window_icon() { viewport = viewport.with_icon(icon); }
    let opts = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Fennec", opts, Box::new(|cc| {
        let (mono, prop) = fonts::install(&cc.egui_ctx);
        cc.egui_ctx.style_mut(|s| {
            s.visuals.dark_mode = true;
            s.visuals.panel_fill = BG;
            s.visuals.window_fill = PANEL;
            s.visuals.extreme_bg_color = SURFACE;
            s.visuals.faint_bg_color = PANEL;
            s.visuals.widgets.noninteractive.bg_fill = PANEL;
            s.visuals.widgets.noninteractive.bg_stroke = egui::Stroke { width: 1.0, color: BORDER };
            s.visuals.widgets.inactive.bg_fill = SURFACE;
            s.visuals.widgets.hovered.bg_fill = HOVER;
            s.visuals.widgets.active.bg_fill = HOVER;
            s.visuals.selection.bg_fill = BRAND.linear_multiply(0.35);
            s.visuals.selection.stroke = egui::Stroke { width: 1.0, color: BRAND };
            s.visuals.hyperlink_color = BRAND;
            s.visuals.window_stroke = egui::Stroke { width: 1.0, color: BORDER };
            s.visuals.popup_shadow.blur = 10.0;
            s.visuals.popup_shadow.color = egui::Color32::from_black_alpha(140);
        });
        let mut app = App::new();
        app.mono_font = mono;
        app.ui_font = prop;
        Box::new(app)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_one_line_and_ellipses() {
        assert_eq!(truncate("first\nsecond", 40), "first");
        assert_eq!(truncate(&"x".repeat(80), 10), format!("{}…", "x".repeat(10)));
    }

    #[test]
    fn the_outlet_rail_keeps_not_probed_apart_from_probed_empty() {
        assert_eq!(outlet_rows(&Env::default()), vec![(false, "Outlets: unknown (agent loop offline)".to_string())]);
        let mut env = Env::default();
        env.outlets_known = true;
        assert_eq!(outlet_rows(&env), vec![(false, "Outlets: the agent server named none".to_string())]);
        env.outlets = vec![Outlet { id: "jev", label: "Jev", present: false,
                                    detail: "not installed".into(), agents: Vec::new() }];
        assert_eq!(outlet_rows(&env), vec![(false, "Jev: not installed".to_string())]);
    }

    #[test]
    fn a_fresh_stamp_reads_as_now() {
        assert_eq!(time_ago(&events::now_stamp()), "now");
    }
}
