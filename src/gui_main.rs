//! Fennec — native Rust (gpui-kit) desktop coding-agent workspace.
//!
//! The same shell as the egui binary in `src/main.rs`, rebuilt on the
//! gpui-kit toolkit: identical protocols (`fennec_client`), identical event
//! model (`fennec_desktop::events`), identical history file, so a task started
//! in either binary resumes in the other. Where a capability the backends do
//! not expose would have to be faked, it is labelled unavailable instead.
// A GUI-subsystem binary: no console window next to the app, ever — a stray
// terminal in the taskbar would be a second app the user did not ask for.
#![cfg_attr(windows, windows_subsystem = "windows")]

use fennec_client::engine::{ChatDelta, Engine};
use fennec_client::{git, suite, Core, Outlet};
use fennec_desktop::events::{self, ApprovalState, ChatMessage, Event, Role, Task, TaskStatus, Timeline, ToolStatus};
use fennec_ui::markdown::{artifact_kind, diff_counts, rich_div};
use fennec_ui::theme::{find_preset_index, theme_presets};
use fennec_ui::widgets::{action_card, ai_header, attachment_card, edit_card, queue_strip, speak_tts, truncate, user_bubble};
use fennec_ui::workspace_dialog::open_workspace_dialog;
use gpui_kit::base::Root;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    Theme,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;
use base64::{Engine as _, engine::general_purpose};

// ---- Settings persistence ----

#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    server_url: Option<String>,
    engine_url: Option<String>,
    model: Option<String>,
    theme: Option<String>,
    font_size: Option<f32>,
    auto_save: Option<bool>,
    notifications: Option<bool>,
    debug_mode: Option<bool>,
    experimental_features: Option<bool>,
}

fn settings_path() -> PathBuf {
    let dir = std::env::current_exe().ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let fennec_dir = dir.join(".fennec");
    let _ = std::fs::create_dir_all(&fennec_dir);
    fennec_dir.join("gui_settings.json")
}

fn load_settings() -> Settings {
    let path = settings_path();
    std::fs::read_to_string(&path).ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) {
    let path = settings_path();
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(&path, json);
    }
}

// ---- History persistence (shared with the egui binary) ----

