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

// Fennec Night: warm charcoal ground, terracotta primary, no blue anywhere.
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

// Two corner steps, per the brief's 10-12 px: inset surfaces at 10, raised and
// floating ones at 12.
const RADIUS: f32 = 10.0;
const RADIUS_RAISED: f32 = 12.0;

/// Cmd on macOS, Ctrl everywhere else. `Modifiers::COMMAND` is egui's own
/// spelling of that rule; the hint text has to name the key that is bound.
const MOD: egui::Modifiers = egui::Modifiers::COMMAND;
const MOD_HINT: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

/// How long a toast stays up. It is a notice, not a log line.
const TOAST_TTL: Duration = Duration::from_secs(6);

/// Why each composer affordance is disabled. Each statement was checked
/// against the backend it describes, so the tooltip is a fact, not a hedge.
const VISION_REFUSED: &str =
    "Unavailable — the attached engine analyzes no images. It refuses a request that carries \
     a non-text content part instead of quietly dropping the image.";
const VISION_NO_PICKER: &str =
    "Unavailable — this shell records no image attachment: there is no picker and nothing in \
     the request it sends, so even a vision-capable engine would have nothing to look at.";
const UNAVAILABLE_DICTATION: &str =
    "Unavailable — this shell has no microphone capture. The agent server does answer \
     /api/stt with a transcription, but nothing here records audio to send it yet.";
const UNAVAILABLE_TTS: &str =
    "Unavailable — neither backend exposes a speech-synthesis route, so there is nothing \
     to read aloud.";

fn status_color(s: TaskStatus) -> egui::Color32 {
    match s {
        TaskStatus::Running => BRAND,
        TaskStatus::Complete => OK,
        TaskStatus::NeedsAttention => ERR,
        TaskStatus::Waiting => TEXT2,
        // The run is stopped on the user, not on the model — the one state in
        // this list that only an answer here can clear.
        TaskStatus::AwaitingApproval => WARN,
    }
}

