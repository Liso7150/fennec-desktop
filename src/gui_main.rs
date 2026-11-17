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
use gpui_kit::base::Root;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement,
    Theme, ThemeMode,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

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
    /// Sessions whose steps this window has already asked the service for, so
    /// a task selected after a restart is fetched once rather than every frame.
    replayed: HashSet<String>,
    composer: Entity<InputState>,
    search: Entity<InputState>,
    username_input: Entity<InputState>,
    allow_write: bool,
    allow_shell: bool,
    allow_web: bool,
    /// `require_approval`: the switches stop admitting calls and every gated
    /// one is asked about instead. The answer, not a box ticked at start, is
    /// what lets a write through.
    ask_each: bool,
    attention_only: bool,
    right_open: bool,
    search_open: bool,
    settings_open: bool,
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

        Self {
            suite: s, workspaces, ws: 0, core, engine, env: Env::default(),
            tasks: Vec::new(), seen_at: HashMap::new(), selected: String::new(),
            timelines: HashMap::new(), replayed: HashSet::new(), composer, search, username_input,
            allow_write: false, allow_shell: false, allow_web: false, ask_each: false,
            attention_only: false, right_open: true, search_open: false, settings_open: false,
            username: "User".to_string(),
            expanded: HashSet::new(), open_thoughts: HashSet::new(),
            toasts: Vec::new(), line: String::new(), pending_send: None,
            composer_restore: None, chat_cancel: None, rx, tx, busy: false,
            task_queue: Vec::new(), timeline_scroll: ScrollHandle::new(), follow_tail: Cell::new(true),
            _task,
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
        });
        self.selected = id.clone();
        self.follow_tail.set(true);
        self.save_history();
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
                    updated: "now".into(), goal: String::new(), draft: true,
                });
                self.selected = id.clone();
                self.follow_tail.set(true);
                id
            };
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
                updated: "now".into(), goal: String::new(), draft: true,
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
        let base = if cfg!(target_os = "windows") {
            std::env::var("LOCALAPPDATA").ok().map(PathBuf::from)
        } else if cfg!(target_os = "macos") {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join("Library/Application Support"))
        } else {
            std::env::var("XDG_DATA_HOME").ok().map(PathBuf::from)
                .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))
        }?;
        Some(base.join("fennec-desktop").join("history.json"))
    }

    fn save_history(&self) {
        let Some(path) = Self::history_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let data = HistoryData {
            tasks: self.tasks.iter().map(|t| HistoryTask {
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
            }).collect(),
            // Only timelines that belong to a task row; a stale key with no
            // task (like the corrupt "" entry) would re-serialize forever.
            timelines: self.timelines.iter()
                .filter(|(id, _)| self.tasks.iter().any(|t| &t.id == *id))
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

// ---- The model's text, laid out ----

/// One block between code fences: prose the Markdown subset covers, or a fence
/// the shell shows as a monospace card.
enum Block {
    Prose(String),
    Code { lang: String, body: String },
}

/// Split ``` fences out of a message the way `render_rich` does in the egui
/// shell: prose outside the fences, and a fence's first line is a language only
/// when it looks like one.
fn rich_blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let before = &rest[..open];
        if !before.trim().is_empty() {
            out.push(Block::Prose(before.trim().to_string()));
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
        out.push(Block::Code { lang, body: code });
        rest = tail;
    }
    if !rest.trim().is_empty() {
        out.push(Block::Prose(rest.trim().to_string()));
    }
    out
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

/// One styled piece of a line.
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

/// Chat text as the model wrote it. ``` fences become monospace cards; the
/// prose between them is read as a Markdown subset.
fn rich_div(t: &Theme, text: &str, dim: bool) -> Div {
    let base = if dim { 12.0 } else { 13.5 };
    let mut col = div().flex().flex_col().gap_2().max_w(px(680.0));
    for (bi, b) in rich_blocks(text).into_iter().enumerate() {
        col = col.child(div().id(ElementId::NamedInteger("blk".into(), bi as u64)).child(match b {
            Block::Prose(p) => md_div(t, &p, base, dim),
            Block::Code { lang, body } => code_block(t, &lang, &body),
        }));
    }
    col
}

/// Prose under the Markdown subset: headings size up, bullets get a marker,
/// inline runs keep their own styles.
fn md_div(t: &Theme, text: &str, base: f32, dim: bool) -> Div {
    let mut col = div().flex().flex_col().gap_1();
    for (li, line) in md_lines(text).into_iter().enumerate() {
        if line.runs.is_empty() {
            continue;
        }
        let size = base + match line.head { 1 => 6.0, 2 => 3.5, 3 => 1.5, _ => 0.0 };
        let body = if line.head > 0 { t.foreground } else if dim { t.muted_foreground } else { t.foreground };
        let mut row = div().id(ElementId::NamedInteger("mdl".into(), li as u64)).flex().flex_row().flex_wrap().items_center();
        if line.bullet {
            row = row.child(div().flex_shrink_0().child(Label::new("•").text_size(px(size)).text_color(body)));
        }
        for (ri, run) in line.runs.iter().enumerate() {
            row = row.child(run_div(t, run, size, body, line.head > 0, ri));
        }
        col = col.child(row);
    }
    col
}

fn run_div(t: &Theme, run: &Run, size: f32, body: Hsla, heading: bool, idx: usize) -> impl IntoElement {
    let id = ElementId::NamedInteger("mdr".into(), idx as u64);
    match run {
        Run::Text(s) => {
            let mut l = Label::new(s.clone()).text_size(px(size)).text_color(body);
            if heading {
                l = l.font_weight(FontWeight::SEMIBOLD);
            }
            div().id(id).child(l)
        }
        Run::Strong(s) => div().id(id).child(
            Label::new(s.clone()).text_size(px(size)).text_color(t.foreground).font_weight(FontWeight::SEMIBOLD)
        ),
        Run::Em(s) => div().id(id).italic().child(Label::new(s.clone()).text_size(px(size)).text_color(body)),
        Run::Code(s) => div().id(id).rounded_md().bg(t.muted).px_1().font_family("monospace").child(
            Label::new(s.clone()).text_size(px((size - 1.0).max(9.0))).text_color(t.foreground)
        ),
        Run::Link { label, url } => div().id(id).flex().flex_row().flex_wrap().items_center()
            .child(Label::new(label.clone()).text_size(px(size)).text_color(t.primary))
            .child(Label::new(format!(" ({url})")).font_family("monospace")
                .text_size(px((size - 1.5).max(9.0))).text_color(t.muted_foreground)),
    }
}

fn code_block(t: &Theme, lang: &str, code: &str) -> Div {
    let mut col = div().flex().flex_col().gap_1().max_w(px(680.0))
        .rounded_md().border_1().border_color(t.border).bg(t.muted).px_3().py_2();
    if !lang.is_empty() {
        col = col.child(div().font_family("monospace")
            .child(Label::new(lang.to_string()).text_size(px(10.0)).text_color(t.muted_foreground)));
    }
    col = col.child(div().font_family("monospace").max_h(px(240.0)).overflow_y_scrollbar()
        .child(Label::new(code.to_string()).text_size(px(12.0)).text_color(t.foreground)));
    col
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

/// What a produced file is called in the artifact card's subtitle.
fn artifact_kind(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "md" => "Markdown attachment",
        "rs" => "Rust source",
        "py" => "Python source",
        "json" | "toml" | "yaml" | "yml" => "Config file",
        "txt" => "Text file",
        "js" | "ts" | "tsx" | "jsx" => "Script",
        "html" | "css" => "Web file",
        _ => "File attachment",
    }
}

/// Diff-shaped line counts (+/−, fence headers excluded) over whatever text a
/// tool card carries; (0, 0) means the payload held no diff to count.
fn diff_counts(text: &str) -> (usize, usize) {
    let (mut adds, mut dels) = (0usize, 0usize);
    for line in text.lines() {
        if line.starts_with('+') && !line.starts_with("+++") { adds += 1; }
        else if line.starts_with('-') && !line.starts_with("---") { dels += 1; }
    }
    (adds, dels)
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
    }
}

impl App {
    fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let (core_up, engine_up) = (self.env.core_up, self.env.engine_up);
        let connected = core_up && engine_up;
        let status_label = if connected { "Backend connected" } else if core_up { "Agent loop only" } else if engine_up { "Engine only" } else { "No backend" };
        let status_color = if connected { t.success } else { t.yellow };
        let user = self.username_input.read(cx).value().to_string();

        div().flex().items_center().justify_between().px_4().py_2().bg(t.background).border_b_1().border_color(t.border)
            .child(div().flex().items_center().gap_3()
                .child(Label::new(">_").font_family("monospace").text_size(px(16.0)).text_color(t.primary))
                .child(div().flex().flex_col()
                    .child(Label::new("Fennec").text_size(px(16.0)).font_weight(FontWeight::BOLD))
                    .child(Label::new(format!("{user} / local-first workspace")).text_size(px(11.0)).text_color(t.muted_foreground))))
            .child(div().flex().items_center().gap_3()
                .child(div().flex().items_center().gap_1()
                    .child(div().w(px(7.0)).h(px(7.0)).rounded_full().bg(status_color))
                    .child(Label::new(status_label).text_size(px(12.0)).text_color(status_color)))
                .child(Button::new("topbar-settings").label("⚙").ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.settings_open = !a.settings_open; cx.notify(); })))
                .child(Button::new("inspector").label(if self.right_open { "◫" } else { "◰" }).ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.right_open = !a.right_open; cx.notify(); }))))
    }

    fn left(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let query = self.search.read(cx).value().trim().to_string();
        let tasks = self.filtered_tasks(&query);
        let attention = self.attention_count();
        let branch_display = if self.env.branch.is_empty() { "main".to_string() } else { self.env.branch.clone() };

        let mut rail = div().w(px(250.0)).flex_shrink_0().bg(t.background).border_r_1().border_color(t.border)
            .flex().flex_col().p_3().gap_3()
            .child(div().p_3().rounded_md().border_1().border_color(t.border)
                .child(Label::new("ACTIVE WORKSPACE").text_size(px(10.0)).text_color(t.muted_foreground))
                .child(Label::new(self.workspace_name()).text_size(px(14.0)).font_weight(FontWeight::SEMIBOLD).truncate())
                .child(Label::new(format!("{branch_display} · Local mode")).text_size(px(11.0)).text_color(t.muted_foreground)))
            .child(div().flex().flex_col().gap_1()
                .child(self.nav_button("nav-coding", "Coding", !self.attention_only && !self.search_open, cx))
                .child(self.nav_button("nav-new", "New Task", false, cx))
                .child(Button::new("nav-attention").label(format!("My Attention{}", if attention > 0 { format!(" · {attention}") } else { String::new() })).ghost().small()
                    .when(self.attention_only, |b| b.bg(t.muted))
                    .on_click(cx.listener(|a, _, _, cx| { a.attention_only = !a.attention_only; cx.notify(); })))
                .child(Button::new("nav-search").label("Search").ghost().small()
                    .when(self.search_open, |b| b.bg(t.muted))
                    .on_click(cx.listener(|a, _, _, cx| { a.search_open = !a.search_open; cx.notify(); }))));
        // gpui-base inputs all render element id "input-state"; this id-bearing
        // parent keeps their a11y node ids distinct when a UIA client connects.
        rail = rail.children(self.search_open.then(|| {
            div().id("search-dock").flex().flex_col().gap_1()
                .child(Input::new(&self.search).h(px(28.0)).text_size(px(12.0)))
                .child(Label::new(if query.is_empty() { String::new() } else { format!("{} tasks", tasks.len()) })
                    .text_size(px(10.0)).text_color(t.muted_foreground))
        }));
        rail = rail.child(div().flex().flex_col().gap_1().overflow_y_scrollbar().flex_1()
            .child(Label::new("RECENT TASKS").text_size(px(10.0)).text_color(t.muted_foreground))
            .children(tasks.iter().take(30).enumerate().map(|(idx, tk)| {
                let t = t.clone();
                let id = tk.id.clone();
                let is_active = self.selected == tk.id;
                div().id(ElementId::NamedInteger("task-item".into(), idx as u64))
                    .p_2().rounded_md().cursor_pointer()
                    .when(is_active, |d| d.bg(t.muted))
                    .hover(|d| d.bg(t.muted))
                    .on_click(cx.listener(move |a, _, _, cx| { a.selected = id.clone(); cx.notify(); }))
                    .child(Label::new(tk.title.clone()).text_size(px(13.0)).font_weight(FontWeight::MEDIUM).truncate())
                    .child(div().flex().items_center().gap_1().mt_1()
                        .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(status_color(&t, tk.status)))
                        .child(Label::new(tk.status.label()).text_size(px(11.0)).text_color(t.muted_foreground))
                        .child(Label::new(if tk.draft { "· draft" } else { "· task" }).text_size(px(11.0)).text_color(t.muted_foreground))
                        .child(Label::new(tk.updated.clone()).text_size(px(11.0)).text_color(t.muted_foreground).truncate()))
            }))
            .children(tasks.is_empty().then(|| Label::new(if query.is_empty() {
                "Nothing here yet — the rail fills when a backend answers."
            } else {
                "No matching task."
            }).text_size(px(11.5)).text_color(t.muted_foreground))))
            .child(div().mt_auto().pt_3().border_t_1().border_color(t.border)
                .child(Label::new(self.username_input.read(cx).value().to_string()).text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD))
                .child(Label::new("Local account").text_size(px(11.0)).text_color(t.muted_foreground)));
        rail
    }

    fn nav_button(&self, id: &'static str, label: &str, active: bool, cx: &mut Context<Self>) -> Button {
        let t = cx.theme().clone();
        let label = label.to_string();
        Button::new(id).label(label.clone()).ghost().small()
            .when(active, |b| b.bg(t.muted))
            .on_click(cx.listener(move |a, _, _, cx| {
                match label.as_str() {
                    "Coding" => { a.attention_only = false; a.search_open = false; }
                    "New Task" => a.new_draft(),
                    _ => {}
                }
                cx.notify();
            }))
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
        let show_queue = self.busy && !self.task_queue.is_empty();

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
        div().flex().flex_col().flex_1().overflow_hidden()
            .child(div().flex().items_center().justify_between().px_5().pt_4().pb_3().border_b_1().border_color(t.border)
                .child(Label::new("Settings").text_size(px(20.0)).font_weight(FontWeight::BOLD))
                .child(Button::new("settings-close").label("✕").ghost().small()
                    .on_click(cx.listener(|a, _, _, cx| { a.settings_open = false; cx.notify(); }))))
            .child(div().flex().flex_1().overflow_y_scrollbar().p_5().gap_6().flex_col()
                // Appearance section
                .child(div().flex().flex_col().gap_3()
                    .child(Label::new("Appearance").text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD))
                    .child(div().flex().items_center().justify_between().p_3().rounded_md().border_1().border_color(t.border)
                        .child(Label::new("Theme").text_size(px(13.0)))
                        .child(Label::new("Dark").text_size(px(13.0)).text_color(t.muted_foreground)))
                    .child(div().flex().items_center().justify_between().p_3().rounded_md().border_1().border_color(t.border)
                        .child(Label::new("Font size").text_size(px(13.0)))
                        .child(Label::new("14px").text_size(px(13.0)).text_color(t.muted_foreground))))
                // User section
                .child(div().flex().flex_col().gap_3().mt_4()
                    .child(Label::new("User").text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD))
                    .child(div().p_3().rounded_md().border_1().border_color(t.border)
                        .child(div().flex().flex_col().gap_1()
                            .child(Label::new("Username").text_size(px(11.0)).text_color(t.muted_foreground))
                            .child(Input::new(&self.username_input).text_size(px(13.0)))))
                    .child(div().p_3().rounded_md().border_1().border_color(t.border)
                        .child(div().flex().flex_col().gap_1()
                            .child(Label::new("Account type").text_size(px(11.0)).text_color(t.muted_foreground))
                            .child(Label::new("Local account").text_size(px(13.0)).font_weight(FontWeight::MEDIUM)))))
                // Other section
                .child(div().flex().flex_col().gap_3().mt_4()
                    .child(Label::new("Other").text_size(px(16.0)).font_weight(FontWeight::SEMIBOLD))
                    .child(div().flex().items_center().justify_between().p_3().rounded_md().border_1().border_color(t.border)
                        .child(Label::new("Auto-save conversations").text_size(px(13.0)))
                        .child(Label::new("On").text_size(px(13.0)).text_color(t.success)))
                    .child(div().flex().items_center().justify_between().p_3().rounded_md().border_1().border_color(t.border)
                        .child(Label::new("Send diagnostics").text_size(px(13.0)))
                        .child(Label::new("Off").text_size(px(13.0)).text_color(t.muted_foreground)))
                    .child(div().flex().items_center().justify_between().p_3().rounded_md().border_1().border_color(t.border)
                        .child(Label::new("Version").text_size(px(13.0)))
                        .child(Label::new("1.0.0").text_size(px(13.0)).text_color(t.muted_foreground)))))
    }

    fn timeline(&self, tl: &Timeline, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let timeline_id = self.selected.clone();
        let empty = tl.messages.is_empty() && tl.tools.is_empty() && tl.summary.is_empty();
        if self.follow_tail.replace(false) {
            self.timeline_scroll.scroll_to_bottom();
        }

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
                list = list.child(div().id(ElementId::NamedInteger("msg".into(), midx as u64))
                    .flex().justify_end()
                    .child(div().max_w(px(560.0)).rounded_lg().bg(t.muted).px_3().py_2()
                        .child(rich_div(&t, &m.text, false))));
                midx += 1;
                continue;
            }
            if m.role == Role::Progress {
                let start = midx;
                let mut end = midx;
                while end < tl.messages.len() && tl.messages[end].role == Role::Progress {
                    end += 1;
                }
                // Reasoning is visible, not hidden behind a button: the first
                // chunk always shows, the rest expands when the text is clicked.
                let first = &tl.messages[start];
                let key = format!("{}:{}", timeline_id, start);
                let expandable = end - start > 1;
                let open = !expandable || self.open_thoughts.contains(&key);
                let time = first.time.clone();
                let head_text = first.text.clone();
                let rest: Vec<String> = tl.messages[start + 1..end].iter().map(|p| p.text.clone()).collect();
                let mut body = div().flex().flex_col().gap_1().flex_1()
                    .child(rich_div(&t, &head_text, true));
                if open {
                    for r in &rest {
                        body = body.child(rich_div(&t, r, true));
                    }
                }
                let mut block = div().id(ElementId::NamedInteger("msg".into(), start as u64)).flex().gap_3()
                    .child(self.avatar("F", t.accent))
                    .child(div().flex().flex_col().gap_1().flex_1()
                        .child(div().flex().items_center().gap_2()
                            .child(Label::new("Fennec").font_weight(FontWeight::SEMIBOLD).text_size(px(11.5)))
                            .children((!time.is_empty()).then(|| Label::new(time.clone()).font_family("monospace").text_size(px(10.5)).text_color(t.muted_foreground)))
                            .child(Label::new("Thinking").italic().text_size(px(10.5)).text_color(t.success))));
                if expandable {
                    let key2 = key.clone();
                    block = block.child(body.id(ElementId::Name(format!("think-{start}").into()))
                        .on_click(cx.listener(move |a, _, _, cx| {
                            if a.open_thoughts.contains(&key2) { a.open_thoughts.remove(&key2); }
                            else { a.open_thoughts.insert(key2.clone()); }
                            cx.notify();
                        })));
                } else {
                    block = block.child(body);
                }
                list = list.child(block);
                midx = end;
                continue;
            }
            // The assistant's finished answer keeps its avatar row.
            list = list.child(div().id(ElementId::NamedInteger("msg".into(), midx as u64)).flex().gap_3()
                .child(self.avatar("F", t.accent))
                .child(div().flex().flex_col().gap_1().flex_1()
                    .child(div().flex().items_center().gap_2()
                        .child(Label::new("Fennec").font_weight(FontWeight::SEMIBOLD).text_size(px(11.5)))
                        .children((!m.time.is_empty()).then(|| Label::new(m.time.clone()).font_family("monospace").text_size(px(10.5)).text_color(t.muted_foreground))))
                    .child(rich_div(&t, &m.text, false))));
            midx += 1;
        }
        if tl.streaming && !tl.streaming_text.is_empty() {
            list = list.child(div().flex().gap_3()
                .child(self.avatar("F", t.accent))
                .child(div().flex().flex_col().gap_1().flex_1()
                    .child(div().flex().items_center().gap_2()
                        .child(Label::new("Fennec").font_weight(FontWeight::SEMIBOLD).text_size(px(11.5)))
                        .child(Label::new("Thinking").italic().text_size(px(10.5)).text_color(t.success)))
                    .child(rich_div(&t, &tl.streaming_text, false))));
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
        for card in &tl.tools {
            let open = self.expanded.contains(&card.id);
            let is_edit = card.category == "EDIT";
            let path = card.path.clone();
            // A file the run produced is shown like an attachment above the
            // change that made it; clicking the card reviews the same diff.
            if is_edit {
                if let Some(p) = path.as_deref() {
                    let file = std::path::Path::new(p);
                    let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
                    let dir = file.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
                    let kind = artifact_kind(&name);
                    let review_path = path.clone().unwrap_or_default();
                    list = list.child(div().id(ElementId::Name(format!("art-{}", card.id).into()))
                        .rounded_md().border_1().border_color(t.border).bg(t.muted).px_3().py_2()
                        .flex().items_center().justify_between()
                        .on_click(cx.listener(move |a, _, window, cx| { a.review_edit(review_path.clone(), window, cx); cx.notify(); }))
                        .child(div().flex().items_center().gap_2()
                            .child(div().w(px(26.0)).h(px(26.0)).rounded_md().bg(t.primary).flex().items_center().justify_center().flex_shrink_0()
                                .child(Label::new("📄").text_size(px(12.0)).text_color(t.background)))
                            .child(div().flex().flex_col()
                                .child(Label::new(name.clone()).font_weight(FontWeight::SEMIBOLD).text_size(px(12.5)))
                                .child(Label::new(format!("{dir} · {kind}")).text_size(px(10.5)).text_color(t.muted_foreground))))
                        .child(Label::new("…").text_size(px(12.0)).text_color(t.muted_foreground)));
                }
            }
            let mut card_div = div().id(ElementId::Name(format!("tool-{}", card.id).into()))
                .rounded_md().border_1().border_color(t.border).bg(t.muted).px_3().py_2().flex().flex_col().gap_1();
            let (adds, dels) = diff_counts(&format!("{}\n{}", card.detail, card.output));
            let show_counts = is_edit && path.is_some() && (adds > 0 || dels > 0);
            let edited_title = if is_edit && path.is_some() {
                if card.title.starts_with("Write") { "Created 1 file".to_string() } else { "Edited 1 file".to_string() }
            } else {
                card.title.clone()
            };
            let mut header = div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap_2()
                    .child({
                        let key = card.id.clone();
                        Button::new(ElementId::Name(format!("tool-tog-{}", card.id).into()))
                            .label(if open { "▾" } else { "▸" }).ghost().xsmall()
                            .on_click(cx.listener(move |a, _, _, cx| {
                                if a.expanded.contains(&key) { a.expanded.remove(&key); }
                                else { a.expanded.insert(key.clone()); }
                                cx.notify();
                            }))
                    })
                    .child(Label::new(tool_icon(&card.category)).text_size(px(13.0)).text_color(t.primary))
                    .child(Label::new(edited_title).text_size(px(12.5)))
                    .children(show_counts.then(|| div().flex().items_center().gap_1()
                        .child(Label::new(format!("+{adds}")).font_family("monospace").text_size(px(10.5)).text_color(t.success))
                        .child(Label::new(format!("−{dels}")).font_family("monospace").text_size(px(10.5)).text_color(t.red)))));
            let mut right = div().flex().items_center().gap_2();
            right = right.child(div().flex().items_center().gap_1()
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(tool_color(&t, card.status)))
                .child(Label::new(card.status.label()).text_size(px(11.0)).text_color(tool_color(&t, card.status))));
            if !card.duration.is_empty() {
                right = right.child(Label::new(format!("· {}", card.duration)).font_family("monospace").text_size(px(10.5)).text_color(t.muted_foreground));
            }
            if let Some(code) = card.exit {
                right = right.child(Label::new(format!("exit {code}")).font_family("monospace").text_size(px(10.5))
                    .text_color(if code == 0 { t.success } else { t.red }));
            }
            if is_edit {
                if let Some(p) = path.as_deref() {
                    right = right.child({
                        let p = p.to_string();
                        Button::new(ElementId::Name(format!("tool-rev-{}", card.id).into()))
                            .label("Review").ghost().small()
                            .on_click(cx.listener(move |a, _, window, cx| { a.review_edit(p.clone(), window, cx); cx.notify(); }))
                    });
                }
            }
            header = header.child(right);
            card_div = card_div.child(header);
            // The short detail lines are the card's body: always visible, the
            // way the mockup shows a finished inspection; the long output
            // stays behind the toggle.
            if !card.detail.is_empty() {
                card_div = card_div.child(Label::new(card.detail.clone()).font_family("monospace").text_size(px(11.0)).text_color(t.muted_foreground));
            }
            if is_edit {
                if let Some(p) = path.as_deref() {
                    let mut prow = div().flex().items_center().justify_between().border_t_1().border_color(t.border).pt_1()
                        .child(Label::new(p.to_string()).font_family("monospace").text_size(px(10.5)).text_color(t.muted_foreground));
                    if adds > 0 || dels > 0 {
                        prow = prow.child(div().flex().items_center().gap_1()
                            .child(Label::new(format!("+{adds}")).font_family("monospace").text_size(px(10.5)).text_color(t.success))
                            .child(Label::new(format!("−{dels}")).font_family("monospace").text_size(px(10.5)).text_color(t.red)));
                    }
                    card_div = card_div.child(prow);
                }
            }
            if open {
                if !card.output.is_empty() {
                    card_div = card_div.child(div().rounded_md().border_1().border_color(t.border).bg(t.background).p_2()
                        .max_h(px(160.0)).overflow_y_scrollbar().font_family("monospace")
                        .child(Label::new(card.output.clone()).text_size(px(11.5)).text_color(t.foreground)));
                }
            }
            list = list.child(card_div);
        }
        if !tl.summary.is_empty() {
            list = list.child(div().rounded_md().border_1().border_color(t.border).bg(t.muted).p_3().flex().flex_col().gap_1()
                .child(Label::new("Summary").font_weight(FontWeight::SEMIBOLD).text_size(px(11.0)).text_color(t.muted_foreground))
                .child(Label::new(tl.summary.clone()).text_size(px(13.0)).text_color(t.success)));
        }
        list
    }

    fn avatar(&self, letter: &str, bg: Hsla) -> impl IntoElement {
        div().w(px(28.0)).h(px(28.0)).rounded_full().bg(bg).flex().items_center().justify_center().flex_shrink_0()
            .child(Label::new(letter).text_size(px(12.0)).text_color(Hsla::white()))
    }

    fn queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let len = self.task_queue.len();
        let mut col = div().border_t_1().border_color(t.border).bg(t.background).px_5().py_2().flex().flex_col().gap_1()
            .child(div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap_2()
                    .child(Label::new("☰").text_size(px(14.0)).text_color(t.muted_foreground))
                    .child(Label::new(format!("Task queue")).font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                    .child(Label::new(format!("{len} item{}", if len == 1 { "" } else { "s" })).text_size(px(11.0)).text_color(t.muted_foreground)))
                .child(Label::new("View all").text_size(px(11.0)).text_color(t.primary)));
        for (i, task) in self.task_queue.iter().enumerate() {
            let is_next = i == 0;
            col = col.child(div().id(ElementId::NamedInteger("queue-item".into(), i as u64)).flex().items_center().justify_between().py_1()
                .child(div().flex().items_center().gap_2()
                    .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(if is_next { t.success } else { t.muted_foreground }))
                    .child(Label::new(truncate(&task.text, 80)).text_size(px(12.0)).truncate()))
                .child(Label::new(if is_next { "Next" } else { "Queued" }).font_weight(FontWeight::SEMIBOLD).text_size(px(10.5))
                    .text_color(if is_next { t.success } else { t.muted_foreground })));
        }
        col
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let value = self.composer.read(cx).value().trim().to_string();
        let send_enabled = !value.is_empty();

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
        col = col
            .child(div().id("composer-dock").rounded_md().border_1().border_color(t.border).p_3()
                .child(Input::new(&self.composer)))
            .child(div().flex().items_center().justify_between()
                .child(div().flex().items_center().gap_2()
                    .child(Button::new("composer-plus").label("+").ghost().small())
                    .child(Button::new("composer-attach").label("📎").ghost().small())
                    .child(Button::new("composer-mic").label("🎤").ghost().small())
                    .child(Label::new("Hold to dictate").text_size(px(11.0)).text_color(t.muted_foreground)))
                .child(Button::new("send").label("↑").primary().small()
                    .when(!send_enabled, |b| b.disabled(true))
                    .on_click(cx.listener(|a, _, window, cx| { a.send(window, cx); }))));
        col
    }

    fn toggle(&self, id: &'static str, label: &'static str, on: bool, cx: &mut Context<Self>) -> Button {
        let t = cx.theme().clone();
        Button::new(id).label(label).ghost().small()
            .when(on, |b| b.bg(t.muted))
            .on_click(cx.listener(move |a, _, _, cx| {
                match label {
                    "edits" => a.allow_write = !a.allow_write,
                    "commands" => a.allow_shell = !a.allow_shell,
                    "web" => a.allow_web = !a.allow_web,
                    "ask each" => a.ask_each = !a.ask_each,
                    _ => {}
                }
                cx.notify();
            }))
    }

    fn right(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let tl = self.timelines.get(&self.selected).cloned().unwrap_or_default();
        let working = self.busy || self.tl_status(&self.selected) == TaskStatus::Running;
        let task_title = self.tasks.iter().find(|t| t.id == self.selected)
            .map(|t| t.title.clone()).unwrap_or_else(|| "Idle".into());
        let peers = tl.tools.iter().filter(|c| c.category == "PEER" || c.category == "WEB").count();
        let web = tl.tools.iter().filter(|c| c.category == "WEB").count();
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
        let vision_label = match self.env.vision {
            Some(true) => VISION_OK,
            Some(false) => VISION_REFUSED,
            None => "not declared",
        };
        let vision_hint = match self.env.vision {
            Some(false) => VISION_REFUSED,
            _ => VISION_NO_PICKER,
        };

        let panel = div().w(px(300.0)).flex_shrink_0().bg(t.background).border_l_1().border_color(t.border)
            .flex().flex_col().overflow_y_scrollbar()
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().items_center().justify_between()
                .child(div().flex().flex_col()
                    .child(Label::new("Workspace context").font_weight(FontWeight::SEMIBOLD))
                    .child(Label::new("Inspector").text_size(px(12.0)).text_color(t.muted_foreground)))
                .child(Button::new("collapse-inspector").label("⊟").ghost().xsmall()
                    .on_click(cx.listener(|a, _, _, cx| { a.right_open = false; cx.notify(); }))))
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
                            .child(Label::new(if self.busy { "Running · now".to_string() } else { self.tl_status(&self.selected).label().to_string() })
                                .text_size(px(10.5)).text_color(if self.busy { t.primary } else { status_color(&t, self.tl_status(&self.selected)) })))
                })))
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().flex_col().gap_2()
                .child(Label::new("WORKSPACE CONTEXT").text_size(px(10.0)).text_color(t.muted_foreground))
                .child(self.ctx_row("subagents", "👥", "Subagents", if peers > 0 {
                    format!("{peers} delegation card(s)")
                } else {
                    "No active delegated agents".to_string()
                }, cx))
                .child(self.ctx_row("processes", "📡", "Processes", if self.env.core_up {
                    "Event stream connected".to_string()
                } else {
                    "Agent loop offline".to_string()
                }, cx))
                .child(self.ctx_row("skills", "🔧", "Skills", "Repository inspection".to_string(), cx))
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
                .child(self.ctx_row("sources", "🔗", "Sources", "No task sources".to_string(), cx)))
            .child(div().px_4().py_3().border_b_1().border_color(t.border).flex().flex_col().gap_1()
                .child(Label::new("ENVIRONMENT").text_size(px(10.0)).text_color(t.muted_foreground))
                .child(self.row("env-repo", "Repository", ws, cx))
                .child(self.row("env-branch", "Branch", branch_display, cx))
                .child(self.row("env-backend", "Backend", backend_text.to_string(), cx))
                .children((!self.env.model.is_empty()).then(|| self.row("env-model", "Model", self.env.model.clone(), cx)))
                .children((!self.env.ctx.is_empty()).then(|| self.row("env-ctx", "Context", self.env.ctx.clone(), cx)))
                .children((!self.env.tok_s.is_empty()).then(|| self.row("env-tok", "Throughput", self.env.tok_s.clone(), cx)))
                .children((!self.env.core_workspace.is_empty()).then(|| self.row("env-ws", "Core workspace", self.env.core_workspace.clone(), cx)))
                .child(self.row("env-vision", "Vision", vision_label.to_string(), cx))
                .children(self.env.outlets_known.then(|| {
                    let mut col = div().flex().flex_col().gap_1().mt_1();
                    if self.env.outlets.is_empty() {
                        col = col.child(self.row("outlets", "Outlets", "the agent server named none".to_string(), cx));
                    } else {
                        for (i, o) in self.env.outlets.iter().enumerate() {
                            col = col.child(self.row_colored(
                                ElementId::Name(format!("outlet-{i}-{}", o.id).into()),
                                &o.label, &o.detail,
                                if o.present { t.success } else { t.muted_foreground },
                                cx,
                            ));
                        }
                    }
                    col
                }))
                .child(div().mt_1().child(Label::new(vision_hint).text_size(px(10.5)).text_color(t.muted_foreground))));
        panel.child(div().px_4().py_3()
            .child(Button::new("compact").label("Compact this task's context").ghost().small()
                .on_click(cx.listener(|a, _, _, cx| { a.compact_selected(); cx.notify(); }))))
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
        Theme::change(ThemeMode::Dark, None, cx);
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