#[derive(Serialize, Deserialize)]
struct HistoryData {
    tasks: Vec<HistoryTask>,
    timelines: HashMap<String, HistoryTimeline>,
    seen_at: HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct HistoryTask {
    id: String,
    title: String,
    status: String,
    updated: String,
    goal: String,
    draft: bool,
    workspace_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct HistoryTimeline {
    messages: Vec<HistoryMessage>,
    summary: String,
    status: String,
}

#[derive(Serialize, Deserialize)]
struct HistoryMessage {
    role: String,
    text: String,
    time: String,
}

/// How long a prompt may wait for a cold engine. The service gives the warm-up
/// 240 s; waiting past that would re-send a prompt long after the user gave up
/// on it.
const WARM_WINDOW: Duration = Duration::from_secs(300);

/// Why the composer's off affordances stay off. Checked against the backends
/// the shell actually ships, so the line is a fact, not a hedge.
const UNAVAILABLE_NOTE: &str =
    "image · record · read aloud stay off in this shell: no image picker, no microphone capture, and no speech-synthesis route.";

const VISION_REFUSED: &str = "engine analyzes no images";
const VISION_NO_PICKER: &str = "no attachment path";
const VISION_OK: &str = "engine reports vision";

/// Cmd on macOS, Ctrl everywhere else — spelled the way the key is bound.
const MOD_HINT: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Workspace {
    id: String,
    name: String,
    icon: String,
    color: String,
    source_folders: Vec<String>,
    indexed: bool,
}

impl Workspace {
    fn new(name: String) -> Self {
        Self {
            id: format!("ws-{}", millis()),
            name,
            icon: "folder".to_string(),
            color: "orange".to_string(),
            source_folders: vec![],
            indexed: true,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct WorkspaceStore {
    workspaces: Vec<Workspace>,
    active_id: Option<String>,
}

/// Permission mode for agent actions. AskEveryTime requires approval for each
/// action, AutoApprove only asks for risky operations, FullAccess never asks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
enum PermissionMode {
    AskEveryTime,
    AutoApprove,
    FullAccess,
}

impl Default for PermissionMode {
    fn default() -> Self {
        Self::AskEveryTime
    }
}

impl PermissionMode {
    fn label(&self) -> &'static str {
        match self {
            Self::AskEveryTime => "Ask every time",
            Self::AutoApprove => "Auto approve",
            Self::FullAccess => "Full access",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::AskEveryTime => "Always ask before commands, external edits, and internet use.",
            Self::AutoApprove => "Only asks when a potential risk is detected.",
            Self::FullAccess => "Never ask, with full access to your files, terminal, and the internet.",
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::AskEveryTime => "🛡",
            Self::AutoApprove => "🕐",
            Self::FullAccess => "⚠",
        }
    }

    fn color(&self, t: &gpui_kit::component::Theme) -> gpui::Hsla {
        match self {
            Self::AskEveryTime => t.primary,
            Self::AutoApprove => t.success,
            Self::FullAccess => t.red,
        }
    }
}

/// What the supervisor thread pushes to the UI. Every field is reported-or-
/// unknown; nothing defaults to a value that merely looks plausible.
#[derive(Clone, Default)]
struct Env {
    core_up: bool,
    core_workspace: String,
    engine_up: bool,
    model: String,
    ctx: String,
    tok_s: String,
    /// Only the engine knows whether it can look at pixels. `None` means the
    /// attached engine never answered, which is not the same claim as `false`.
    vision: Option<bool>,
    outlets: Vec<Outlet>,
    outlets_known: bool,
    /// Cached here rather than run per frame: a `git rev-parse` is a process,
    /// and the render path runs a lot more often than the probe.
    branch: String,
}

enum UiEv {
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
    Attachments(Vec<Attachment>),
    SttText(String),
}

/// A task waiting in the queue while AI works on another.
#[derive(Clone)]
struct QueuedTask {
    text: String,
}

/// A notice about something that happened outside the pane being looked at.
/// It expires on its own — it is not a log the user has to clear.
#[derive(Clone)]
struct Toast {
    text: String,
    good: bool,
    at: std::time::Instant,
}

/// File attachment for messages (images, documents, etc.)
#[derive(Clone, Debug)]
struct Attachment {
    path: PathBuf,
    name: String,
    mime_type: String,
    /// Base64-encoded data for images
    data: Option<String>,
    /// File size in bytes
    size: u64,
}

struct App {
    suite: suite::Suite,
    workspaces: Vec<PathBuf>,
    ws: usize,
    workspace_items: Vec<Workspace>,
    active_workspace_id: Option<String>,
    core: Core,
    engine: Engine,
    env: Env,
    tasks: Vec<Task>,
    seen_at: HashMap<String, String>,
    selected: String,
    timelines: HashMap<String, Timeline>,
    /// Sessions whose steps this window has already asked the service for, so
    /// a task selected after a restart is fetched once rather than every frame.
    replayed: HashSet<String>,
    composer: Entity<InputState>,
    search: Entity<InputState>,
    username_input: Entity<InputState>,
    /// Permission mode: AskEveryTime requires approval for each action,
    /// AutoApprove only asks for risky operations, FullAccess never asks.
    permission_mode: PermissionMode,
    attention_only: bool,
    right_open: bool,
    search_open: bool,
    settings_open: bool,
    permission_dropdown_open: bool,
    username: String,
    expanded: HashSet<String>,
    /// Thought/reasoning rows the user has opened; collapsed is the default.
    open_thoughts: HashSet<String>,
    toasts: Vec<Toast>,
    line: String,
    /// A prompt held back while a cold engine warms up, with the moment it was
    /// queued. `/api/agent/start` answers 503 until the engine is listening, so
    /// the prompt waits here instead of being thrown away.
    pending_send: Option<(String, std::time::Instant)>,
    /// A prompt that goes back to the composer after a failed send. Applied on
    /// the next render pass, which is where the input state is reachable.
    composer_restore: Option<String>,
    chat_cancel: Option<Arc<AtomicBool>>,
    rx: Receiver<UiEv>,
    tx: Sender<UiEv>,
    busy: bool,
    /// Task queue: prompts waiting for the AI to finish current task.
    task_queue: Vec<QueuedTask>,
    /// The timeline pane's scroll handle. `follow_tail` chases new messages
    /// until the user scrolls back through older history.
    timeline_scroll: ScrollHandle,
    follow_tail: Cell<bool>,
    _task: gpui_kit::Task<()>,
    model: String,
    theme: String,
    font_size: f32,
    auto_save: bool,
    notifications: bool,
    debug_mode: bool,
    experimental_features: bool,
    hovered_ws_id: Option<String>,
    model_dropdown_open: bool,
    /// Attached files for the current message (path, content type, base64 data for images)
    attachments: Vec<Attachment>,
    /// Task being renamed (id, new title input)
    renaming_task: Option<(String, Entity<InputState>)>,
    /// STT recording state
    stt_recording: bool,
    stt_stop: Arc<AtomicBool>,
    stt_buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    stt_rate: Arc<std::sync::atomic::AtomicU32>,
    stt_stream: Option<cpal::Stream>,
}

impl App {
    fn new(
        composer: Entity<InputState>,
        search: Entity<InputState>,
        username_input: Entity<InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let s = suite::discover();
        let workspaces = s.workspaces();
        let core = Core::new();
        let engine = Engine::new();

        // The probe loop is the app talking to live services. It reports the
        // environment and the session rail; nothing here invents a state a
        // backend did not answer with.
        let tx2 = tx.clone();
        let core0 = core.clone();
        let engine0 = engine.clone();
        let ws0 = workspaces.clone();
        let root0 = s.root.clone();
        std::thread::spawn(move || loop {
            let mut env = Env { ..Default::default() };
            let dir = ws0.first().cloned().unwrap_or(root0.clone());
            env.branch = git::repo_context(&dir.to_string_lossy()).branch;
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
                        workspace_id: None,
                        id,
                    });
                }
                let _ = tx2.send(UiEv::Sessions(tasks));
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
            let _ = tx2.send(UiEv::Env(env));
            std::thread::sleep(Duration::from_millis(2000));
        });

        // The 50 ms poll drains the channel into state and repaints when
        // something changed; while a run streams, a 1 s tick keeps elapsed
        // labels and dots honest even with no event in between.
        let _task = cx.spawn(async move |weak, cx: &mut AsyncApp| {
            let mut last_tick = std::time::Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                if weak.update(cx, |a, cx| {
                    let changed = a.drain();
                    a.replay_history();
                    let ticking = a.is_working() && last_tick.elapsed() >= Duration::from_secs(1);
                    if ticking {
                        last_tick = std::time::Instant::now();
                    }
                    if changed || ticking {
                        cx.notify();
                    }
                }).is_err() {
                    break;
                }
            }
        });

        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    this.send(window, cx);
                }
                _ => {}
            }
        }).detach();

        let mut app = Self {
            suite: s, workspaces, ws: 0, workspace_items: Vec::new(), active_workspace_id: None, core, engine, env: Env::default(),
            tasks: Vec::new(), seen_at: HashMap::new(), selected: String::new(),
            timelines: HashMap::new(), replayed: HashSet::new(), composer, search, username_input,
            permission_mode: PermissionMode::AskEveryTime,
            attention_only: false, right_open: true, search_open: false, settings_open: false,
            permission_dropdown_open: false,
            username: "Liso7150".to_string(),
            expanded: HashSet::new(), open_thoughts: HashSet::new(),
            toasts: Vec::new(), line: String::new(), pending_send: None,
            composer_restore: None, chat_cancel: None, rx, tx, busy: false,
            task_queue: Vec::new(), timeline_scroll: ScrollHandle::new(), follow_tail: Cell::new(true),
            _task,
            model: load_settings().model.unwrap_or_else(|| "qwen36_35b".into()),
            theme: load_settings().theme.unwrap_or_else(|| "dark".into()),
            font_size: load_settings().font_size.unwrap_or(14.0),
            auto_save: load_settings().auto_save.unwrap_or(true),
            notifications: load_settings().notifications.unwrap_or(true),
            debug_mode: load_settings().debug_mode.unwrap_or(false),
            experimental_features: load_settings().experimental_features.unwrap_or(false),
            hovered_ws_id: None,
            model_dropdown_open: false,
            attachments: Vec::new(),
            renaming_task: None,
            stt_recording: false,
            stt_stop: Arc::new(AtomicBool::new(false)),
            stt_buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
            stt_rate: Arc::new(std::sync::atomic::AtomicU32::new(16000)),
            stt_stream: None,
        };
        app.load_workspaces();
        app
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

    fn model_display_name(&self) -> String {
        match self.model.as_str() {
            "qwen3_5_moe" => "Qwen3.5-MoE".to_string(),
            "edge0_145b" => "Edge0-145B".to_string(),
            "qwen36_35b" => "Qwen36-35B".to_string(),
            _ => self.model.clone(),
        }
    }

    /// The status a task's pane shows. A task with no timeline yet has not
    /// started — the library's `Timeline::default()` exists for event assembly
    /// and must not leak into the UI as a phantom "Running".
    fn tl_status(&self, id: &str) -> TaskStatus {
        self.timelines.get(id).map(|t| t.status).unwrap_or(TaskStatus::Waiting)
    }

    fn tl(&mut self, id: &str) -> &mut Timeline {
        self.timelines.entry(id.to_string()).or_default()
    }

    /// Follow one supervised run: pull the steps the loop kept, push them into
    /// the timeline, and stop when the run is no longer running. This serves a
    /// task started here and a task the service already knew about, which is
    /// why restarting the app does not lose a finished run's history.
    fn follow(&self, id: String) {
        let (core, tx) = (self.core.clone(), self.tx.clone());
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
                                let _ = tx.send(UiEv::Task(id.clone(), e));
                            }
                        }
                        let finished = v["status"].as_str().map(|s| s != "running").unwrap_or(true);
                        // `pending` is the questions this run still holds. A
                        // service that does not report the list is not the same
                        // as an empty one — except once the run is over, when it
                        // cannot be waiting on anyone.
                        let live = match (v["pending"].as_array(), finished) {
                            (Some(list), _) => Some(list.iter().filter_map(Value::as_str)
                                .map(str::to_string).collect::<Vec<String>>()),
                            (None, true) => Some(Vec::new()),
                            (None, false) => None,
                        };
                        if let Some(ids) = live {
                            let _ = tx.send(UiEv::Pending(id.clone(), ids));
                        }
                        if finished {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(UiEv::Line(format!("Polling stopped: {e}")));
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }

    /// The rail lists sessions from the service, but a timeline is built from
    /// the events this window watched. So a task selected after a restart — or
    /// started in another shell — opens empty unless its steps are fetched.
    fn replay_history(&mut self) {
        let id = self.selected.clone();
        if id.is_empty() || id.starts_with("draft-") || self.replayed.contains(&id) {
            return;
        }
        self.replayed.insert(id.clone());
        if self.timelines.get(&id).map(|t| !t.messages.is_empty()).unwrap_or(false) {
            return;
        }
        let goal = self.tasks.iter().find(|t| t.id == id)
            .map(|t| t.goal.clone()).unwrap_or_default();
        if !goal.is_empty() {
            // The prompt itself is not one of the loop's steps; the row that
            // started the run put it at the top. Its time is the service's, not
            // this window's, so no stamp is invented for it.
            self.tl(&id).messages.push(ChatMessage { role: Role::User, text: goal, time: String::new() });
        }
        self.follow(id);
    }

    /// Returns whether anything arrived, so the poll loop knows to repaint.
    fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(ev) = self.rx.try_recv() {
            changed = true;
            match ev {
                UiEv::Task(id, e) => {
                    let before = self.timelines.get(&id).map(|t| t.status);
                    let tl = self.timelines.entry(id.clone()).or_default();
                    tl.apply(e);
                    if id == self.selected {
                        self.follow_tail.set(true);
                    }
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
                    // Status transitions, not every streamed delta, are what
                    // history persists — the send path already wrote Running.
                    if before != Some(status) {
                        self.save_history();
                    }
                }
                UiEv::Pending(id, live) => {
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
                    if before != Some(status) {
                        self.save_history();
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
                UiEv::Answered(id, approval_id, decision, error) => {
                    let tl = self.timelines.entry(id).or_default();
                    tl.answer_approval(&approval_id, &decision, error);
                }
                UiEv::Env(e) => {
                    self.env = e;
                    self.flush_warm_queue();
                }
                UiEv::Sessions(list) => {
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
                        self.follow_tail.set(true);
                    }
                }
                UiEv::Line(s) => self.line = s,
                UiEv::Attachments(atts) => {
                    self.attachments.extend(atts);
                }
                UiEv::SttText(text) => {
                    self.stt_recording = false;
                    self.line = String::new();
                    if !text.is_empty() {
                        self.composer_restore = Some(text);
                    }
                }
            }
        }
        // Process task queue when AI becomes idle
        self.process_queue();
        changed
    }

    /// A toast expires on its own — `render` drops the stale ones; only the
    /// four newest are kept so a burst cannot pile up behind a busy frame.
    fn toast(&mut self, text: impl Into<String>, good: bool) {
        self.toasts.push(Toast { text: text.into(), good, at: std::time::Instant::now() });
        if self.toasts.len() > 4 {
            let extra = self.toasts.len() - 4;
            self.toasts.drain(..extra);
        }
    }

    fn new_draft(&mut self) {
        let id = format!("draft-{}", millis());
        self.tasks.insert(0, Task {
            id: id.clone(),
            title: "New task".into(),
            status: TaskStatus::Waiting,
            updated: "now".into(),
            goal: String::new(),
            draft: true,
            workspace_id: None,
        });
        self.selected = id.clone();
        self.follow_tail.set(true);
        self.save_history();
    }

    fn cycle_model(&mut self) {
        let models = ["qwen3_5_moe", "edge0_145b", "qwen36_35b"];
        let current = models.iter().position(|m| m == &self.model.as_str()).unwrap_or(2);
        let next = models[(current + 1) % models.len()];
        self.model = next.to_string();
        self.save_settings();
    }

    fn save_settings(&self) {
        let settings = Settings {
            model: Some(self.model.clone()),
            theme: Some(self.theme.clone()),
            font_size: Some(self.font_size),
            auto_save: Some(self.auto_save),
            notifications: Some(self.notifications),
            debug_mode: Some(self.debug_mode),
            experimental_features: Some(self.experimental_features),
            ..Default::default()
        };
        save_settings(&settings);
    }

    fn add_workspace(&mut self, name: String, source_folders: Vec<String>, icon: String, color: String, indexed: bool) {
        let mut ws = Workspace::new(name);
        ws.source_folders = source_folders;
        ws.icon = icon;
        ws.color = color;
        ws.indexed = indexed;
        let id = ws.id.clone();
        self.workspace_items.push(ws);
        self.active_workspace_id = Some(id);
        self.save_workspaces();
    }

    fn delete_workspace(&mut self, id: &str) {
        self.workspace_items.retain(|w| w.id != id);
        if self.active_workspace_id.as_deref() == Some(id) {
            self.active_workspace_id = self.workspace_items.first().map(|w| w.id.clone());
        }
        self.save_workspaces();
    }

    fn select_workspace(&mut self, id: &str) {
        self.active_workspace_id = Some(id.to_string());
        self.save_workspaces();
    }

    fn save_workspaces(&self) {
        let store = WorkspaceStore {
            workspaces: self.workspace_items.clone(),
            active_id: self.active_workspace_id.clone(),
        };
        let path = settings_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("workspaces.json");
        if let Ok(json) = serde_json::to_string_pretty(&store) {
            let _ = std::fs::write(path, json);
        }
    }

    fn load_workspaces(&mut self) {
        let path = settings_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("workspaces.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(store) = serde_json::from_str::<WorkspaceStore>(&content) {
                self.workspace_items = store.workspaces;
                self.active_workspace_id = store.active_id;
            }
        }
        if self.workspace_items.is_empty() {
            self.add_workspace("Default".into(), vec![], "folder".into(), "orange".into(), true);
        }
    }

    fn delete_task(&mut self, id: &str) {
        self.tasks.retain(|t| t.id != id);
        self.timelines.remove(id);
        self.seen_at.remove(id);
        self.replayed.remove(id);
        if self.selected == id {
            self.selected = self.tasks.first().map(|t| t.id.clone()).unwrap_or_default();
        }
        self.save_history();
    }

    fn rename_task(&mut self, id: &str, new_title: String) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.title = new_title;
            self.save_history();
        }
    }

    /// Open file picker and attach files to the current message
    fn attach_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new()
                .set_title("Attach files")
                .pick_files();
            if let Some(paths) = files {
                let mut attachments = Vec::new();
                for path in paths {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                    let mime = match ext.as_str() {
                        "png" => "image/png",
                        "jpg" | "jpeg" => "image/jpeg",
                        "gif" => "image/gif",
                        "webp" => "image/webp",
                        "pdf" => "application/pdf",
                        "txt" => "text/plain",
                        "rs" | "py" | "js" | "ts" | "json" | "toml" | "yaml" | "yml" | "md" => "text/plain",
                        _ => "application/octet-stream",
                    };
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let data = if mime.starts_with("image/") {
                        std::fs::read(&path).ok().map(|bytes| general_purpose::STANDARD.encode(&bytes))
                    } else {
                        None
                    };
                    attachments.push(Attachment { path, name, mime_type: mime.to_string(), data, size });
                }
                let _ = tx.send(UiEv::Attachments(attachments));
            }
        });
    }

    /// Remove an attachment by index
    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.attachments.len() {
            self.attachments.remove(index);
            cx.notify();
        }
    }

    /// Start speech recognition: capture the default mic until stopped, then
    /// transcribe via the faster-whisper sidecar on port 8183.
    fn start_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stt_recording {
            self.stop_stt(window, cx);
            return;
        }
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(d) => d,
            None => {
                window.push_notification(gpui_kit::component::notification::Notification::info("No microphone found."), cx);
                return;
            }
        };
        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => {
                window.push_notification(gpui_kit::component::notification::Notification::info(format!("Mic config error: {e}")), cx);
                return;
            }
        };
        let rate = config.sample_rate().0;
        self.stt_rate.store(rate, Ordering::Relaxed);
        self.stt_buffer.lock().unwrap().clear();
        self.stt_stop.store(false, Ordering::Relaxed);
        let buffer = self.stt_buffer.clone();
        let channels = config.channels() as usize;
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(&config.into(), move |data: &[f32], _| {
                let mut b = buffer.lock().unwrap();
                for frame in data.chunks(channels) { b.push(frame[0]); }
            }, |e| eprintln!("stt stream error: {e}"), None),
            cpal::SampleFormat::I16 => {
                let buffer = self.stt_buffer.clone();
                device.build_input_stream(&config.into(), move |data: &[i16], _| {
                    let mut b = buffer.lock().unwrap();
                    for frame in data.chunks(channels) { b.push(frame[0] as f32 / 32768.0); }
                }, |e| eprintln!("stt stream error: {e}"), None)
            },
            _ => {
                window.push_notification(gpui_kit::component::notification::Notification::info("Unsupported mic format."), cx);
                return;
            }
        };
        let stream = match stream { Ok(s) => s, Err(e) => {
            window.push_notification(gpui_kit::component::notification::Notification::info(format!("Mic open error: {e}")), cx);
            return;
        }};
        if stream.play().is_err() {
            window.push_notification(gpui_kit::component::notification::Notification::info("Could not start mic."), cx);
            return;
        }
        self.stt_recording = true;
        self.stt_stream = Some(stream);
        self.line = "Recording… click the mic again to stop and transcribe.".to_string();
        cx.notify();
        // Background thread waits for the stop flag, then transcribes the buffer.
        let stop = self.stt_stop.clone();
        let buffer = self.stt_buffer.clone();
        let rate_arc = self.stt_rate.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let samples = buffer.lock().unwrap().clone();
            let rate = rate_arc.load(Ordering::Relaxed);
            let text = transcribe_samples(&samples, rate);
            let _ = tx.send(UiEv::SttText(text));
        });
    }

    /// Stop speech recognition and transcribe
    fn stop_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stt_stop.store(true, Ordering::Relaxed);
        self.stt_stream = None; // drop the capture stream
        self.line = "Transcribing…".to_string();
        cx.notify();
    }

    /// Start renaming a task
    fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current_title = self.tasks.iter().find(|t| t.id == id).map(|t| t.title.clone()).unwrap_or_default();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(current_title, window, cx);
            state
        });
        self.renaming_task = Some((id.to_string(), input));
        cx.notify();
    }

    /// Finish renaming a task
    fn finish_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((id, input)) = self.renaming_task.take() {
            let new_title = input.read(cx).value().trim().to_string();
            if !new_title.is_empty() {
                self.rename_task(&id, new_title);
            }
            cx.notify();
        }
    }

    /// Cancel renaming
    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming_task = None;
        cx.notify();
    }

    /// The whole life of a prompt queued behind a cold engine: send it when both
    /// backends answer, give it back the moment the service stops answering, keep
    /// waiting inside the window, and hand it back to the composer when the window
    /// closes — a dropped prompt is worse than a late one.
    fn flush_warm_queue(&mut self) {
        let Some((text, queued)) = self.pending_send.take() else { return };
        if self.env.core_up && self.env.engine_up {
            self.send_text(text);
        } else if !self.env.core_up {
            self.composer_restore = Some(text);
            self.line = "The agent loop stopped answering. The queued prompt is back in the composer."
                .to_string();
        } else if queued.elapsed() < WARM_WINDOW {
            self.pending_send = Some((text, queued));
        } else {
            self.composer_restore = Some(text);
            self.line = "The engine did not answer within the warm-up window. \
                The queued prompt is back in the composer."
                .to_string();
        }
    }

    /// Sending has exactly two real paths, chosen by which backend answers:
    /// the agent loop (tool events, jail switches) or the engine's chat stream.
    /// With neither reachable the prompt stays on screen and the app says so.
    /// Called from the Enter hook and the Send button — both hold a `Window`.
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        // If the agent is already working on a task, queue this one instead of
        // sending it.
        if self.is_working() {
            self.queue_or_send(text);
        } else {
            self.send_text(text);
        }
        cx.notify();
    }

    /// The send path on its own, so a prompt queued behind a cold engine can be
    /// re-sent without putting it back in the composer first.
    fn send_text(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.line.clear();
        if self.env.core_up {
            if !self.env.engine_up {
                // `/api/agent/start` answers 503 while the engine is down, so the
                // prompt is queued and the service is asked to warm the engine.
                // The status poll re-sends it once the model answers; warming
                // takes minutes, so nothing here may block the UI thread.
                if self.pending_send.is_some() {
                    // One prompt per warm-up. There is a single composer, and
                    // overwriting what it holds would drop the user's earlier
                    // words without ever showing them where they went.
                    self.composer_restore = Some(text);
                    self.line = "The engine is still warming and it already holds a prompt. \
                        This one stays in the composer until that one sends."
                        .to_string();
                    return;
                }
                self.pending_send = Some((text, std::time::Instant::now()));
                self.line = "Engine is offline - starting it. This prompt sends itself once the model answers.".to_string();
                if let Err(e) = self.core.engine_start() {
                    // A failed ask is not evidence that nothing is warming. Measured
                    // on the live service, this POST hit its client timeout and the
                    // engine came up anyway, so the prompt stays queued and the
                    // status poll - which is what actually knows - decides.
                    self.line = format!("Asked the agent loop to start the engine, and it did not \
                        confirm ({e}). The prompt stays queued until the model answers.");
                }
                return;
            }
            let workspace = self.workspace();
            let (allow_write, allow_shell, allow_web, ask_each) = match self.permission_mode {
                PermissionMode::AskEveryTime => (false, false, false, true),
                PermissionMode::AutoApprove => (true, true, true, false),
                PermissionMode::FullAccess => (true, true, true, false),
            };
            match self.core.start(&text, &workspace, allow_write, allow_shell,
                                  allow_web, ask_each) {
                Ok(id) => {
                    let tl = self.tl(&id);
                    tl.messages.push(ChatMessage { role: Role::User, text: text.clone(), time: events::now_stamp() });
                    tl.status = TaskStatus::Running;
                    tl.streaming = true;
                    self.tasks.insert(0, Task {
                        id: id.clone(), title: truncate(&text, 58), status: TaskStatus::Running,
                        updated: "now".into(), goal: text, draft: false, workspace_id: self.active_workspace_id.clone(),
                    });
                    self.selected = id.clone();
                    self.follow_tail.set(true);
                    self.seen_at.insert(id.clone(), events::now_stamp());
                    self.busy = true;
                    self.follow(id.clone());
                    self.save_history();
                }
                Err(e) => {
                    // The prompt goes back to the composer rather than being
                    // discarded with the failure.
                    self.composer_restore = Some(text);
                    self.line = format!("Agent server refused the task: {e}");
                }
            }
            return;
        }
        if self.env.engine_up {
            let id = if self.selected.starts_with("draft-") {
                self.selected.clone()
            } else {
                let id = format!("draft-{}", millis());
                self.tasks.insert(0, Task {
                    id: id.clone(), title: "New task".into(), status: TaskStatus::Waiting,
                    updated: "now".into(), goal: String::new(), draft: true, workspace_id: self.active_workspace_id.clone(),
                });
                self.selected = id.clone();
                self.follow_tail.set(true);
                id
            };
            let mut messages: Vec<Value> = self.timelines.get(&id).map(|tl| tl.messages.iter()
                .filter(|m| matches!(m.role, Role::User | Role::Assistant))
                .map(|m| json!({"role": if m.role == Role::User { "user" } else { "assistant" }, "content": m.text}))
                .collect()).unwrap_or_default();
            // Attachments: images become vision content parts; text files are
            // inlined into the prompt. Cleared once handed to the request.
            let atts = std::mem::take(&mut self.attachments);
            let mut prompt = text.clone();
            let mut parts: Vec<Value> = Vec::new();
            for att in &atts {
                if att.mime_type.starts_with("image/") {
                    if let Some(data) = &att.data {
                        parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", att.mime_type, data)}}));
                    }
                } else if let Ok(content) = std::fs::read_to_string(&att.path) {
                    prompt.push_str(&format!("\n\n[{}]\n{}", att.name, content));
                }
            }
            if parts.is_empty() {
                messages.push(json!({"role": "user", "content": prompt}));
            } else {
                let mut content = vec![json!({"type": "text", "text": prompt})];
                content.extend(parts);
                messages.push(json!({"role": "user", "content": content}));
            }
            {
                let tl = self.tl(&id);
                tl.messages.push(ChatMessage { role: Role::User, text: text.clone(), time: events::now_stamp() });
                tl.streaming = true;
                tl.status = TaskStatus::Running;
            }
            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                t.title = truncate(&text, 58);
                t.goal = text.clone();
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
                        ChatDelta::Reasoning(r) => UiEv::Task(id.clone(), Event::Message(ChatMessage { role: Role::Progress, text: r, time: events::now_stamp() })),
                        ChatDelta::Content(c) => UiEv::Task(id.clone(), Event::Partial(c)),
                        ChatDelta::Done => UiEv::Task(id.clone(), Event::Status(TaskStatus::Complete)),
                        ChatDelta::Failed(e) => UiEv::Line(format!("Engine stream failed: {e}")),
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
            });
            self.busy = true;
            return;
        }
        let id = if self.selected.is_empty() {
            let id = format!("draft-{}", millis());
            self.tasks.insert(0, Task {
                id: id.clone(), title: "New task".into(), status: TaskStatus::Waiting,
                updated: "now".into(), goal: String::new(), draft: true, workspace_id: self.active_workspace_id.clone(),
            });
            self.selected = id.clone();
            self.follow_tail.set(true);
            id
        } else {
            self.selected.clone()
        };
        self.tl(&id).messages.push(ChatMessage { role: Role::User, text, time: events::now_stamp() });
        self.line = "No backend reachable — start the engine (`fennec serve --auto-tier`) or the agent server (`fennec-agent serve`).".to_string();
    }

    /// Hand one answer to the service off the UI thread, then report what it
    /// said. A click is not a claim that the call ran: only the `tool` step
    /// that follows one can say that, and it arrives on its own.
    fn answer(&mut self, approval_id: &str, decision: &str) {
        let (core, tx, sid) = (self.core.clone(), self.tx.clone(), self.selected.clone());
        let (approval_id, decision) = (approval_id.to_string(), decision.to_string());
        std::thread::spawn(move || {
            let error = core.approve(&sid, &approval_id, &decision).err();
            let _ = tx.send(UiEv::Answered(sid, approval_id, decision, error));
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
            // The rail reads Task.status, not the timeline; a stop that only
            // touched the timeline leaves the row saying Running forever —
            // including across restarts, because history serializes it.
            let status = self.tl(&id).status;
            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                t.status = status;
            }
            self.save_history();
        }
        self.busy = false;
    }

    /// The shared context-budgeting crate does this for every Fennec surface;
    /// the desktop only reports what it changed.
    fn compact_selected(&mut self) {
        let Some(tl) = self.timelines.get(&self.selected).cloned() else {
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

    // ---- History persistence ----

    /// Where history is saved. Uses LOCALAPPDATA on Windows, XDG_DATA_HOME or
    /// ~/.local/share on Linux, ~/Library/Application Support on macOS. The
    /// egui binary writes the same file, so the two shells share one history.
    fn history_path() -> Option<PathBuf> {
        let dir = std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        let fennec_dir = dir.join(".fennec");
        let _ = std::fs::create_dir_all(&fennec_dir);
        Some(fennec_dir.join("history.json"))
    }

    fn save_history(&self) {
        let Some(path) = Self::history_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = HistoryData {
            // Empty drafts are not history: first run stays empty, real
            // conversations persist across restarts.
            tasks: self.tasks.iter()
                .filter(|t| !t.draft || self.timelines.get(&t.id).map(|tl| !tl.messages.is_empty()).unwrap_or(false))
                .map(|t| HistoryTask {
                id: t.id.clone(),
                title: t.title.clone(),
                status: match t.status {
                    TaskStatus::Running => "Running",
                    TaskStatus::Complete => "Complete",
                    TaskStatus::NeedsAttention => "NeedsAttention",
                    TaskStatus::Waiting => "Waiting",
                    TaskStatus::AwaitingApproval => "AwaitingApproval",
                }.to_string(),
                updated: t.updated.clone(),
                goal: t.goal.clone(),
                draft: t.draft,
                workspace_id: t.workspace_id.clone(),
            }).collect(),
            // Only timelines that belong to a task row; a stale key with no
            // task (like the corrupt "" entry) would re-serialize forever.
            timelines: self.timelines.iter()
                .filter(|(id, tl)| self.tasks.iter().any(|t| &t.id == *id) && (!tl.messages.is_empty() || !tl.summary.is_empty()))
                .map(|(id, tl)| {
                (id.clone(), HistoryTimeline {
                    messages: tl.messages.iter().map(|m| HistoryMessage {
                        role: match m.role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                            Role::Progress => "Progress",
                        }.to_string(),
                        text: m.text.clone(),
                        time: m.time.clone(),
                    }).collect(),
                    summary: tl.summary.clone(),
                    status: match tl.status {
                        TaskStatus::Running => "Running",
                        TaskStatus::Complete => "Complete",
                        TaskStatus::NeedsAttention => "NeedsAttention",
                        TaskStatus::Waiting => "Waiting",
                        TaskStatus::AwaitingApproval => "AwaitingApproval",
                    }.to_string(),
                })
            }).collect(),
            seen_at: self.seen_at.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&data) {
            let _ = std::fs::write(&path, json);
        }
    }

    fn load_history(&mut self) {
        let Some(path) = Self::history_path() else { return };
        let Ok(contents) = std::fs::read_to_string(&path) else { return };
        let Ok(data) = serde_json::from_str::<HistoryData>(&contents) else { return };
        self.seen_at = data.seen_at;
        // A status persisted as Running is stale by definition: whatever owned
        // it died with the previous process. Restoring it would phantom-busy
        // the whole app (the queue would wait on a session that cannot finish).
        self.tasks = data.tasks.into_iter().map(|t| Task {
            id: t.id,
            title: t.title,
            status: match t.status.as_str() {
                "Complete" => TaskStatus::Complete,
                "NeedsAttention" => TaskStatus::NeedsAttention,
                "AwaitingApproval" => TaskStatus::AwaitingApproval,
                _ => TaskStatus::Waiting,
            },
            updated: t.updated,
            goal: t.goal,
            draft: t.draft,
            workspace_id: t.workspace_id,
        }).collect();
        for (id, htl) in data.timelines {
            let mut tl = Timeline::default();
            tl.messages = htl.messages.into_iter().map(|m| ChatMessage {
                role: match m.role.as_str() {
                    "User" => Role::User,
                    "Assistant" => Role::Assistant,
                    "Progress" => Role::Progress,
                    _ => Role::User,
                },
                text: m.text,
                time: m.time,
            }).collect();
            tl.summary = htl.summary;
            tl.status = match htl.status.as_str() {
                "Complete" => TaskStatus::Complete,
                "NeedsAttention" => TaskStatus::NeedsAttention,
                "AwaitingApproval" => TaskStatus::AwaitingApproval,
                _ => TaskStatus::Waiting,
            };
            self.timelines.insert(id, tl);
        }
        // Drop empty drafts so a fresh install shows an empty history.
        self.tasks.retain(|t| !t.draft || self.timelines.get(&t.id).map(|tl| !tl.messages.is_empty()).unwrap_or(false));
    }

    // ---- Task queue ----

    /// Queue a prompt while the agent works; send immediately when idle.
    fn queue_or_send(&mut self, text: String) -> bool {
        if !self.is_working() {
            self.send_text(text);
            return false;
        }
        self.task_queue.push(QueuedTask { text });
        self.line = format!("Queued task ({} in queue). Will start when current task finishes.", self.task_queue.len());
        true
    }

    /// Check if AI is currently working on a task.
    fn is_working(&self) -> bool {
        self.busy || self.timelines.values().any(|tl| tl.status == TaskStatus::Running)
    }

    /// Process the next queued task if AI is idle and queue is non-empty.
    fn process_queue(&mut self) {
        if !self.is_working() && !self.task_queue.is_empty() {
            let next = self.task_queue.remove(0);
            self.send_text(next.text);
        }
    }

    fn remove_from_queue(&mut self, index: usize) {
        if index < self.task_queue.len() {
            self.task_queue.remove(index);
        }
    }

    fn attention_count(&self) -> usize {
        self.tasks.iter()
            .filter(|t| matches!(t.status, TaskStatus::NeedsAttention | TaskStatus::AwaitingApproval))
            .count()
    }

    fn filtered_tasks(&self, query: &str) -> Vec<&Task> {
        let q = query.to_lowercase();
        self.tasks.iter()
            .filter(|t| !self.attention_only || matches!(t.status, TaskStatus::NeedsAttention | TaskStatus::AwaitingApproval))
            .filter(|t| q.is_empty() || t.title.to_lowercase().contains(&q) || t.goal.to_lowercase().contains(&q))
            .collect()
    }

    /// Show what git says about the file an edit touched. A single-file diff is
    /// normally a few milliseconds, so the fetch happens in the click handler
    /// and the dialog opens with its content already resolved.
    fn review_edit(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.workspace();
        let (text, note) = match git::git_out(&dir, &["diff", "--", &path]) {
            Some(d) if d.trim().is_empty() => (
                None,
                "git reports no unstaged diff for this path — the change is committed, \
                 untracked, or outside this repo."
                    .to_string(),
            ),
            Some(d) => (Some(d), String::new()),
            None => (
                None,
                "git diff returned nothing usable — git is missing, or this path is outside the repo."
                    .to_string(),
            ),
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let (text, note) = (text.clone(), note.clone());
            dialog.title(format!("Review diff · {path}"))
                .content(move |content, _, cx| {
                    let (text, note) = (text.clone(), note.clone());
                    let t = cx.theme().clone();
                    let mut col = div().max_h(px(480.0)).overflow_y_scrollbar().flex().flex_col().gap_1().px_1();
                    if !note.is_empty() {
                        col = col.child(Label::new(note).text_size(px(12.0)).text_color(t.yellow));
                    }
                    if let Some(text) = text {
                        let (lines, extra) = diff_lines(&text);
                        for (di, (line, kind)) in lines.into_iter().enumerate() {
                            let color = match kind {
                                1 => t.success,
                                2 => t.red,
                                3 => t.primary,
                                _ => t.foreground,
                            };
                            col = col.child(div().id(ElementId::NamedInteger("diffl".into(), di as u64)).font_family("monospace")
                                .child(Label::new(line).text_size(px(11.0)).text_color(color)));
                        }
                        if extra > 0 {
                            col = col.child(Label::new(format!(
                                "… and {extra} more lines — the full text is one `git diff` away."
                            )).text_size(px(11.0)).text_color(t.muted_foreground));
                        }
                    }
                    content.child(col)
                })
                .footer(div().flex().justify_end().gap_2().p_3()
                    .child(Button::new("review-close").label("Close").ghost()
                        .on_click(|_, window, cx| { window.close_dialog(cx); })))
        });
    }
}