/// The backend's own risk tiers, in the palette's three signal colours. A risk
/// the gate table did not name gets the neutral colour, never green.
fn risk_color(risk: &str) -> egui::Color32 {
    match risk {
        "high" => ERR,
        "medium" => WARN,
        "low" => OK,
        _ => TEXT2,
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
    // Only the engine knows whether it can look at pixels. `None` means the
    // attached engine never answered, which is not the same claim as `false`.
    vision: Option<bool>,
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
    /// The `pending` list from one `/api/agent/poll`: the approval questions the
    /// loop still holds. A card missing from it cannot be answered any more.
    Pending(String, Vec<String>),
    /// What the service said about one click on an approval card — session,
    /// question, the option chosen, and the reason it did not land, if it did
    /// not. Nothing here claims the call ran; that is the `tool` step's job.
    Answered(String, String, String, Option<String>),
    Env(Env),
    Sessions(Vec<Task>),
    Line(String),
    /// `git diff -- path` for one artifact, answered off the UI thread.
    Diff(String, Result<String, String>),
}

/// A notice about something that happened outside the pane being looked at.
/// It expires on its own — it is not a log the user has to clear.
#[derive(Clone)]
struct Toast {
    text: String,
    good: bool,
    at: std::time::Instant,
}

/// What `git diff` says about one artifact path.
struct Diff {
    path: String,
    pending: bool,
    text: Option<String>,
    note: String,
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
    /// `require_approval`: the switches stop admitting calls and every gated
    /// one is asked about instead. The answer, not a box ticked at start, is
    /// what lets a write through.
    ask_each: bool,
    nav: Nav,
    right_open: bool,
    search_open: bool,
    search: String,
    expanded: HashSet<String>,
    preview: Option<events::Artifact>,
    diff: Option<Diff>,
    toasts: Vec<Toast>,
    jump_latest: bool,
    timeline_off_end: bool,
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
        #[cfg(not(test))]
        let tx2 = tx.clone();
        #[cfg(not(test))]
        let core0 = core.clone();
        #[cfg(not(test))]
        let engine0 = engine.clone();
        // The probe loop is the app talking to live services. A unit test must
        // not depend on whatever happens to be running on the machine, so the
        // thread exists only in a built binary; `paint` reads the same channels
        // either way, just from events the test injects.
        #[cfg(not(test))]
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
                    // `pending` is how many gated calls that run is waiting on.
                    // Without it a rail cannot tell a blocked run from a slow
                    // one, and a server that does not report it leaves the row
                    // exactly as the status string describes it.
                    let mut status = TaskStatus::from_core(sess["status"].as_str().unwrap_or(""));
                    if status == TaskStatus::Running && sess["pending"].as_u64().unwrap_or(0) > 0 {
                        status = TaskStatus::AwaitingApproval;
                    }
                    tasks.push(Task {
                        title: truncate(sess["task"].as_str().unwrap_or("task"), 58),
                        goal: sess["task"].as_str().unwrap_or("").to_string(),
                        updated: String::new(),
                        status,
                        draft: false,
                        id,
                    });
                }
                let _ = tx2.send(UiEvent::Sessions(tasks));
            }
            if let Some(h) = engine0.health() {
                env.engine_up = true;
                env.vision = engine_vision(&h);
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
            allow_write: false, allow_shell: false, allow_web: false, ask_each: false,
            nav: Nav::Coding, right_open: true, search_open: false, search: String::new(),
            expanded: HashSet::new(), preview: None, diff: None, toasts: Vec::new(),
            jump_latest: false, timeline_off_end: false, line: String::new(),
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
                    let before = self.timelines.get(&id).map(|t| t.status);
                    let tl = self.timelines.entry(id.clone()).or_default();
                    tl.apply(e);
                    let status = tl.status;
                    self.busy = tl.streaming || tl.tools.iter().any(|c| c.status == ToolStatus::Running);
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                        t.status = status;
                    }
                    // A task that finishes in another row is news, not a log
                    // line: it gets a toast, because nobody is looking at it.
                    if before != Some(status)
                        && id != self.selected
                        && matches!(status, TaskStatus::Complete | TaskStatus::NeedsAttention)
                    {
                        let title = self.tasks.iter().find(|t| t.id == id)
                            .map(|t| truncate(&t.title, 40))
                            .unwrap_or_else(|| "Task".to_string());
                        self.toast(format!("{title} · {}", status.label()), status == TaskStatus::Complete);
                    }
                }
                UiEvent::Pending(id, live) => {
                    let before = self.timelines.get(&id).map(|t| t.status);
                    let tl = self.timelines.entry(id.clone()).or_default();
                    tl.close_approvals(&live);
                    // Only a run that was already working changes here: a task
                    // that finished or failed keeps the status that says so.
                    tl.status = match (tl.status, tl.waiting_on_approval()) {
                        (TaskStatus::Running, true) | (TaskStatus::AwaitingApproval, true) => {
                            TaskStatus::AwaitingApproval
                        }
                        (TaskStatus::AwaitingApproval, false) => TaskStatus::Running,
                        (other, _) => other,
                    };
                    let status = tl.status;
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                        t.status = status;
                    }
                    if before != Some(status) && status == TaskStatus::AwaitingApproval
                        && id != self.selected
                    {
                        let title = self.tasks.iter().find(|t| t.id == id)
                            .map(|t| truncate(&t.title, 40))
                            .unwrap_or_else(|| "Task".to_string());
                        self.toast(format!("{title} · waiting for an answer"), false);
                    }
                }
                UiEvent::Answered(id, approval_id, decision, error) => {
                    let tl = self.timelines.entry(id).or_default();
                    tl.answer_approval(&approval_id, &decision, error);
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
                UiEvent::Diff(path, res) => {
                    let (text, note) = match res {
                        Ok(d) if d.trim().is_empty() => (
                            None,
                            "git reports no unstaged diff for this path — the change is committed, \
                             untracked, or outside this repo."
                                .to_string(),
                        ),
                        Ok(d) => (Some(d), String::new()),
                        Err(r) => (None, r),
                    };
                    self.diff = Some(Diff { path, pending: false, text, note });
                }
            }
        }
    }

    /// A toast expires on its own; only the four newest are kept on screen.
    fn toast(&mut self, text: impl Into<String>, good: bool) {
        self.toasts.push(Toast { text: text.into(), good, at: std::time::Instant::now() });
        if self.toasts.len() > 4 {
            let extra = self.toasts.len() - 4;
            self.toasts.drain(..extra);
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
            match self.core.start(&text, &workspace, self.allow_write, self.allow_shell,
                                  self.allow_web, self.ask_each) {
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
                                    let finished = v["status"].as_str().map(|s| s != "running").unwrap_or(true);
                                    // `pending` is the questions this run still
                                    // holds. A service that does not report the
                                    // list is not the same as an empty one —
                                    // except once the run is over, when it
                                    // cannot be waiting on anyone.
                                    let live = match (v["pending"].as_array(), finished) {
                                        (Some(list), _) => Some(list.iter().filter_map(Value::as_str)
                                            .map(str::to_string).collect::<Vec<String>>()),
                                        (None, true) => Some(Vec::new()),
                                        (None, false) => None,
                                    };
                                    if let Some(ids) = live {
                                        let _ = tx.send(UiEvent::Pending(id.clone(), ids));
                                    }
                                    if finished {
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

    /// Hand one answer to the service off the UI thread, then report what it
    /// said. A click is not a claim that the call ran: only the `tool` step
    /// that follows one can say that, and it arrives on its own.
    fn answer(&mut self, approval_id: &str, decision: &str) {
        let (core, tx, sid) = (self.core.clone(), self.tx.clone(), self.selected.clone());
        let (approval_id, decision) = (approval_id.to_string(), decision.to_string());
        std::thread::spawn(move || {
            let error = core.approve(&sid, &approval_id, &decision).err();
            let _ = tx.send(UiEvent::Answered(sid, approval_id, decision, error));
        });
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
        if ui.selectable_label(false, "New Task").on_hover_text(format!("{MOD_HINT}+N")).clicked() {
            self.new_draft();
        }
        if ui.selectable_label(self.search_open, "Search").on_hover_text(format!("{MOD_HINT}+K")).clicked() {
            self.search_open = !self.search_open;
        }
        ui.separator();
        ui.label(egui::RichText::new("RECENT TASKS").size(10.5).color(TEXT2));
        egui::ScrollArea::vertical().id_source("tasks").max_height(340.0).show(ui, |ui| {
            let ids: Vec<String> = self.tasks.iter()
                .filter(|t| match self.nav {
                    Nav::Coding => true,
                    Nav::Attention => matches!(t.status,
                        TaskStatus::NeedsAttention | TaskStatus::Waiting | TaskStatus::AwaitingApproval) && !t.draft,
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
            let (vision, _) = vision_state(self.env.vision);
            let rows: Vec<(&str, String, Option<bool>)> = vec![
                ("Runtime", runtime, None),
                ("Agent loop", loop_state, Some(self.env.core_up)),
                ("Engine", engine_state, Some(self.env.engine_up)),
                ("Model", model, None),
                ("Vision", vision.to_string(), None),
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
            let runs: Vec<(String, ToolStatus, String, Option<i64>)> = tl.tools.iter()
                .filter(|c| matches!(c.category.as_str(), "TEST" | "CMD"))
                .map(|c| (c.title.clone(), c.status, c.duration.clone(), c.exit))
                .collect();
            if runs.is_empty() {
                ui.label(egui::RichText::new("No commands run in this task yet.").size(11.5).color(TEXT2));
            }
            for (title, st, dur, exit) in runs {
                ui.horizontal(|ui| {
                    ui.colored_label(tool_color(st), "●");
                    ui.label(egui::RichText::new(format!("{title} · {}", st.label())).size(11.5).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !dur.is_empty() {
                            ui.label(egui::RichText::new(dur).monospace().size(10.5).color(TEXT2));
                        }
                        // Only `_run_process` reports an exit status; a tool that
                        // returns no exit code must not be shown as "exit 0".
                        if let Some(code) = exit {
                            ui.colored_label(if code == 0 { OK } else { ERR },
                                egui::RichText::new(format!("exit {code}")).monospace().size(10.5));
                        }
                    });
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
            .frame(egui::Frame::none().fill(PANEL).rounding(RADIUS_RAISED).inner_margin(10.0))
            .show_inside(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.composer)
                    .desired_rows(2).hint_text("Ask Fennec to inspect, edit, test, or explain your code")
                    .desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    ui.toggle_value(&mut self.allow_write, "edits").on_hover_text("the loop's allow_write switch");
                    ui.toggle_value(&mut self.allow_shell, "commands").on_hover_text("the loop's allow_shell switch");
                    ui.toggle_value(&mut self.allow_web, "web").on_hover_text("the loop's allow_web outlet switch");
                    ui.toggle_value(&mut self.ask_each, "ask each")
                        .on_hover_text("the loop's require_approval switch: every gated call is put to you, and the three switches above stop admitting calls on their own");
                    // Three affordances this shell cannot serve today. They are
                    // shown disabled with the reason rather than hidden, and
                    // never presented as having worked. The image reason is the
                    // engine's own word, not a guess about what it can do.
                    let (_, why_vision) = vision_state(self.env.vision);
                    ui.add_enabled(false, egui::Button::new(egui::RichText::new("image").size(11.5)))
                        .on_disabled_hover_text(why_vision);
                    ui.add_enabled(false, egui::Button::new(egui::RichText::new("record").size(11.5)))
                        .on_disabled_hover_text(UNAVAILABLE_DICTATION);
                    ui.add_enabled(false, egui::Button::new(egui::RichText::new("read aloud").size(11.5)))
                        .on_disabled_hover_text(UNAVAILABLE_TTS);
                    if core_up && self.ask_each {
                        // With an approver attached the service lifts the flags,
                        // so the three switches above stop meaning "yes" and the
                        // answer on each card decides. Saying so is what keeps
                        // the row honest.
                        ui.label(egui::RichText::new("each gated call is put to you first")
                            .size(10.5).color(WARN));
                    }
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
            egui::Frame::none().fill(HOVER).rounding(RADIUS).inner_margin(8.0).show(ui, |ui| {
                ui.label(egui::RichText::new(note).size(12.0).color(WARN));
            });
        }
        ui.separator();
        let id = self.selected.clone();
        let tl = self.timelines.entry(id).or_default().clone();
        let out = egui::ScrollArea::vertical().id_source("timeline").stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
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
                egui::Frame::none().fill(PANEL).rounding(RADIUS).inner_margin(8.0).show(ui, |ui| {
                    ui.label(egui::RichText::new("Plan").size(12.0).strong().color(TEXT2));
                    for p in &tl.plan {
                        ui.label(egui::RichText::new(format!("{}  {}", if p.done { "✓" } else { "→" }, p.content))
                            .size(12.0).color(if p.done { OK } else { TEXT2 }));
                    }
                });
                ui.add_space(10.0);
            }
            // A gated call the loop is stopped on. Above the tool cards,
            // because this is the one thing here that cannot move until the
            // user does. The risk and the reason are the backend's words.
            let mut answer: Option<(String, String)> = None;
            for card in &tl.approvals {
                let live = card.state == events::ApprovalState::Pending && answer.is_none();
                egui::Frame::none().fill(SURFACE).rounding(RADIUS).inner_margin(egui::Margin::symmetric(8.0, 6.0)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&card.category).monospace().size(10.0).color(BG).background_color(risk_color(&card.risk)));
                        ui.label(egui::RichText::new(&card.title).size(12.5).color(TEXT));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.colored_label(risk_color(&card.risk), format!("{} risk", card.risk));
                        });
                    });
                    if !card.reason.is_empty() {
                        ui.label(egui::RichText::new(&card.reason).size(11.5).color(TEXT2));
                    }
                    if let Some(path) = card.path.as_deref() {
                        ui.label(egui::RichText::new(format!("file · {path}")).monospace().size(11.0).color(TEXT2));
                    }
                    ui.label(egui::RichText::new(&card.detail).monospace().size(11.0).color(TEXT2));
                    ui.horizontal(|ui| {
                        for decision in events::APPROVAL_DECISIONS {
                            let response = ui.add_enabled(live, egui::Button::new(
                                egui::RichText::new(events::decision_label(decision)).size(11.5)));
                            if response.clicked() {
                                answer = Some((card.id.clone(), (*decision).to_string()));
                            }
                        }
                    });
                    match card.state {
                        events::ApprovalState::Pending => {
                            ui.label(egui::RichText::new("the run waits on this answer").size(10.5).color(WARN));
                        }
                        events::ApprovalState::Answered => {
                            // Not a claim that the call did anything: the tool
                            // card below carries its result.
                            ui.label(egui::RichText::new(format!("answer sent · {}", events::decision_label(&card.decision)))
                                .size(10.5).color(OK));
                        }
                        events::ApprovalState::Expired => {
                            ui.label(egui::RichText::new(format!("not used · {}", card.note)).size(10.5).color(ERR));
                        }
                    }
                });
                ui.add_space(6.0);
            }
            if let Some((approval_id, decision)) = answer {
                self.answer(&approval_id, &decision);
            }
            let mut toggle: Option<String> = None;
            for card in &tl.tools {
                let open = self.expanded.contains(&card.id);
                egui::Frame::none().fill(SURFACE).rounding(RADIUS).inner_margin(egui::Margin::symmetric(8.0, 6.0)).show(ui, |ui| {
                    ui.push_id(&card.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&card.category).monospace().size(10.0).color(BG).background_color(BRAND));
                            ui.label(egui::RichText::new(&card.title).size(12.5).color(TEXT));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if !card.duration.is_empty() {
                                    ui.label(egui::RichText::new(&card.duration).monospace().size(10.5).color(TEXT2));
                                }
                                if let Some(code) = card.exit {
                                    ui.colored_label(if code == 0 { OK } else { ERR },
                                        egui::RichText::new(format!("exit {code}")).monospace().size(10.5));
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
                    egui::Frame::none().fill(BG).rounding(RADIUS).inner_margin(8.0).show(ui, |ui| {
                        egui::ScrollArea::vertical().id_source(format!("out-{}", card.id)).max_height(160.0).show(ui, |ui| {
                            ui.label(egui::RichText::new(&card.output).monospace().size(11.5).color(TEXT));
                        });
                    });
                }
                ui.add_space(6.0);
            }
            if !tl.summary.is_empty() {
                egui::Frame::none().fill(PANEL).rounding(RADIUS).inner_margin(10.0).show(ui, |ui| {
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
            // Scroll to the bottom edge of everything laid out this frame. A
            // thin rect, because asking for the whole (taller than the
            // viewport) content rect would land on whichever edge egui picks.
            if self.jump_latest {
                self.jump_latest = false;
                let r = ui.min_rect();
                ui.scroll_to_rect(
                    egui::Rect::from_min_size(egui::pos2(r.min.x, r.max.y - 4.0), egui::vec2(r.width(), 4.0)),
                    None,
                );
            }
        });
        let last = out.content_size.y - out.inner_rect.height();
        self.timeline_off_end = out.state.offset.y < last - 24.0;
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        if let Some(a) = self.preview.clone() {
            let mut close = false;
            egui::Window::new(format!("{}  ({})", a.name, a.change)).default_width(620.0).show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("status: {}", a.status)).monospace().size(11.0).color(TEXT2));
                ui.label(egui::RichText::new(&a.name).monospace().size(11.0).color(TEXT2));
                let stale = self.diff.as_ref().map(|d| Some(d.path.as_str()) != a.path.as_deref()).unwrap_or(false);
                if stale {
                    self.diff = None;
                }
                match a.path.clone() {
                    Some(path) => {
                        if ui.button("Show git diff").clicked() {
                            let dir = self.workspace();
                            let tx = self.tx.clone();
                            self.diff = Some(Diff { path: path.clone(), pending: true, text: None, note: String::new() });
                            std::thread::spawn(move || {
                                let r = match git::git_out(&dir, &["diff", "--", &path]) {
                                    Some(d) => Ok(d),
                                    None => Err("git diff returned nothing usable — git is missing, or this path is outside the repo.".to_string()),
                                };
                                let _ = tx.send(UiEvent::Diff(path, r));
                            });
                        }
                    }
                    None => {
                        ui.label(egui::RichText::new("This edit carries no path, so there is nothing to diff — showing what the loop reported.")
                            .size(11.5).color(TEXT2));
                    }
                }
                let pending = self.diff.as_ref().map(|d| d.pending).unwrap_or(false);
                if pending {
                    ui.label(egui::RichText::new("Asking git…").size(11.5).color(TEXT2));
                } else if let Some(d) = self.diff.as_ref().filter(|d| d.text.is_some()) {
                    let text = d.text.clone().unwrap_or_default();
                    egui::ScrollArea::vertical().id_source("diff").max_height(360.0).show(ui, |ui| {
                        render_diff(ui, &text);
                    });
                    let copy = text.clone();
                    if ui.small_button("copy diff").clicked() {
                        ui.output_mut(|o| o.copied_text = copy);
                    }
                } else {
                    if let Some(d) = self.diff.as_ref() {
                        ui.label(egui::RichText::new(&d.note).size(11.5).color(WARN));
                    }
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.label(egui::RichText::new(&a.preview).monospace().size(12.0));
                    });
                }
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
        // The timeline's own stick-to-bottom releases as soon as the user
        // scrolls up; this is how they get back without dragging.
        if self.timeline_off_end && self.nav == Nav::Coding {
            let mut jump = false;
            egui::Area::new(egui::Id::new("jump-latest"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -72.0])
                .show(ctx, |ui| {
                    let button = egui::Button::new(egui::RichText::new("↓ Jump to latest").size(11.5).color(TEXT))
                        .fill(SURFACE).small();
                    if ui.add(button).clicked() {
                        jump = true;
                    }
                });
            if jump {
                self.jump_latest = true;
            }
        }
        // Toasts sit over the composer: they report something that happened in
        // a pane the user is not looking at, then expire without being cleared.
        self.toasts.retain(|t| t.at.elapsed() < TOAST_TTL);
        if !self.toasts.is_empty() {
            let items = self.toasts.clone();
            egui::Area::new(egui::Id::new("toasts"))
                .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -56.0])
                .show(ctx, |ui| {
                    for t in items {
                        egui::Frame::none().fill(SURFACE).rounding(RADIUS)
                            .inner_margin(egui::Margin::symmetric(12.0, 8.0)).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(if t.good { OK } else { WARN }, "●");
                                    ui.label(egui::RichText::new(t.text).size(12.0).color(TEXT));
                                });
                            });
                        ui.add_space(6.0);
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }
}