// ---- Rendering ----

fn status_color(t: &Theme, s: TaskStatus) -> Hsla {
    match s {
        TaskStatus::Running => t.primary,
        TaskStatus::Complete => t.success,
        TaskStatus::NeedsAttention => t.red,
        TaskStatus::Waiting => t.muted_foreground,
        // The run is stopped on the user, not on the model — the one state in
        // this list that only an answer here can clear.
        TaskStatus::AwaitingApproval => t.yellow,
    }
}

/// The backend's own risk tiers, in the palette's three signal colours. A risk
/// the gate table did not name gets the neutral colour, never green.
fn risk_color(t: &Theme, risk: &str) -> Hsla {
    match risk {
        "high" => t.red,
        "medium" => t.yellow,
        "low" => t.success,
        _ => t.muted_foreground,
    }
}

fn tool_color(t: &Theme, s: ToolStatus) -> Hsla {
    match s {
        ToolStatus::Running => t.primary,
        ToolStatus::Complete => t.success,
        ToolStatus::Warning => t.yellow,
        ToolStatus::Failed => t.red,
    }
}

/// A unified diff, line-tinted. Long diffs are truncated with a note rather
/// than paged: this is a preview, and the full text is one `git diff` away.
fn diff_lines(text: &str) -> (Vec<(String, u8)>, usize) {
    const MAX_LINES: usize = 400;
    let all: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for line in all.iter().take(MAX_LINES) {
        let kind = if line.starts_with('+') && !line.starts_with("+++") { 1 }
            else if line.starts_with('-') && !line.starts_with("---") { 2 }
            else if line.starts_with('@') { 3 }
            else { 0 };
        out.push(((*line).to_string(), kind));
    }
    let extra = all.len().saturating_sub(MAX_LINES);
    (out, extra)
}