fn expand_button(ui: &mut egui::Ui, open: bool) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(if open { "Hide result" } else { "Show result" }).size(10.5).color(TEXT2))
        .fill(HOVER).small())
}

/// A unified diff, line-tinted. Long diffs are truncated with a note rather
/// than paged: this is a preview, and the full text is one `copy diff` away.
fn render_diff(ui: &mut egui::Ui, text: &str) {
    const MAX_LINES: usize = 400;
    let total = text.lines().count();
    for (i, line) in text.lines().enumerate() {
        if i >= MAX_LINES {
            ui.label(egui::RichText::new(format!("… {} more lines — copy the diff or run git diff in a terminal.", total - i))
                .size(11.0).color(TEXT2));
            break;
        }
        let col = if line.starts_with('+') && !line.starts_with("+++") {
            OK
        } else if line.starts_with('-') && !line.starts_with("---") {
            ERR
        } else if line.starts_with('@') {
            BRAND
        } else {
            TEXT2
        };
        ui.label(egui::RichText::new(line).monospace().size(11.5).color(col));
    }
}

/// Chat text as the model wrote it. ``` fences become copyable monospace
/// cards; the prose between them is read as a Markdown subset. No new
/// dependency: egui's own LayoutJob carries the styled runs.
fn render_rich(ui: &mut egui::Ui, text: &str, dim: bool) {
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let before = &rest[..open];
        if !before.trim().is_empty() {
            render_md(ui, before.trim(), dim);
        }
        let after = &rest[open + 3..];
        let (body, tail) = match after.find("```") {
            Some(end) => (&after[..end], &after[end + 3..]),
            None => (after, ""),
        };
        let body = body.trim();
        let (lang, code) = match body.split_once('\n') {
            Some((first, code)) => match info_string(first) {
                Some(lang) => (lang.to_string(), code.trim().to_string()),
                None => (String::new(), body.to_string()),
            },
            None => (String::new(), body.to_string()),
        };
        egui::Frame::none().fill(SURFACE).rounding(RADIUS).inner_margin(8.0).show(ui, |ui| {
            if !lang.is_empty() {
                ui.label(egui::RichText::new(&lang).monospace().size(10.0).color(TEXT2));
            }
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                ui.label(egui::RichText::new(&code).monospace().size(12.0).color(TEXT));
            });
            let copy = code.clone();
            if ui.small_button("copy").clicked() {
                ui.output_mut(|o| o.copied_text = copy);
            }
        });
        rest = tail;
    }
    if !rest.trim().is_empty() {
        render_md(ui, rest.trim(), dim);
    }
}