/// The glyph standing in for a tool card's category icon.
fn tool_icon(category: &str) -> &'static str {
    match category {
        "READ" => "🔍",
        "EDIT" => "✎",
        "CMD" => "⚙",
        _ => "•",
    }
}

/// Write captured mono f32 samples to a WAV and POST to the faster-whisper
/// sidecar (port 8183). Tries the OpenAI-compatible route then /transcribe.
fn transcribe_samples(samples: &[f32], rate: u32) -> String {
    if samples.is_empty() {
        return String::new();
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = Vec::new();
    {
        let mut writer = match hound::WavWriter::new(std::io::Cursor::new(&mut wav), spec) {
            Ok(w) => w,
            Err(_) => return String::new(),
        };
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
            let _ = writer.write_sample(v);
        }
        let _ = writer.finalize();
    }
    let client = match reqwest::blocking::Client::builder().timeout(Duration::from_secs(60)).build() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    for path in ["/v1/audio/transcriptions", "/transcribe"] {
        let url = format!("http://127.0.0.1:8183{}", path);
        let part = match reqwest::blocking::multipart::Part::bytes(wav.clone())
            .file_name("audio.wav")
            .mime_str("audio/wav")
        {
            Ok(p) => p,
            Err(_) => continue,
        };
        let form = reqwest::blocking::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1");
        if let Ok(resp) = client.post(&url).multipart(form).send() {
            if let Ok(json) = resp.json::<Value>() {
                if let Some(text) = json.get("text").and_then(|t| t.as_str()) {
                    return text.to_string();
                }
            }
        }
    }
    String::new()
}

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A prompt that went back to the composer after a failed send is
        // applied here: this is the one pass where the input state is both
        // reachable and about to be shown.
        if let Some(text) = self.composer_restore.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, window, cx));
        }
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_secs(6));
        let t = cx.theme().clone();
        let settings_open = self.settings_open;
        div().flex().flex_col().size_full().bg(t.background).text_color(t.foreground)
            .child(self.topbar(cx))
            .child(div().flex().flex_1().overflow_hidden()
                .child(self.left(cx))
                .child(if settings_open {
                    self.settings_panel(cx).into_any_element()
                } else {
                    self.center(cx).into_any_element()
                })
                .children(self.right_open.then(|| self.right(cx))))
            .child(self.dropdowns(cx))
    }
}

impl App {
    fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let (core_up, engine_up) = (self.env.core_up, self.env.engine_up);
        let connected = core_up && engine_up;
        let status_label = if connected { "Backend connected" } else if core_up { "Agent loop only" } else if engine_up { "Engine only" } else { "No backend" };
        let status_color = if connected { t.success } else { t.yellow };