/// A fence's first line, when it names a language instead of holding code.
fn info_string(line: &str) -> Option<&str> {
    let t = line.trim();
    (!t.is_empty()
        && t.len() <= 16
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.')))
        .then_some(t)
}

/// One styled piece of a line. `Strong` is egui's own reading of strong text —
/// 0.27 has no fake-bold, so it reads brighter, not heavier.
#[derive(Clone, PartialEq, Debug)]
enum Run {
    Text(String),
    Code(String),
    Strong(String),
    Em(String),
    Link { label: String, url: String },
}

#[derive(Clone, PartialEq, Debug)]
struct MdLine {
    head: u8,
    bullet: bool,
    runs: Vec<Run>,
}

/// The Markdown subset these models actually emit: `#` headings, `-` bullets,
/// `code`, **strong**, *emphasis*, [label](url). Numbered items keep the
/// model's own digits, because the number is part of the answer.
fn md_lines(text: &str) -> Vec<MdLine> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        let (head, body) = if hashes > 0 && hashes <= 4 && line[hashes..].starts_with(' ') {
            (hashes as u8, line[hashes..].trim_start())
        } else {
            (0u8, line)
        };
        let mut bullet = false;
        let mut rest = body;
        for m in ["- ", "* ", "+ "] {
            if let Some(r) = rest.strip_prefix(m) {
                bullet = true;
                rest = r;
                break;
            }
        }
        out.push(MdLine { head, bullet, runs: md_runs(rest) });
    }
    out
}