        div().flex().items_center().justify_between().px_4().py_2().bg(t.background).border_b_1().border_color(t.border)
            .child(div().flex().items_center().gap_3()
                // Shield icon
                .child(div().w(px(20.0)).h(px(20.0)).flex().items_center().justify_center()
                    .child(Label::new("🛡").text_size(px(16.0)).text_color(t.primary)))
                .child(div().flex().flex_col()
                    .child(Label::new("Fennec").text_size(px(16.0)).font_weight(FontWeight::BOLD))
                    .child(Label::new("Liso7150 / local-first workspace").text_size(px(11.0)).text_color(t.muted_foreground))))
            .child(div().flex().items_center().gap_3()
                .child(div().flex().items_center().gap_1()
                    .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(status_color))
                    .child(Label::new(status_label).text_size(px(12.0)).text_color(status_color)))
                .child(Button::new("topbar-settings").label("⚙").ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.settings_open = !a.settings_open; cx.notify(); })))
                .child(Button::new("inspector").label(if self.right_open { "⊟" } else { "⊞" }).ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.right_open = !a.right_open; cx.notify(); }))))
    }

    fn left(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let query = self.search.read(cx).value().trim().to_string();
        let attention = self.attention_count();
        let ws_name = self.workspace_name();
        let branch_display = if self.env.branch.is_empty() { "main".to_string() } else { self.env.branch.clone() };

        let mut rail = div().w(px(280.0)).flex_shrink_0().bg(t.background).border_r_1().border_color(t.border)
            .flex().flex_col().p_3().gap_3().overflow_y_scrollbar();

        // Coding header with + button
        rail = rail.child(div().flex().items_center().gap_2()
            .child(Button::new("nav-coding").label("Coding").ghost().small()
                .bg(t.muted.clone())
                .on_click(cx.listener(|a, _, _, cx| { a.attention_only = false; a.search_open = false; cx.notify(); })))
            .child(Button::new("nav-new-task").label("+").ghost().small().w(px(24.0)).h(px(24.0))
                .on_click(cx.listener(|a, _, _, cx| { a.new_draft(); cx.notify(); }))));

        // Nav items
        rail = rail.child(div().flex().flex_col().gap_1()
            .child(Button::new("nav-new-task2").ghost().small().w_full().justify_start()
                .child(div().flex().items_center().gap_2()
                    .child(Label::new("⟳").text_size(px(14.0)))
                    .child(Label::new("New Task").text_size(px(13.0))))
                .on_click(cx.listener(|a, _, _, cx| { a.new_draft(); cx.notify(); })))
            .child(Button::new("nav-attention").ghost().small().w_full().justify_start()
                .child(div().flex().items_center().gap_2()
                    .child(Label::new("▷").text_size(px(14.0)))
                    .child(Label::new("My Attention").text_size(px(13.0))))
                .on_click(cx.listener(|a, _, _, cx| { a.attention_only = !a.attention_only; cx.notify(); })))
            .child(Button::new("nav-search").ghost().small().w_full().justify_start()
                .child(div().flex().items_center().gap_2()
                    .child(Label::new("⌕").text_size(px(14.0)))
                    .child(Label::new("Search").text_size(px(13.0))))
                .on_click(cx.listener(|a, _, _, cx| { a.search_open = !a.search_open; a.attention_only = false; cx.notify(); }))));

        // Search dock
        rail = rail.children(self.search_open.then(|| {
            div().id("search-dock").flex().flex_col().gap_1()
                .child(Input::new(&self.search).h(px(28.0)).text_size(px(12.0)))
        }));

        // WORKSPACES section header with + button
        rail = rail.child(div().flex().items_center().justify_between().id("ws-header")
            .child(Label::new("Workspaces").text_size(px(11.0)).text_color(t.muted_foreground))
            .child(Button::new("ws-add-header").label("+").ghost().small().w(px(22.0)).h(px(22.0)).text_color(t.muted_foreground)
                .on_click(cx.listener(|_a, _, window, cx| {
                    let weak = cx.entity().downgrade();
                    open_workspace_dialog(window, cx, move |name, folders, icon, color, indexed, cx| {
                        let _ = weak.update(cx, |a, cx| {
                            a.add_workspace(name, folders, icon, color, indexed);
                            cx.notify();
                        });
                    });
                }))));

        // Directory tree: workspaces as folder nodes, tasks as indented children.
        let filtered = self.filtered_tasks(&query);
        let workspace_data: Vec<_> = self.workspace_items.iter().map(|ws| {
            let ws_id = ws.id.clone();
            let ws_name = ws.name.clone();
            let ws_icon = ws.icon.clone();
            let is_active = self.active_workspace_id.as_deref() == Some(&ws_id);
            let ws_tasks: Vec<_> = filtered.iter()
                .filter(|t| t.workspace_id.as_deref() == Some(&ws_id))
                .map(|t| (t.id.clone(), t.title.clone(), t.status, self.selected == t.id))
                .collect();
            (ws_id, ws_name, ws_icon, is_active, ws_tasks)
        }).collect();
        let unassigned: Vec<_> = filtered.iter()
            .filter(|t| t.workspace_id.is_none())
            .map(|t| (t.id.clone(), t.title.clone(), t.status, self.selected == t.id))
            .collect();

        let mut tree = div().flex().flex_col().gap_0();

        for (ws_id, ws_name, ws_icon, is_active, ws_tasks) in workspace_data {
            let t = t.clone();
            let ws_id2 = ws_id.clone();
            let ws_id3 = ws_id.clone();
            let folder_glyph = if ws_icon.is_empty() || ws_icon == "folder" { "📁".to_string() } else { ws_icon.clone() };

            let mut ws_div = div().flex().flex_col()
                .child(
                    div().flex().items_center().gap_1().px_1().py_1().rounded_md()
                        .when(is_active, |d| d.bg(t.muted.clone()))
                        .child(
                            Button::new(format!("ws-select-{}", ws_id)).ghost().small().flex_1().justify_start()
                                .child(div().flex().items_center().gap_2()
                                    .child(Label::new(folder_glyph).text_size(px(14.0)))
                                    .child(Label::new(ws_name.clone()).text_size(px(13.0)).font_weight(FontWeight::MEDIUM).truncate()))
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    a.select_workspace(&ws_id);
                                    cx.notify();
                                }))
                        )
                        // new task in this workspace
                        .child(Button::new(format!("ws-add-task-{}", ws_id2)).ghost().small().px_1()
                            .child(Label::new("+").text_size(px(14.0)).text_color(t.success))
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.select_workspace(&ws_id2);
                                a.new_draft();
                                cx.notify();
                            })))
                        // delete workspace (trash, red) - visually distinct from +
                        .child(Button::new(format!("ws-del-{}", ws_id3)).ghost().small().px_1()
                            .child(Label::new("🗑").text_size(px(13.0)).text_color(t.red))
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.delete_workspace(&ws_id3);
                                cx.notify();
                            })))
                );

            // Task children indented under the workspace folder
            if !ws_tasks.is_empty() {
                let tasks_div = div().flex().flex_col().gap_0().pl_5()
                    .children(ws_tasks.into_iter().map(|(id, title, status, is_selected)| {
                        let t = t.clone();
                        let id_del = id.clone();
                        let id_rename = id.clone();
                        div().flex().items_center().gap_1().px_1().py_0p5().rounded_md()
                            .when(is_selected, |d| d.bg(t.muted.clone()))
                            .child(Button::new(format!("task-sel-{}", id)).ghost().small().flex_1().justify_start()
                                .child(div().flex().items_center().gap_2()
                                    .child(div().w(px(6.0)).h(px(6.0)).flex_shrink_0().rounded_full().bg(status_color(&t, status)))
                                    .child(Label::new(title).text_size(px(12.0)).truncate()))
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    a.selected = id.clone();
                                    cx.notify();
                                })))
                            .child(Button::new(format!("task-rename-{}", id_rename)).ghost().small().px_1()
                                .child(Label::new("✎").text_size(px(12.0)).text_color(t.muted_foreground))
                                .on_click(cx.listener(move |a, _, window, cx| {
                                    a.start_rename(&id_rename, window, cx);
                                })))
                            .child(Button::new(format!("task-del-{}", id_del)).ghost().small().px_1()
                                .child(Label::new("×").text_size(px(13.0)).text_color(t.red))
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    a.delete_task(&id_del);
                                    cx.notify();
                                })))
                    }));
                ws_div = ws_div.child(tasks_div);
            }

            tree = tree.child(ws_div);
        }

        // Unfiled tasks (no workspace) as a final tree node so nothing is lost
        if !unassigned.is_empty() {
            let t2 = t.clone();
            let mut un_div = div().flex().flex_col()
                .child(div().flex().items_center().gap_2().px_1().py_1()
                    .child(Label::new("🗂").text_size(px(14.0)))
                    .child(Label::new("Unfiled").text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(t2.muted_foreground)));
            let tasks_div = div().flex().flex_col().gap_0().pl_5()
                .children(unassigned.into_iter().map(|(id, title, status, is_selected)| {
                    let t = t.clone();
                    let id_del = id.clone();
                    let id_rename = id.clone();
                    div().flex().items_center().gap_1().px_1().py_0p5().rounded_md()
                        .when(is_selected, |d| d.bg(t.muted.clone()))
                        .child(Button::new(format!("task-sel2-{}", id)).ghost().small().flex_1().justify_start()
                            .child(div().flex().items_center().gap_2()
                                .child(div().w(px(6.0)).h(px(6.0)).flex_shrink_0().rounded_full().bg(status_color(&t, status)))
                                .child(Label::new(title).text_size(px(12.0)).truncate()))
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.selected = id.clone();
                                cx.notify();
                            })))
                        .child(Button::new(format!("task-rename2-{}", id_rename)).ghost().small().px_1()
                            .child(Label::new("✎").text_size(px(12.0)).text_color(t.muted_foreground))
                            .on_click(cx.listener(move |a, _, window, cx| {
                                a.start_rename(&id_rename, window, cx);
                            })))
                        .child(Button::new(format!("task-del2-{}", id_del)).ghost().small().px_1()
                            .child(Label::new("×").text_size(px(13.0)).text_color(t.red))
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.delete_task(&id_del);
                                cx.notify();
                            })))
                }));
            un_div = un_div.child(tasks_div);
            tree = tree.child(un_div);
        }

        rail = rail.child(tree);

        // User account at bottom
        rail = rail.child(div().mt_auto().pt_2().border_t_1().border_color(t.border)
            .child(Label::new("Liso7150").text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD))
            .child(Label::new("Local account").text_size(px(11.0)).text_color(t.muted_foreground)));

        // Rename task dialog
        if let Some((_, input)) = &self.renaming_task {
            rail = rail.child(
                div().absolute().top(px(100.0)).left(px(20.0)).right(px(20.0))
                    .rounded_md().border_1().border_color(t.border).bg(t.background).shadow_md().p_3()
                    .child(Label::new("Rename task").text_size(px(12.0)).font_weight(FontWeight::MEDIUM))
                    .child(div().mt_2().child(Input::new(input)))
                    .child(div().flex().items_center().justify_end().gap_2().mt_2()
                        .child(Button::new("rename-cancel").label("Cancel").ghost().small()
                            .on_click(cx.listener(|a, _, _, cx| {
                                a.cancel_rename(cx);
                            })))
                        .child(Button::new("rename-save").label("Save").small()
                            .on_click(cx.listener(|a, _, window, cx| {
                                a.finish_rename(window, cx);
                            }))))
            );
        }

        rail
    }

    fn nav_button_icon(&self, id: &'static str, icon: &str, label: &str, active: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let label_str = label.to_string();
        div().flex().items_center().gap_2()
            .child(Button::new(id).label(icon).ghost().small().w(px(24.0))
                .when(active, |b| b.bg(t.muted.clone()))
                .on_click(cx.listener(move |a, _, _, cx| {
                    match label_str.as_str() {
                        "Coding" => { a.attention_only = false; a.search_open = false; }
                        "New Task" => a.new_draft(),
                        "Search" => { a.search_open = !a.search_open; a.attention_only = false; }
                        _ => {}
                    }
                    cx.notify();
                })))
            .child(Label::new(label).text_size(px(13.0)))
    }

    fn center(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let (core_up, engine_up, model) = (self.env.core_up, self.env.engine_up, self.env.model.clone());
        let ws = self.workspace_name();
        let branch_display = if self.env.branch.is_empty() { "main".to_string() } else { self.env.branch.clone() };
        let tl = self.timelines.get(&self.selected).cloned().unwrap_or_default();
        let title = self.tasks.iter().find(|t| t.id == self.selected)
            .map(|t| t.title.clone()).unwrap_or_else(|| "No task selected".into());
        let busy = self.busy;
        let status = self.tl_status(&self.selected);
        let show_queue = self.is_working() && !self.task_queue.is_empty();

        let mut col = div().flex().flex_col().flex_1().overflow_hidden()
            // Header: workspace · branch, title, status pill.
            .child(div().flex().items_start().justify_between().px_5().pt_4().pb_3().border_b_1().border_color(t.border)
                .child(div().flex().flex_col().gap_1()
                    .child(Label::new(format!("{ws} · {branch_display}")).font_family("monospace").text_size(px(11.0)).text_color(t.muted_foreground))
                    .child(Label::new(title).text_size(px(20.0)).font_weight(FontWeight::BOLD))
                    .child(Label::new("Chronological local-agent activity and verification for the active repository.")
                        .text_size(px(12.0)).text_color(t.muted_foreground)))
                .child(div().flex().flex_col().items_end().gap_2()
                    .child(div().flex().items_center().gap_2()
                        .child(Button::new("stop").label("Stop").ghost().small()
                            .when(!busy, |b| b.disabled(true))
                            .on_click(cx.listener(|a, _, _, cx| { a.stop(); cx.notify(); })))
                        .child(Button::new("compact").label("⊟").ghost().small()
                            .on_click(cx.listener(|a, _, _, cx| { a.compact_selected(); cx.notify(); })))
                        .child(div().flex().items_center().gap_1().px_3().py_1().rounded_full().border_1().border_color(t.border)
                            .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(if busy { t.success } else { status_color(&t, status) }))
                            .child(Label::new(if busy { "Running" } else { status.label() }).text_size(px(12.0)))))
                    .children((!model.is_empty()).then(|| {
                        Label::new(model.clone()).font_family("monospace").text_size(px(11.0)).text_color(t.muted_foreground)
                    }))))
            // Offline note: the exact claim the layout can back.
            .children((!core_up).then(|| {
                let note = if engine_up {
                    "Agent loop offline — direct chat mode: tokens stream from the engine; the tool timeline is unavailable."
                } else {
                    "No backend reachable — run `fennec serve --auto-tier` (engine) or `fennec-agent serve` (agent loop)."
                };
                div().mx_5().mt_3().p_3().rounded_md().bg(t.muted)
                    .child(Label::new(note).text_size(px(12.0)).text_color(t.yellow))
            }));

        col = col.child(self.timeline(&tl, cx));
        if show_queue {
            col = col.child(self.queue(cx));
        }
        col.child(self.composer(cx))
    }

    fn settings_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let active_tab: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        let tab_names = ["Connection", "Appearance", "Workspaces", "Behavior", "Advanced"];

        div().flex().flex_col().flex_1().overflow_hidden()
            .child(div().flex().items_center().justify_between().px_5().pt_4().pb_3().border_b_1().border_color(t.border.clone())
                .child(Label::new("Settings").text_size(px(20.0)).font_weight(FontWeight::BOLD))
                .child(Button::new("settings-close").label("✕").ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.settings_open = false; cx.notify(); }))))
            .child(div().flex().flex_1().gap_4().p_5()
                // Vertical tabs on the left
                .child(div().w(px(144.0)).flex().flex_col().gap_1().pr_3().border_r_1().border_color(t.border.clone())
                    .children(tab_names.iter().enumerate().map(|(i, name)| {
                        let active_tab = active_tab.clone();
                        let is_active = active_tab.get() == i;
                        Button::new(format!("tab-{}", i))
                            .label(*name)
                            .ghost()
                            .small()
                            .when(is_active, |b| b.bg(t.muted.clone()))
                            .on_click(move |_, window, _cx| {
                                active_tab.set(i);
                                window.refresh();
                            })
                    })))
                // Content area on the right
                .child(div().flex_1().overflow_y_scrollbar()
                    .children({
                        let tab = active_tab.get();
                        match tab {
                            0 => vec![
                                // Connection section
                                div().mb_4()
                                    .child(Label::new("Connection").font_weight(FontWeight::BOLD).text_size(px(15.0)))
                                    .child(div().mt_2()
                                        .child(Label::new("Backend Server URL").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                        .child(div().mt_1().child(Input::new(&self.username_input)))
                                        .child(Label::new("Desktop backend server. Default: http://127.0.0.1:8080")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_1())
                                    )
                                    .child(div().mt_3()
                                        .child(Label::new("Engine URL").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                        .child(div().mt_1().child(Label::new("http://127.0.0.1:8184").text_size(px(12.0)).font_family("monospace")))
                                        .child(Label::new("Fennec inference engine.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_1())
                                    )
                                    .into_any_element(),
                            ],
                            1 => vec![
                                // Appearance section
                                div().mb_4().flex().flex_col().gap_3()
                                    .child(Label::new("Appearance").font_weight(FontWeight::BOLD).text_size(px(15.0)))
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(Label::new("Theme").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                        .child(div().mt_1().gap_1().flex().flex_col()
                                            .children(theme_presets().into_iter().enumerate().map(|(i, preset)| {
                                                let preset_name = preset.name;
                                                let preset_id = preset.id().to_string();
                                                Button::new(format!("theme-{}", i))
                                                    .label(preset_name)
                                                    .ghost()
                                                    .small()
                                                    .w_full()
                                                    .justify_start()
                                                    .on_click(cx.listener(move |a, _, window, cx| {
                                                        preset.apply(cx);
                                                        a.theme = preset_id.clone();
                                                        a.save_settings();
                                                        window.refresh();
                                                        cx.notify();
                                                    }))
                                            })))
                                        .child(Label::new("Select a color theme. Changes apply immediately.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_1())
                                    )
                                    .into_any_element(),
                            ],
                            2 => vec![
                                // Workspaces section
                                {
                                    let workspaces = self.workspace_items.clone();
                                    let active_id = self.active_workspace_id.clone();
                                    div().mb_4().flex().flex_col().gap_3()
                                        .child(Label::new("Workspaces").font_weight(FontWeight::BOLD).text_size(px(15.0)))
                                        .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                            .child(Label::new("Manage your workspaces").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                            .child(div().mt_2().flex().flex_col().gap_2()
                                                .children(workspaces.iter().map(|ws| {
                                                    let ws_id = ws.id.clone();
                                                    let ws_id2 = ws_id.clone();
                                                    let ws_name = ws.name.clone();
                                                    let is_active = active_id.as_deref() == Some(&ws_id);
                                                    div().flex().items_center().justify_between().p_2().rounded_md()
                                                        .when(is_active, |d| d.bg(t.border.clone()))
                                                        .child(div().flex().items_center().gap_2()
                                                            .child(Label::new(ws_name.clone()).text_size(px(12.0)).font_weight(FontWeight::MEDIUM))
                                                            .child(Label::new(format!("{} folders", ws.source_folders.len())).text_size(px(10.0)).text_color(t.muted_foreground.clone())))
                                                        .child(div().flex().items_center().gap_1()
                                                            .when(!is_active, |d| d.child(
                                                                Button::new(format!("ws-select-{}", ws_id)).label("Activate").ghost().small()
                                                                    .on_click(cx.listener(move |a, _, _, cx| {
                                                                        a.select_workspace(&ws_id);
                                                                        cx.notify();
                                                                    }))
                                                            ))
                                                            .child(Button::new(format!("ws-delete-{}", ws_id2)).label("×").w(px(20.0)).h(px(20.0)).rounded_sm()
                                                                .on_click(cx.listener(move |a, _, _, cx| {
                                                                    a.delete_workspace(&ws_id2);
                                                                    cx.notify();
                                                                })))
                                                        )
                                                }))
                                            )
                                            .child(div().mt_3().flex().items_center().gap_2()
                                                .child(Button::new("ws-add").label("+ Add Workspace").small()
                                                    .on_click(cx.listener(|_a, _, window, cx| {
                                                        let weak = cx.entity().downgrade();
                                                        open_workspace_dialog(window, cx, move |name, folders, icon, color, indexed, cx| {
                                                            let _ = weak.update(cx, |a, cx| {
                                                                a.add_workspace(name, folders, icon, color, indexed);
                                                                cx.notify();
                                                            });
                                                        });
                                                    })))
                                            )
                                            .child(Label::new("Workspaces organize your projects. Activate a workspace to switch context.")
                                                .text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_2())
                                        )
                                        .into_any_element()
                                },
                            ],
                            3 => vec![
                                // Behavior section
                                div().mb_4().flex().flex_col().gap_3()
                                    .child(Label::new("Behavior").font_weight(FontWeight::BOLD).text_size(px(15.0)))
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(div().flex().items_center().justify_between()
                                            .child(Label::new("Auto-save").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                            .child(Label::new(if self.auto_save { "✓ Enabled" } else { "✗ Disabled" })
                                                .text_size(px(11.0)).text_color(if self.auto_save { t.green } else { t.muted_foreground })))
                                        .child(Label::new("Automatically save task history and settings.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()))
                                    )
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(div().flex().items_center().justify_between()
                                            .child(Label::new("Notifications").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                            .child(Label::new(if self.notifications { "✓ Enabled" } else { "✗ Disabled" })
                                                .text_size(px(11.0)).text_color(if self.notifications { t.green } else { t.muted_foreground })))
                                        .child(Label::new("Show desktop notifications for task completion.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()))
                                    )
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(Label::new("Clear History").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                        .child(Button::new("clear-history-btn").label("Clear all task history").ghost().small().mt_1()
                                            .on_click(cx.listener(|a, _, window, cx| {
                                                a.tasks.clear();
                                                a.timelines.clear();
                                                a.seen_at.clear();
                                                a.selected.clear();
                                                a.save_history();
                                                window.push_notification(gpui_kit::component::notification::Notification::info("History cleared."), cx);
                                                cx.notify();
                                            })))
                                        .child(Label::new("Remove all tasks and conversations.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_1())
                                    )
                                    .into_any_element(),
                            ],
                            4 => vec![
                                // Advanced section
                                div().mb_4().flex().flex_col().gap_3()
                                    .child(Label::new("Advanced").font_weight(FontWeight::BOLD).text_size(px(15.0)))
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(div().flex().items_center().justify_between()
                                            .child(Label::new("Debug Mode").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                            .child(Label::new(if self.debug_mode { "✓ Enabled" } else { "✗ Disabled" })
                                                .text_size(px(11.0)).text_color(if self.debug_mode { t.green } else { t.muted_foreground })))
                                        .child(Label::new("Enable verbose logging and debug information.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()))
                                    )
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(div().flex().items_center().justify_between()
                                            .child(Label::new("Experimental Features").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                            .child(Label::new(if self.experimental_features { "✓ Enabled" } else { "✗ Disabled" })
                                                .text_size(px(11.0)).text_color(if self.experimental_features { t.green } else { t.muted_foreground })))
                                        .child(Label::new("Enable unstable features.")
                                            .text_size(px(11.0)).text_color(t.muted_foreground.clone()))
                                    )
                                    .child(div().flex().flex_col().gap_2().p_3().rounded_md().bg(t.muted).border_1().border_color(t.border)
                                        .child(Label::new("Version").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                                        .child(Label::new("1.0.0").text_size(px(11.0)).text_color(t.muted_foreground.clone()).mt_1().font_family("monospace"))
                                    )
                                    .into_any_element(),
                            ],
                            _ => vec![],
                        }
                    })))
    }

    fn timeline(&self, tl: &Timeline, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let timeline_id = self.selected.clone();
        let empty = tl.messages.is_empty() && tl.tools.is_empty() && tl.summary.is_empty();
        if self.follow_tail.replace(false) {
            self.timeline_scroll.scroll_to_bottom();
        }
        let weak = cx.entity().downgrade();

        let mut list = div().id("timeline").track_scroll(&self.timeline_scroll).flex().flex_col().gap_4().flex_1().overflow_y_scrollbar().px_5().py_3();
        if empty {
            list = list.child(Label::new("Nothing has happened in this task yet. The timeline fills as the loop reads, edits, runs and tests.")
                .text_size(px(13.0)).text_color(t.muted_foreground));
        }
        // Walked by index so consecutive reasoning chunks coalesce into one
        // Thinking block, the way a single model turn actually reads.
        let mut midx = 0usize;
        while midx < tl.messages.len() {
            let m = &tl.messages[midx];
            if m.role == Role::User {
                // The user's own words sit on the right in a bubble: no avatar,
                // no header, just the message.
                list = list.child(user_bubble(&t, &m.text, midx));
                midx += 1;
                continue;
            }
            if m.role == Role::Progress {
                let start = midx;
                let mut end = midx;
                while end < tl.messages.len() && tl.messages[end].role == Role::Progress {
                    end += 1;
                }
                // Thoughts are collapsed by default; click the header to expand.
                let first = &tl.messages[start];
                let key = format!("{}:{}", timeline_id, start);
                let open = self.open_thoughts.contains(&key);
                let time = first.time.clone();
                let chunks: Vec<String> = tl.messages[start..end].iter().map(|p| p.text.clone()).collect();
                let count = chunks.len();
                let key2 = key.clone();
                let weak2 = weak.clone();
                let header = ai_header(&t, start as u64, &time, None::<fn(&mut Window, &mut gpui::App)>)
                    .child(Label::new("Thinking").italic().text_size(px(10.5)).text_color(t.success))
                    .child(Label::new(format!("{} chunk{}", count, if count == 1 { "" } else { "s" })).text_size(px(10.5)).text_color(t.muted_foreground))
                    .child(Label::new(if open { "▾" } else { "▸" }).text_size(px(11.0)).text_color(t.muted_foreground))
                    .id(ElementId::Name(format!("think-h-{start}").into()))
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        let _ = weak2.update(cx, |a, cx| {
                            if a.open_thoughts.contains(&key2) { a.open_thoughts.remove(&key2); }
                            else { a.open_thoughts.insert(key2.clone()); }
                            cx.notify();
                        });
                    });
                let mut block = div().id(ElementId::NamedInteger("msg".into(), start as u64))
                    .flex().flex_col().gap_1()
                    .child(header);
                if open {
                    let mut body = div().flex().flex_col().gap_1();
                    for (i, r) in chunks.iter().enumerate() {
                        body = body.child(rich_div(&t, r, true, start + i));
                    }
                    block = block.child(body);
                }
                list = list.child(block);
                midx = end;
                continue;
            }
            // The assistant's finished answer: plain text under a small header.
            let speak_text = m.text.clone();
            let header = ai_header(&t, midx as u64, &m.time, Some(move |window: &mut Window, cx: &mut gpui::App| {
                speak_tts(&speak_text, window, cx);
            }));
            list = list.child(div().id(ElementId::NamedInteger("msg".into(), midx as u64))
                .flex().flex_col().gap_1()
                .child(header)
                .child(rich_div(&t, &m.text, false, midx)));
            midx += 1;
        }
        if tl.streaming && !tl.streaming_text.is_empty() {
            let header = ai_header(&t, tl.messages.len() as u64, "", None::<fn(&mut Window, &mut gpui::App)>)
                .child(Label::new("Thinking").italic().text_size(px(10.5)).text_color(t.success));
            list = list.child(div().flex().flex_col().gap_1()
                .child(header)
                .child(rich_div(&t, &tl.streaming_text, false, tl.messages.len())));
        }
        if !tl.plan.is_empty() {
            let mut plan = div().rounded_md().border_1().border_color(t.border).bg(t.muted).p_3().flex().flex_col().gap_1()
                .child(Label::new("Plan").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)).text_color(t.muted_foreground));
            for (pi, p) in tl.plan.iter().enumerate() {
                plan = plan.child(div().id(ElementId::NamedInteger("plan".into(), pi as u64)).flex().items_center().gap_2()
                    .child(Label::new(if p.done { "✓" } else { "→" }).text_size(px(12.0)).text_color(if p.done { t.success } else { t.muted_foreground }))
                    .child(Label::new(p.content.clone()).text_size(px(12.0)).text_color(if p.done { t.success } else { t.muted_foreground })));
            }
            list = list.child(plan);
        }
        // A gated call the loop is stopped on. Above the tool cards, because
        // this is the one thing here that cannot move until the user does.
        for card in &tl.approvals {
            let live = card.state == ApprovalState::Pending;
            let mut card_div = div().id(ElementId::Name(format!("appr-{}", card.id).into()))
                .rounded_md().border_1().border_color(t.border).bg(t.muted).px_3().py_2().flex().flex_col().gap_1();
            card_div = card_div
                .child(div().flex().items_center().justify_between()
                    .child(div().flex().items_center().gap_2()
                        .child(div().px_1().rounded_md().bg(risk_color(&t, &card.risk))
                            .child(Label::new(card.category.clone()).font_family("monospace").text_size(px(10.0)).text_color(t.background)))
                        .child(Label::new(card.title.clone()).text_size(px(12.5))))
                    .child(Label::new(format!("{} risk", card.risk)).text_size(px(11.0)).text_color(risk_color(&t, &card.risk))));
            if !card.reason.is_empty() {
                card_div = card_div.child(Label::new(card.reason.clone()).text_size(px(11.5)).text_color(t.muted_foreground));
            }
            if let Some(path) = card.path.as_deref() {
                card_div = card_div.child(Label::new(format!("file · {path}")).font_family("monospace").text_size(px(11.0)).text_color(t.muted_foreground));
            }
            card_div = card_div.child(Label::new(card.detail.clone()).font_family("monospace").text_size(px(11.0)).text_color(t.muted_foreground));
            let mut buttons = div().flex().flex_row().flex_wrap().gap_2().mt_1();
            for decision in events::APPROVAL_DECISIONS {
                let approval_id = card.id.clone();
                buttons = buttons.child(
                    Button::new(ElementId::Name(format!("appr-{}-{}", card.id, decision).into()))
                        .label(events::decision_label(decision)).ghost().small()
                        .when(!live, |b| b.disabled(true))
                        .on_click(cx.listener(move |a, _, _, cx| { a.answer(&approval_id, decision); cx.notify(); }))
                );
            }
            card_div = card_div.child(buttons);
            card_div = card_div.child(match card.state {
                ApprovalState::Pending => Label::new("the run waits on this answer").text_size(px(10.5)).text_color(t.yellow),
                ApprovalState::Answered => Label::new(format!("answer sent · {}", events::decision_label(&card.decision))).text_size(px(10.5)).text_color(t.success),
                ApprovalState::Expired => Label::new(format!("not used · {}", card.note)).text_size(px(10.5)).text_color(t.red),
            });
            list = list.child(card_div);
        }
        for (ci, card) in tl.tools.iter().enumerate() {
            let open = self.expanded.contains(&card.id);
            let is_edit = card.category == "EDIT";
            let (adds, dels) = diff_counts(&format!("{}\n{}", card.detail, card.output));
            let key = card.id.clone();
            let weak2 = weak.clone();
            let on_toggle = move |_: &mut Window, cx: &mut gpui::App| {
                let _ = weak2.update(cx, |a, cx| {
                    if a.expanded.contains(&key) { a.expanded.remove(&key); }
                    else { a.expanded.insert(key.clone()); }
                    cx.notify();
                });
            };
            if is_edit {
                // A file the run produced is shown like an attachment above the
                // change that made it; clicking either reviews the same diff.
                if let Some(p) = card.path.as_deref() {
                    let file = std::path::Path::new(p);
                    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                    let dir = file.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
                    let kind = artifact_kind(&name);
                    let review_path = p.to_string();
                    let weak3 = weak.clone();
                    list = list.child(attachment_card(&t, ci as u64, &name, &format!("{dir} · {kind}"), "…",
                        Some(move |window: &mut Window, cx: &mut gpui::App| {
                            let _ = weak3.update(cx, |a, cx| { a.review_edit(review_path.clone(), window, cx); cx.notify(); });
                        })));
                }
                let title = if card.title.starts_with("Write") { "Created 1 file" } else { "Edited 1 file" };
                let detail = if card.output.is_empty() { card.detail.clone() } else { card.output.clone() };
                let review_path = card.path.clone().unwrap_or_default();
                let weak3 = weak.clone();
                let on_review = move |window: &mut Window, cx: &mut gpui::App| {
                    let _ = weak3.update(cx, |a, cx| { a.review_edit(review_path.clone(), window, cx); cx.notify(); });
                };
                list = list.child(edit_card(&t, ci as u64, title, adds, dels,
                    card.path.as_deref().unwrap_or(""), &detail, open, on_toggle, on_review));
                continue;
            }
            let mut status_text = card.status.label().to_string();
            if !card.duration.is_empty() {
                status_text.push_str(&format!(" · {}", card.duration));
            }
            if let Some(code) = card.exit {
                status_text.push_str(&format!(" · exit {code}"));
            }
            let detail = match (card.detail.is_empty(), card.output.is_empty()) {
                (true, true) => String::new(),
                (false, true) => card.detail.clone(),
                (true, false) => card.output.clone(),
                (false, false) => format!("{}\n{}", card.detail, card.output),
            };
            list = list.child(action_card(&t, ci as u64, tool_icon(&card.category), &card.title,
                &detail, &status_text, tool_color(&t, card.status), open, on_toggle));
        }
        // The run's final text, shown once as an assistant turn. The old green
        // "Summary" card duplicated the answer; when the backend sent only a
        // `final` step, this is where that text lives.
        let last_assistant = tl.messages.iter().rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| m.text.trim().to_string());
        if !tl.summary.is_empty() && last_assistant.as_deref() != Some(tl.summary.trim()) {
            let sidx = tl.messages.len() + 1;
            let speak_text = tl.summary.clone();
            let header = ai_header(&t, sidx as u64, "", Some(move |window: &mut Window, cx: &mut gpui::App| {
                speak_tts(&speak_text, window, cx);
            }));
            list = list.child(div().id(ElementId::Name("summary-turn".into()))
                .flex().flex_col().gap_1()
                .child(header)
                .child(rich_div(&t, &tl.summary, false, sidx)));
        }
        list
    }    fn queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if self.task_queue.is_empty() {
            return div().into_any_element();
        }
        let items: Vec<String> = self.task_queue.iter().map(|q| q.text.clone()).collect();
        let weak = cx.entity().downgrade();
        queue_strip(&t, &items, move |window, cx| {
            if let Some(app) = weak.upgrade() {
                let n = app.read(cx).task_queue.len();
                window.push_notification(gpui_kit::component::notification::Notification::info(
                    format!("{n} tasks in queue")
                ), cx);
            }
        }).into_any_element()
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let value = self.composer.read(cx).value().trim().to_string();
        let send_enabled = !value.is_empty();
        let mode = self.permission_mode.clone();

        let mut col = div().border_t_1().border_color(t.border).bg(t.background).px_5().py_3().flex().flex_col().gap_2();
        // Toasts sit right above the input.
        for (ti, toast) in self.toasts.iter().enumerate() {
            col = col.child(div().id(ElementId::NamedInteger("toast".into(), ti as u64)).flex().items_center().gap_2()
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(if toast.good { t.success } else { t.yellow }))
                .child(Label::new(toast.text.clone()).text_size(px(12.0)).text_color(t.foreground)));
        }
        if !self.line.is_empty() {
            col = col.child(Label::new(self.line.clone()).text_size(px(12.0)).text_color(t.yellow));
        }

        // Show attachments above composer if any
        if !self.attachments.is_empty() {
            let mut attach_row = div().flex().flex_wrap().gap_1().px_2().py_1();
            for (i, att) in self.attachments.iter().enumerate() {
                let name = att.name.clone();
                let is_image = att.mime_type.starts_with("image/");
                attach_row = attach_row.child(
                    div().flex().items_center().gap_1().px_2().py_1().rounded_md().bg(t.muted)
                        .child(Label::new(if is_image { "🖼" } else { "📄" }).text_size(px(12.0)))
                        .child(Label::new(name).text_size(px(11.0)).truncate())
                        .child(Button::new(format!("att-remove-{}", i)).label("×").ghost().small().w(px(22.0)).h(px(22.0)).text_color(t.muted_foreground).text_color(t.muted_foreground)
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.remove_attachment(i, cx);
                            })))
                );
            }
            col = col.child(attach_row);
        }

        col = col
            .child(div().id("composer-dock").rounded_md().border_1().border_color(t.border).p_3()
                .child(Label::new("Ask Fennec to inspect, edit, test, or explain your code").text_size(px(13.0)).text_color(t.muted_foreground))
                .child(div().mt_2().child(Input::new(&self.composer))))
            .child(div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap_1()
                    .child(Button::new("composer-new-task").label("+").ghost().small()
                        .on_click(cx.listener(|a, _, _, cx| { a.new_draft(); cx.notify(); })))
                    .child(Button::new("composer-attach").ghost().small()
                        .child(Label::new("📎").text_size(px(15.0)))
                        .on_click(cx.listener(|a, _, window, cx| {
                            a.attach_file(window, cx);
                        })))
                    .child(Button::new("permission-trigger").ghost().small()
                        .child(Label::new(format!("{} {}", mode.icon(), mode.label())).text_size(px(12.0)).text_color(mode.color(&t)))
                        .on_click(cx.listener(|a, _, _, cx| {
                            a.permission_dropdown_open = !a.permission_dropdown_open;
                            a.model_dropdown_open = false;
                            cx.notify();
                        }))))
                .child(div().flex().items_center().gap_2()
                    .child(Button::new("model-trigger").label(format!("⚡ {}", self.model_display_name())).ghost().small()
                        .on_click(cx.listener(|a, _, _, cx| {
                            a.model_dropdown_open = !a.model_dropdown_open;
                            a.permission_dropdown_open = false;
                            cx.notify();
                        })))
                    .child(Button::new("composer-mic").ghost().small()
                        .child(Label::new(if self.stt_recording { "⏹" } else { "🎤" }).text_size(px(15.0)))
                        .on_click(cx.listener(|a, _, window, cx| {
                            a.start_stt(window, cx);
                        })))
                    .child(Button::new("send").label("↑").primary().small()
                        .when(!send_enabled, |b| b.disabled(true))
                        .on_click(cx.listener(|a, _, window, cx| { a.send(window, cx); })))));
        col
    }

    /// Dropdowns rendered at the window root so they are never clipped by the
    /// centre pane's overflow_hidden. Permission panel is tall enough to show
    /// every description in full.
    fn dropdowns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let mut overlay = div();
        if self.permission_dropdown_open {
            overlay = overlay.child(
                div().absolute().bottom(px(70.0)).left(px(20.0))
                    .rounded_md().border_1().border_color(t.border).bg(t.background).shadow_lg()
                    .p_2().w(px(460.0)).flex().flex_col().gap_1()
                    .child(self.permission_option("ask", "🛡", "Ask every time", "Always ask before commands, external edits, and internet use", cx))
                    .child(self.permission_option("auto", "🕐", "Auto approve", "Only asks when a potential risk is detected.", cx))
                    .child(self.permission_option_danger("full", "⚠", "Full access", "Never ask, with full access to your files, terminal, and the internet", cx))
            );
        }
        if self.model_dropdown_open {
            overlay = overlay.child(
                div().absolute().bottom(px(70.0)).right(px(20.0))
                    .rounded_md().border_1().border_color(t.border).bg(t.background).shadow_lg()
                    .p_2().w(px(220.0)).flex().flex_col().gap_1()
                    .child(Button::new("model-qwen35").ghost().small().w_full().justify_start()
                        .child(div().flex().items_center().gap_3().px_3().py_2()
                            .child(Label::new("Qwen3.5-MoE").text_size(px(13.0)).font_weight(FontWeight::MEDIUM))
                            .child(div().flex_1())
                            .child(Label::new(if self.model == "qwen3_5_moe" { "✓" } else { "" }).text_size(px(13.0)).text_color(t.success)))
                        .on_click(cx.listener(|a, _, _, cx| {
                            a.model = "qwen3_5_moe".to_string();
                            a.model_dropdown_open = false;
                            a.save_settings();
                            cx.notify();
                        })))
                    .child(Button::new("model-edge0").ghost().small().w_full().justify_start()
                        .child(div().flex().items_center().gap_3().px_3().py_2()
                            .child(Label::new("Edge0-145B").text_size(px(13.0)).font_weight(FontWeight::MEDIUM))
                            .child(div().flex_1())
                            .child(Label::new(if self.model == "edge0_145b" { "✓" } else { "" }).text_size(px(13.0)).text_color(t.success)))
                        .on_click(cx.listener(|a, _, _, cx| {
                            a.model = "edge0_145b".to_string();
                            a.model_dropdown_open = false;
                            a.save_settings();
                            cx.notify();
                        })))
                    .child(Button::new("model-qwen36").ghost().small().w_full().justify_start()
                        .child(div().flex().items_center().gap_3().px_3().py_2()
                            .child(Label::new("Qwen36-35B").text_size(px(13.0)).font_weight(FontWeight::MEDIUM))
                            .child(div().flex_1())
                            .child(Label::new(if self.model == "qwen36_35b" { "✓" } else { "" }).text_size(px(13.0)).text_color(t.success)))
                        .on_click(cx.listener(|a, _, _, cx| {
                            a.model = "qwen36_35b".to_string();
                            a.model_dropdown_open = false;
                            a.save_settings();
                            cx.notify();
                        })))
                    .child(div().border_t_1().border_color(t.border).my_1())
                    .child(Button::new("model-download").ghost().small().w_full().justify_start()
                        .child(div().flex().items_center().gap_3().px_3().py_2()
                            .child(Label::new("⬇").text_size(px(15.0)))
                            .child(Label::new("Download new model...").text_size(px(12.0)).text_color(t.muted_foreground)))
                        .on_click(cx.listener(|a, _, window, cx| {
                            window.push_notification(gpui_kit::component::notification::Notification::info("Model download coming soon."), cx);
                            a.model_dropdown_open = false;
                            cx.notify();
                        })))
            );
        }
        overlay
    }

    fn permission_option(&self, mode_id: &str, icon: &str, title: &str, desc: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let current_mode = self.permission_mode.clone();
        let is_selected = match mode_id {
            "ask" => current_mode == PermissionMode::AskEveryTime,
            "auto" => current_mode == PermissionMode::AutoApprove,
            "full" => current_mode == PermissionMode::FullAccess,
            _ => false,
        };
        let mode_id = mode_id.to_string();
        Button::new(format!("perm-opt-{}", mode_id)).ghost().w_full().justify_start()
            .on_click(cx.listener(move |a, _, _, cx| {
                a.permission_mode = match mode_id.as_str() {
                    "ask" => PermissionMode::AskEveryTime,
                    "auto" => PermissionMode::AutoApprove,
                    "full" => PermissionMode::FullAccess,
                    _ => PermissionMode::AskEveryTime,
                };
                a.permission_dropdown_open = false;
                cx.notify();
            }))
            .child(div().flex().items_start().gap_3().px_3().py_3().w_full()
                .child(Label::new(icon).text_size(px(18.0)).mt(px(2.0)))
                .child(div().flex().flex_col().flex_1().gap_0p5()
                    .child(Label::new(title).text_size(px(13.0)).font_weight(FontWeight::MEDIUM))
                    .child(Label::new(desc).text_size(px(11.0)).text_color(t.muted_foreground)))
                .child(Label::new(if is_selected { "✓" } else { "" }).text_size(px(13.0)).text_color(t.success).mt(px(2.0))))
    }

    fn permission_option_danger(&self, mode_id: &str, icon: &str, title: &str, desc: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let current_mode = self.permission_mode.clone();
        let is_selected = current_mode == PermissionMode::FullAccess;
        let mode_id = mode_id.to_string();
        Button::new(format!("perm-opt-{}", mode_id)).ghost().w_full().justify_start()
            .on_click(cx.listener(move |a, _, _, cx| {
                a.permission_mode = PermissionMode::FullAccess;
                a.permission_dropdown_open = false;
                cx.notify();
            }))
            .child(div().flex().items_start().gap_3().px_3().py_3().w_full()
                .child(Label::new(icon).text_size(px(18.0)).mt(px(2.0)))
                .child(div().flex().flex_col().flex_1().gap_0p5()
                    .child(Label::new(title).text_size(px(13.0)).font_weight(FontWeight::MEDIUM).text_color(t.red))
                    .child(Label::new(desc).text_size(px(11.0)).text_color(t.muted_foreground)))
                .child(Label::new(if is_selected { "✓" } else { "" }).text_size(px(13.0)).text_color(t.success).mt(px(2.0))))
    }

    fn right(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let tl = self.timelines.get(&self.selected).cloned().unwrap_or_default();
        let working = self.busy || self.tl_status(&self.selected) == TaskStatus::Running;
        let task_title = self.tasks.iter().find(|t| t.id == self.selected)
            .map(|t| t.title.clone()).unwrap_or_else(|| "Idle".into());
        let peers = tl.tools.iter().filter(|c| c.category == "PEER" || c.category == "WEB").count();
        let web = tl.tools.iter().filter(|c| c.category == "WEB").count();
        let procs = tl.tools.iter().filter(|c| c.category == "CMD").count();
        let sources = tl.tools.iter().filter(|c| c.category == "READ" || c.category == "LS").count();
        let skill_kinds: std::collections::HashSet<&str> = tl.tools.iter().map(|c| c.category.as_str()).collect();
        let arts = tl.artifacts();
        let ws = self.workspace_name();
        let branch_display = if self.env.branch.is_empty() { "main".to_string() } else { self.env.branch.clone() };
        let backend_text = if self.env.core_up && self.env.engine_up {
            "Connected"
        } else if self.env.core_up || self.env.engine_up {
            "Partial"
        } else {
            "Offline"
        };

        let panel = div().w(px(300.0)).flex_shrink_0().bg(t.background).border_l_1().border_color(t.border)
            .flex().flex_col().overflow_y_scrollbar()
            // Inspector header
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().items_center().justify_between()
                .child(div().flex().flex_col()
                    .child(Label::new("Workspace context").font_weight(FontWeight::SEMIBOLD).text_size(px(13.0)))
                    .child(Label::new("Inspector").text_size(px(11.0)).text_color(t.muted_foreground)))
                .child(Button::new("collapse-inspector").label("⊟").ghost().xsmall()
                    .on_click(cx.listener(|a, _, _, cx| { a.right_open = false; cx.notify(); }))))
            // ACTIVE AGENTS section
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().flex_col().gap_2()
                .child(div().flex().items_center().justify_between()
                    .child(Label::new("ACTIVE AGENTS").text_size(px(10.0)).text_color(t.muted_foreground))
                    .child(Label::new(format!("{} online", if working { 1 } else { 0 })).text_size(px(11.0))
                        .text_color(if working { t.success } else { t.muted_foreground })))
                .children(working.then(|| {
                    div().p_2().rounded_md().border_1().border_color(t.border).flex().items_center().gap_2()
                        .child(Label::new("🤖").text_size(px(16.0)))
                        .child(div().flex().flex_col().overflow_hidden()
                            .child(Label::new("Fennec").font_weight(FontWeight::MEDIUM).text_size(px(12.0)))
                            .child(Label::new(task_title.clone()).text_size(px(11.0)).text_color(t.muted_foreground).truncate())
                            .child(Label::new(if self.busy { "Running".to_string() } else { self.tl_status(&self.selected).label().to_string() })
                                .text_size(px(10.5)).text_color(if self.busy { t.primary } else { status_color(&t, self.tl_status(&self.selected)) })))
                })))
            // WORKSPACE CONTEXT section
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().flex_col().gap_2()
                .child(Label::new("WORKSPACE CONTEXT").text_size(px(10.0)).text_color(t.muted_foreground))
                .child(self.ctx_row("subagents", "👥", "Subagents", if peers > 0 {
                    format!("{peers} delegation card(s)")
                } else {
                    "No active delegated agents".to_string()
                }, cx))
                .child(self.ctx_row("processes", "📡", "Processes", if procs > 0 {
                    format!("{procs} command(s) run")
                } else if self.env.core_up {
                    "Event stream connected".to_string()
                } else {
                    "Agent loop offline".to_string()
                }, cx))
                .child(self.ctx_row("skills", "⚡", "Skills", if skill_kinds.is_empty() {
                    "Repository inspection".to_string()
                } else {
                    format!("{} tool kind(s) exercised", skill_kinds.len())
                }, cx))
                .child(self.ctx_row("artifacts", "📦", "Artifacts", if arts.is_empty() {
                    "No task artifacts".to_string()
                } else {
                    format!("{} task artifact(s)", arts.len())
                }, cx))
                .child(self.ctx_row("web", "🌐", "Web Readings", if web > 0 {
                    format!("{web} source(s) fetched")
                } else {
                    "No linked readings".to_string()
                }, cx))
                .child(self.ctx_row("sources", "🔗", "Sources", if sources > 0 {
                    format!("{sources} file(s) inspected")
                } else {
                    "No task sources".to_string()
                }, cx)))
            // ENVIRONMENT section
            .child(div().px_4().py_3().flex().flex_col().gap_1()
                .child(Label::new("ENVIRONMENT").text_size(px(10.0)).text_color(t.muted_foreground))
                .child(self.row("env-repo", "Repository", ws, cx))
                .child(self.row("env-branch", "Branch", branch_display, cx))
                .child(self.row("env-backend", "Backend", backend_text.to_string(), cx)));
        panel
    }

    fn ctx_row(&self, id: &'static str, icon: &str, label: &str, value: String, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        div().id(ElementId::Name(id.to_string().into())).flex().items_center().gap_2()
            .child(Label::new(icon).text_size(px(14.0)))
            .child(div().flex().flex_col().overflow_hidden()
                .child(Label::new(label).font_weight(FontWeight::MEDIUM).text_size(px(11.5)))
                .child(Label::new(value).text_size(px(10.5)).text_color(t.muted_foreground).truncate()))
    }

    fn row(&self, id: &'static str, label: &str, value: String, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        div().id(ElementId::Name(id.to_string().into())).flex().items_center().justify_between().py_1()
            .child(Label::new(label).text_size(px(11.0)).text_color(t.muted_foreground))
            .child(Label::new(value).font_family("monospace").text_size(px(11.0)).truncate())
    }

    fn row_colored(&self, id: ElementId, label: &str, value: &str, color: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        div().id(id).flex().items_center().justify_between().py_1()
            .child(Label::new(label).text_size(px(11.0)).text_color(t.muted_foreground))
            .child(div().flex().items_center().gap_1()
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(color))
                .child(Label::new(value.to_string()).text_size(px(11.0)).text_color(color).truncate()))
    }
}