/// Inline parsing. A marker that never closes stays literal text, so a stray
/// asterisk or bracket can never swallow the rest of the sentence.
fn md_runs(text: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut buf = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut best: Option<(usize, usize, u8)> = None;
        for (idx, len, kind) in [
            (rest.find('`'), 1usize, 1u8),
            (rest.find("**"), 2, 2),
            (rest.find('*'), 1, 3),
            (rest.find('['), 1, 4),
        ] {
            let Some(i) = idx else { continue };
            let wins = match best {
                None => true,
                Some((bi, bl, _)) => i < bi || (i == bi && len > bl),
            };
            if wins {
                best = Some((i, len, kind));
            }
        }
        let Some((at, open_len, kind)) = best else {
            buf.push_str(rest);
            break;
        };
        buf.push_str(&rest[..at]);
        rest = &rest[at..];
        if kind == 4 {
            match md_link(rest) {
                Some((label, url, used)) => {
                    if !buf.is_empty() {
                        runs.push(Run::Text(std::mem::take(&mut buf)));
                    }
                    runs.push(Run::Link { label, url });
                    rest = &rest[used..];
                }
                None => {
                    buf.push('[');
                    rest = &rest[1..];
                }
            }
            continue;
        }
        let close_len = open_len;
        let body = &rest[open_len..];
        match body.find(&rest[..close_len]) {
            Some(end) if end > 0 => {
                if !buf.is_empty() {
                    runs.push(Run::Text(std::mem::take(&mut buf)));
                }
                let text = body[..end].to_string();
                runs.push(match kind {
                    1 => Run::Code(text),
                    2 => Run::Strong(text),
                    _ => Run::Em(text),
                });
                rest = &rest[open_len + end + close_len..];
            }
            _ => {
                buf.push_str(&rest[..open_len]);
                rest = &rest[open_len..];
            }
        }
    }
    if !buf.is_empty() {
        runs.push(Run::Text(buf));
    }
    runs
}

/// `[label](url)` at the front of `s`, plus how many bytes it used.
fn md_link(s: &str) -> Option<(String, String, usize)> {
    let after = s.strip_prefix('[')?;
    let split = after.find("](")?;
    let label = &after[..split];
    let tail = &after[split + 2..];
    let end = tail.find(')')?;
    Some((label.to_string(), tail[..end].to_string(), 1 + split + 2 + end + 1))
}