// ---- Small helpers (kept identical to the egui shell) ----

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

fn main() {
    std::panic::set_hook(Box::new(|info| {
        use std::io::Write as _;
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let line = format!(
            "=== panic at {:?} ===\n{}\nat {}\n",
            std::time::SystemTime::now(),
            payload,
            location
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(r"C:\Users\Liso1\.qoder\tmp\fennec-gui-panic.log")
        {
            let _ = f.write_all(line.as_bytes());
        }
    }));
    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        let saved = load_settings();
        let theme_id = saved.theme.as_deref().unwrap_or("dark");
        let idx = find_preset_index(theme_id);
        theme_presets()[idx].apply(cx);
        let bounds = Bounds::centered(None, size(px(1500.0), px(960.0)), cx);
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), titlebar: Some(TitlebarOptions { title: Some("Fennec".into()), ..Default::default() }), ..Default::default() }, |window, cx| {
            let composer = cx.new(|cx| InputState::new(window, cx));
            let search = cx.new(|cx| InputState::new(window, cx));
            let username_input = cx.new(|cx| InputState::new(window, cx));
            let view = cx.new(|cx| {
                let mut app = App::new(composer, search, username_input.clone(), window, cx);
                app.load_history();
                if app.selected.is_empty() {
                    app.selected = app.tasks.first().map(|t| t.id.clone()).unwrap_or_default();
                }
                // Set initial username in the input
                username_input.update(cx, |state, cx| state.set_value("User".to_string(), window, cx));
                app
            });
            cx.new(|cx| Root::new(view, window, cx))
        }).expect("Failed to open window");
    });
}