fn render_md(ui: &mut egui::Ui, text: &str, dim: bool) {
    let base = if dim { 12.0 } else { 13.5 };
    for line in md_lines(text) {
        let size = base + match line.head {
            1 => 6.0,
            2 => 3.5,
            3 => 1.5,
            _ => 0.0,
        };
        let body = if line.head > 0 { TEXT } else if dim { TEXT2 } else { TEXT };
        if line.head == 0 && line.runs.len() == 1 && matches!(line.runs[0], Run::Link { .. }) {
            if let Run::Link { label, url } = &line.runs[0] {
                ui.hyperlink_to(label.as_str(), url.as_str());
                continue;
            }
        }
        let mut job = egui::text::LayoutJob::default();
        if line.bullet {
            job.append("• ", 0.0, egui::text::TextFormat::simple(egui::FontId::proportional(size), body));
        }
        for run in &line.runs {
            let plain = egui::text::TextFormat::simple(egui::FontId::proportional(size), body);
            match run {
                Run::Text(t) => job.append(t, 0.0, plain),
                Run::Strong(t) => job.append(t, 0.0, egui::text::TextFormat::simple(egui::FontId::proportional(size), TEXT)),
                Run::Em(t) => {
                    let mut f = plain;
                    f.italics = true;
                    job.append(t, 0.0, f);
                }
                Run::Code(t) => {
                    let mut f = egui::text::TextFormat::simple(egui::FontId::monospace(size - 1.0), TEXT);
                    f.background = HOVER;
                    job.append(t, 0.0, f);
                }
                Run::Link { label, url } => {
                    job.append(label, 0.0, egui::text::TextFormat::simple(egui::FontId::proportional(size), BRAND));
                    job.append(&format!(" ({url})"), 0.0, egui::text::TextFormat::simple(egui::FontId::monospace(size - 1.5), TEXT2));
                }
            }
        }
        ui.add(egui::Label::new(job).wrap(true));
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

/// What the engine itself said about images, in whichever spelling it uses: a
/// `vision` flag (the Fennec engine declares `false`) or an `input_modalities`
/// list (a Colibri server lists `image`). `None` is the engine staying silent,
/// which is not the same claim as a declared "no".
fn engine_vision(health: &serde_json::Value) -> Option<bool> {
    if let Some(v) = health["vision"].as_bool() {
        return Some(v);
    }
    health["input_modalities"]
        .as_array()
        .map(|list| list.iter().any(|m| m.as_str() == Some("image")))
}

/// The same answer as the Environment-pane label paired with the composer's
/// reason. The button is off in all three states — this shell sends no
/// attachment — but only an engine that declared it cannot see images may be
/// described that way; silence is not a refusal.
fn vision_state(vision: Option<bool>) -> (&'static str, &'static str) {
    match vision {
        Some(true) => ("engine reports vision", VISION_NO_PICKER),
        Some(false) => ("engine analyzes no images", VISION_REFUSED),
        None => ("not declared", VISION_NO_PICKER),
    }
}

impl App {
    /// One frame of the whole interface. `App::update` is a thin wrapper over
    /// this, because an `eframe::Frame` cannot be made without a window: a test
    /// can drive the three panes and every overlay through `egui::Context::run`
    /// and check what the layout actually produced rather than what it claims.
    fn paint(&mut self, ctx: &egui::Context) {
        self.drain();
        if self.brand.is_none() {
            self.brand = brand_texture(ctx);
        }
        ctx.input_mut(|i| {
            if i.consume_key(MOD, egui::Key::K) {
                self.search_open = !self.search_open;
            }
            if i.consume_key(MOD, egui::Key::N) {
                self.new_draft();
            }
            if i.consume_key(MOD, egui::Key::Enter) {
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

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.paint(ctx);
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
        let (mono, prop) = apply_theme(&cc.egui_ctx);
        let mut app = App::new();
        app.mono_font = mono;
        app.ui_font = prop;
        Box::new(app)
    }))
}

/// The whole theme in one function, returning which font faces were found.
/// The window calls it at startup and a headless frame calls the same thing,
/// so a test paints what the user would see instead of egui's defaults.
fn apply_theme(ctx: &egui::Context) -> (bool, bool) {
    let (mono, prop) = fonts::install(ctx);
    ctx.style_mut(|s| {
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
        // Buttons, fields and windows take the same two corner steps as the
        // frames above, so nothing in the app is squared off next to a card.
        let card = egui::Rounding::same(RADIUS);
        let raised = egui::Rounding::same(RADIUS_RAISED);
        for w in [&mut s.visuals.widgets.noninteractive, &mut s.visuals.widgets.inactive,
                  &mut s.visuals.widgets.hovered, &mut s.visuals.widgets.active] {
            w.rounding = card;
        }
        s.visuals.window_rounding = raised;
    });
    (mono, prop)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the headless frame builds these; the live UI reads them through
    // `Event`, so naming them here keeps them out of the binary.
    use fennec_desktop::events::{PlanEntry, ToolEvent};

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
    fn the_engine_answers_for_images_in_whichever_spelling_it_uses() {
        use serde_json::json;
        assert_eq!(engine_vision(&json!({"status": "ok", "vision": false})), Some(false));
        // A Colibri server has no vision flag; it lists what it accepts, and that is an answer.
        assert_eq!(engine_vision(&json!({"arch": "qwen36", "input_modalities": ["text", "image"]})), Some(true));
        assert_eq!(engine_vision(&json!({"input_modalities": ["text"]})), Some(false));
        // The flag is the more specific statement, so it wins over the list.
        assert_eq!(engine_vision(&json!({"vision": false, "input_modalities": ["text", "image"]})), Some(false));
        // Silence, and a wrong type, are both "the engine never said".
        assert_eq!(engine_vision(&json!({"status": "ok"})), None);
        assert_eq!(engine_vision(&json!({"vision": "false", "input_modalities": "image"})), None);
    }

    #[test]
    fn only_an_engine_that_refused_may_be_reported_blind() {
        assert_eq!(vision_state(Some(false)), ("engine analyzes no images", VISION_REFUSED));
        assert!(VISION_REFUSED.contains("refuses"), "{VISION_REFUSED}");
        // Silence is not a refusal: a Colibri checkout has vision towers and declares
        // nothing here, so the shell may only name its own missing attachment path.
        for state in [None, Some(true)] {
            assert!(!vision_state(state).1.contains("analyzes no images"), "{state:?}");
        }
        assert_eq!(vision_state(None).0, "not declared");
        assert_eq!(vision_state(Some(true)).0, "engine reports vision");
    }

    #[test]
    fn a_fresh_stamp_reads_as_now() {
        assert_eq!(time_ago(&events::now_stamp()), "now");
    }

    #[test]
    fn markdown_lines_carry_headings_and_bullets() {
        let lines = md_lines("# Title\n- first\n2. second\nplain");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], MdLine { head: 1, bullet: false, runs: vec![Run::Text("Title".into())] });
        assert_eq!(lines[1], MdLine { head: 0, bullet: true, runs: vec![Run::Text("first".into())] });
        assert_eq!(lines[2].runs, vec![Run::Text("2. second".into())]);
        assert!(!lines[2].bullet);
    }

    #[test]
    fn inline_markers_split_into_runs() {
        assert_eq!(
            md_runs("use `git status`, it is **fast** and *safe*"),
            vec![
                Run::Text("use ".into()),
                Run::Code("git status".into()),
                Run::Text(", it is ".into()),
                Run::Strong("fast".into()),
                Run::Text(" and ".into()),
                Run::Em("safe".into()),
            ]
        );
    }

    #[test]
    fn an_unclosed_marker_stays_literal() {
        assert_eq!(
            md_runs("2 * 3 and `code still open"),
            vec![Run::Text("2 * 3 and `code still open".into())]
        );
    }

    #[test]
    fn a_link_keeps_its_label_and_url() {
        assert_eq!(
            md_runs("see [the repo](https://example.com/a) now"),
            vec![
                Run::Text("see ".into()),
                Run::Link { label: "the repo".into(), url: "https://example.com/a".into() },
                Run::Text(" now".into()),
            ]
        );
        assert_eq!(md_runs("[not a link"), vec![Run::Text("[not a link".into())]);
    }

    #[test]
    fn a_fence_opener_is_only_a_language_when_it_looks_like_one() {
        assert_eq!(info_string("rust"), Some("rust"));
        assert_eq!(info_string("C++"), Some("C++"));
        assert_eq!(info_string("# not code"), None);
        assert_eq!(info_string("let x = 1;"), None);
    }

    /// Paint one frame with no window: `egui::Context::run` needs no
    /// `eframe::Frame`, and `tessellate` is the same triangle generation the
    /// GL painter calls. Returns the vertex count and the rectangle those
    /// triangles cover, so a pane that collapsed to nothing fails here instead
    /// of only looking wrong to somebody at a desk.
    fn paint_frame(app: &mut App, size: egui::Vec2) -> (usize, egui::Rect) {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size));
        let out = ctx.run(input, |ctx| app.paint(ctx));
        let mut covered = egui::Rect::NOTHING;
        let mut vertices = 0;
        for prim in ctx.tessellate(out.shapes, out.pixels_per_point) {
            if let egui::epaint::Primitive::Mesh(mesh) = prim.primitive {
                vertices += mesh.vertices.len();
                for v in &mesh.vertices {
                    covered.extend_with(v.pos);
                }
            }
        }
        (vertices, covered)
    }

    #[test]
    fn a_headless_frame_paints_the_three_panes_and_their_overlays() {
        let mut app = App::new();
        let (bare, span) = paint_frame(&mut app, egui::vec2(1500.0, 960.0));
        assert!(span.min.x <= 1.0 && span.min.y <= 1.0, "{span:?} does not start at the corner");
        assert!(span.max.x >= 1499.0 && span.max.y >= 959.0, "{span:?} does not reach the corner");
        assert!(bare > 300, "{bare} vertices is not an interface");

        let mut tl = Timeline::default();
        tl.apply(Event::Message(ChatMessage {
            role: Role::User,
            text: "add the plan panel".into(),
            time: events::now_stamp(),
        }));
        tl.apply(Event::Tool(ToolEvent {
            id: "c1".into(),
            category: "edit_file".into(),
            title: "edit src/app.rs".into(),
            status: ToolStatus::Complete,
            detail: "1 replacement".into(),
            output: String::new(),
            path: Some("src/app.rs".into()),
            exit: None,
            started_t: 0.4,
            duration: "1.1 s".into(),
        }));
        tl.apply(Event::Plan(vec![
            PlanEntry { content: "Inspect the workspace".into(), done: true },
            PlanEntry { content: "Implement the change".into(), done: false },
        ]));
        tl.apply(Event::Message(ChatMessage {
            role: Role::Assistant,
            text: "Done.\n\n- **plan** panel added\n- `src/app.rs` edited\n".into(),
            time: events::now_stamp(),
        }));
        app.timelines.insert("t1".into(), tl);
        app.selected = "t1".into();
        app.diff = Some(Diff {
            path: "src/app.rs".into(),
            pending: false,
            text: Some("--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,2 +1,2 @@\n-old line\n+new line\n".into()),
            note: String::new(),
        });
        app.toast("Task · complete", true);
        let (full, _) = paint_frame(&mut app, egui::vec2(1500.0, 960.0));
        assert!(full > bare, "{full} vertices should outdraw the empty task's {bare}");

        // The minimum window the builder promises is a layout the app keeps.
        let (_, small) = paint_frame(&mut app, egui::vec2(1180.0, 760.0));
        assert!(small.max.x >= 1179.0 && small.max.y >= 759.0, "{small:?} clipped at the minimum size");
    }

    /// The card a blocked run leaves on screen, drawn from the step the service
    /// actually sent. Whether the four answers do anything is the state
    /// machine's test below; this one says the question is on the pane at all.
    #[test]
    fn an_approval_question_draws_a_card() {
        let mut app = App::new();
        let (bare, _) = paint_frame(&mut app, egui::vec2(1500.0, 960.0));
        let mut tl = Timeline::default();
        for e in events::map_step(&json!({"type": "approval_required", "approval_id": "a1",
                                          "name": "run_shell", "args": {"command": "pytest -q"},
                                          "risk": "high",
                                          "reason": "runs a command on this machine, unsandboxed"})) {
            tl.apply(e);
        }
        tl.status = TaskStatus::AwaitingApproval;
        app.timelines.insert("t1".into(), tl);
        app.selected = "t1".into();
        let (asked, _) = paint_frame(&mut app, egui::vec2(1500.0, 960.0));
        assert!(asked > bare, "{asked} vertices should outdraw the empty task's {bare}");

        // A question that is no longer live still paints: the card has to say
        // what happened to it rather than vanish off the timeline.
        if let Some(tl) = app.timelines.get_mut("t1") {
            tl.close_approvals(&[]);
        }
        let (closed, _) = paint_frame(&mut app, egui::vec2(1500.0, 960.0));
        assert!(closed > bare, "a retired question has to report why it retired");
    }

    /// The whole wait, from the step to the answer, through the same channel
    /// the poll thread uses. Nothing here reaches the network: the worker's
    /// reply is injected, exactly as it would arrive.
    #[test]
    fn a_run_blocked_on_an_answer_reads_as_blocked() {
        let mut app = App::new();
        for id in ["s1", "s2"] {
            app.tasks.push(Task {
                id: id.into(),
                title: format!("task {id}"),
                status: TaskStatus::Running,
                updated: "now".into(),
                goal: String::new(),
                draft: false,
            });
        }
        app.selected = "s1".into();
        let step = json!({"type": "approval_required", "approval_id": "a1", "name": "write_file",
                          "args": {"path": "note.md"}, "risk": "medium",
                          "reason": "replaces a file in the workspace"});
        for e in events::map_step(&step) {
            let _ = app.tx.send(UiEvent::Task("s2".into(), e));
        }
        let _ = app.tx.send(UiEvent::Pending("s2".into(), vec!["a1".into()]));
        app.drain();
        assert_eq!(app.timelines["s2"].status, TaskStatus::AwaitingApproval);
        assert_eq!(app.tasks[1].status, TaskStatus::AwaitingApproval);
        // Nobody is looking at s2, so the wait is news: a toast that says
        // waiting, not one that says the task finished.
        assert!(app.toasts.iter().any(|t| !t.good && t.text.contains("waiting for an answer")));
        let told = app.toasts.len();

        let _ = app.tx.send(UiEvent::Answered("s2".into(), "a1".into(), "allow_once".into(), None));
        let _ = app.tx.send(UiEvent::Pending("s2".into(), vec![]));
        app.drain();
        let card = &app.timelines["s2"].approvals[0];
        assert_eq!(card.state, events::ApprovalState::Answered);
        assert_eq!(card.decision, "allow_once");
        assert_eq!(app.timelines["s2"].status, TaskStatus::Running);
        assert_eq!(app.tasks[1].status, TaskStatus::Running);
        // Clearing a wait is not news; only being asked is.
        assert_eq!(app.toasts.len(), told);
    }

    /// A server that never reports the list is not the same as one that reports
    /// none: a live card must not be retired on an absent field.
    #[test]
    fn a_service_that_reports_no_pending_list_leaves_the_question_live() {
        let mut app = App::new();
        app.selected = "s1".into();
        for e in events::map_step(&json!({"type": "approval_required", "approval_id": "a1",
                                          "name": "edit_file", "args": {"path": "a.rs"},
                                          "risk": "medium", "reason": "edits a file"})) {
            let _ = app.tx.send(UiEvent::Task("s1".into(), e));
        }
        app.drain();
        assert_eq!(app.timelines["s1"].approvals[0].state, events::ApprovalState::Pending);
        assert_eq!(app.timelines["s1"].status, TaskStatus::Running);
    }

    /// Paint one frame that contains a key press, so a shortcut is tested the
    /// way a user reaches it -- through egui's input, not by calling the method
    /// behind it. A shortcut wired to the wrong key is invisible to every other
    /// test here.
    fn press_key(app: &mut App, key: egui::Key, modifiers: egui::Modifiers) {
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect =
            Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1500.0, 960.0)));
        input.modifiers = modifiers;
        input.events = vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }];
        let _ = ctx.run(input, |ctx| app.paint(ctx));
    }

    #[test]
    fn the_shortcuts_drive_the_state_they_claim() {
        let mut app = App::new();
        assert!(!app.search_open);
        press_key(&mut app, egui::Key::K, MOD);
        assert!(app.search_open, "{MOD_HINT}+K opens search");
        press_key(&mut app, egui::Key::K, MOD);
        assert!(!app.search_open, "and closes it again");

        press_key(&mut app, egui::Key::N, MOD);
        assert!(!app.selected.is_empty(), "{MOD_HINT}+N selects the draft it made");
        assert!(app.timelines.contains_key(&app.selected.clone()));
        assert!(app.tasks.iter().any(|t| t.id == app.selected && t.draft));

        app.preview = Some(events::Artifact {
            name: "src/app.rs".into(),
            path: Some("src/app.rs".into()),
            change: "+2 -0".into(),
            status: "written".into(),
            preview: "fn main() {}".into(),
        });
        press_key(&mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert!(!app.search_open && app.preview.is_none(), "Escape dismisses what is open");
    }
}
