//! Fennec — the native desktop application (gpui-kit).
//!
//! One desktop window for the whole suite. This shell drives the agent loop and
//! the inference engine; `fennec-coding-agent` serves the same workflow as a web
//! PWA in the default browser, so nothing here needs a second native app.
// A GUI-subsystem binary: no console window next to the app, ever — a stray
// terminal in the taskbar would be a second app the user did not ask for.
#![cfg_attr(windows, windows_subsystem = "windows")]

use fennec_client::coding_agent::{
    BackendCapabilities, CodingAgentClient, GithubAccountStatus, GithubRepository,
    GithubRepositoryStatus, ObsidianStatus,
};
use fennec_client::engine::{ChatDelta, Engine};
use fennec_client::{git, outlet_rows, suite, Core, Outlet};

use fennec_ui::attention::{
    ApprovalCard, ApprovedCommand, CardState, approval_card, approved_commands_panel,
    attention_badge, card_shows, gate_title, needs_attention, write_of_title,
};
use fennec_ui::confirm::{Confirm, open_confirm};
use fennec_ui::icons::ui_icon;
use fennec_ui::markdown::{artifact_badge, attachment_subtitle, diff_counts, rich_div};
use fennec_ui::theme::{find_preset_index, theme_presets};
use fennec_ui::timeline::{CallShape, GroupRow, block_open, block_title, clock, kind_of, live_stretch, reveal_text, short_text, unique_actions};
use fennec_ui::widgets::{action_card, attachment_card, attachment_chips, brand_fox, composer_dock, composer_toolbar, msg_card, msg_footer, queue_strip, QueueAction, QueueItem, QueueOwner as UiQueueOwner, show_output, speak_tts, tool_group, user_card};
use fennec_ui::workspace_dialog::open_workspace_dialog;
use gpui_kit::base::Root;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    label::Label,
    notification::Notification,
    scroll::ScrollableElement,
    switch::Switch,
    Theme, ThemeMode,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const WARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const MAX_STT_SECONDS: usize = 120;

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Msg {
    id: u64,
    author: String,
    content: String,
    kind: MsgKind,
    time: String,
    collapsed: bool,
    meta: Option<MsgMeta>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
enum MsgKind { User, Think, Action, Attach, Edit, Result }

#[derive(Clone, Serialize, Deserialize, Debug)]
struct MsgMeta {
    duration: Option<String>,
    title: Option<String>,
    add: Option<usize>,
    del: Option<usize>,
    path: Option<String>,
    size: Option<String>,
    /// What a delivered file's call did to it: `created` or `updated`. Only
    /// files the run itself wrote carry one — a file the user attached has no
    /// verb, so the chip can tell the two apart.
    #[serde(default)]
    verb: Option<String>,
    /// The absolute directory the run was jailed to, recorded when a file was
    /// delivered. `path` is relative to it, so this is what turns a delivered
    /// path back into a real file.
    #[serde(default)]
    root: Option<String>,
    /// The loop's `call_id` for a call card, so a `progress` step can be filed
    /// under the card its `tool_start` opened. Live bookkeeping, like the lines
    /// themselves, so it never reaches the history file.
    #[serde(skip, default)]
    call_id: Option<String>,
}

/// The one block a run of calls folded into, and the messages it absorbed.
struct FoldBlocks {
    /// The id of the message this stretch answers: the block keys its element id
    /// and its open state off it, so no two runs in one task can collide.
    id: u64,
    rows: Vec<GroupRow>,
    /// What each row opens on click, in row order.
    acts: Vec<RowOpen>,
    /// Where in the run the thought rows came from, as run indexes. A thought is
    /// a row of the block now, so the card it would have been is hidden — but
    /// only where the block really took it.
    thoughts: Vec<usize>,
    /// The live line of each call this stretch is still inside, in call order.
    /// Drawn after the settled rows, dimmed, unclickable, and counted by nothing:
    /// it says what the run is doing, not what it delivered.
    live_rows: Vec<GroupRow>,
    /// The run is still inside this stretch: no answer has landed after it. The
    /// block then carries a ticking timer and its rows on show — the answer is
    /// what folds it and replaces the clock with a count.
    live: bool,
}

/// What a row inside a block leads to: the change left in the file it wrote, or
/// the whole call and whatever it returned. A block hides the calls, so these are
/// the two ways back to what happened.
#[derive(Clone)]
enum RowOpen {
    File(String),
    Text(String, String),
}

/// Every run of calls in a timeline, as the block to draw and the messages to
/// leave out. The stream here interleaves the calls with the answers between
/// them, so a run is one maximal stretch of calls and its block lands where the
/// calls happened — the same stretch the desktop anchors its block to.
struct Folds {
    hidden: std::collections::HashSet<usize>,
    at: std::collections::HashMap<usize, FoldBlocks>,
}

/// Whether a message is a call the blocks can absorb. A file the user attached
/// themselves is not the run's work, so it never folds. A thought folds too: it
/// is the run's own reasoning, and the block is where the run said it.
///
/// A call still in flight folds as well, because the block is what carries it
/// while it waits: its header keeps ticking and its live line, if it has one,
/// rides inside. Only its own card shows a running call when the stretch holds
/// nothing to draw — see [`fold_calls`].
fn folds_up(m: &Msg) -> bool {
    let meta = m.meta.as_ref();
    match m.kind {
        MsgKind::Think => true,
        MsgKind::Action => true,
        MsgKind::Attach => meta.and_then(|x| x.verb.as_ref()).is_some(),
        MsgKind::Edit => true,
        _ => false,
    }
}

/// Whether the call is still in flight: no result has landed on its card, which
/// is the one thing that makes a row of a block a settled one.
fn call_running(m: &Msg) -> bool {
    m.kind == MsgKind::Action && m.meta.as_ref().and_then(|x| x.duration.as_ref()).is_none()
}

fn call_failed(m: &Msg) -> bool {
    m.meta.as_ref().and_then(|x| x.duration.as_ref()).map(|d| d == "failed").unwrap_or(false)
}

/// The call's own title, which `describe_call` spells `EDIT · Write file x.py`.
fn call_title(m: &Msg) -> String {
    m.meta.as_ref().and_then(|x| x.title.clone()).unwrap_or_else(|| "Action".into())
}

/// The badge a call was filed under — the first half of its own title, which is
/// what the row of a settled call shows as its icon. A live progress line takes
/// the icon of the call it reports on, so the two rows read as one thing.
fn call_badge(m: &Msg) -> String {
    match call_title(m).split_once(" · ") {
        Some((badge, _)) => badge.to_string(),
        None => "⚙".to_string(),
    }
}

/// Whether the call put a file on disk. A failed write wrote nothing, so it
/// folds with the calls that merely ran.
fn is_write(m: &Msg) -> bool {
    match m.kind {
        MsgKind::Attach | MsgKind::Edit => true,
        MsgKind::Action => !call_failed(m) && write_of_title(&call_title(m)).is_some(),
        _ => false,
    }
}

fn base_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// The file a row leads to, as the OS needs it. A delivered path is relative to
/// the directory the loop ran in; `root` is that directory when the message
/// knows it and the current workspace when it does not.
fn absolute(root: &str, path: &str) -> String {
    if path.is_empty() {
        String::new()
    } else if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        let joined = Path::new(root).join(path).to_string_lossy().into_owned();
        // The delivered path is repo-relative with forward slashes; joining it
        // onto a Windows root left `D:\repo\.local-docs/queue-probe/x.md`, which
        // reads as two different locations once it is copied out of the pane.
        if cfg!(windows) { joined.replace('/', "\\") } else { joined }
    }
}

/// The file a write is about, as the call named it. A delivery card carries the
/// path in its own field; a call only says it inside its title.
fn write_target(m: &Msg) -> String {
    let meta = m.meta.as_ref();
    meta.and_then(|x| x.path.clone())
        // A write whose delivered card this window never saw: the call's own
        // title still says which file it was.
        .or_else(|| write_of_title(&call_title(m)).map(|(_, p)| p))
        .unwrap_or_default()
}

fn written_row(m: &Msg) -> (GroupRow, RowOpen) {
    let meta = m.meta.as_ref();
    let path = write_target(m);
    let verb = meta.and_then(|x| x.verb.clone())
        .or_else(|| write_of_title(&call_title(m)).map(|(v, _)| v.to_string()))
        .unwrap_or_else(|| "changed".into());
    let (adds, dels) = if m.kind == MsgKind::Edit {
        (meta.and_then(|x| x.add).unwrap_or(0), meta.and_then(|x| x.del).unwrap_or(0))
    } else {
        diff_counts(&m.content)
    };
    let label = short_text(&path, &call_title(m));
    // A write's row leads to the diff of the file it changed, the same change the
    // delivered card names. The path is relative to the directory the loop ran in,
    // so it is resolved there before git is asked; a write that named no file has
    // nothing to diff and falls back to printing the call.
    let root = meta.and_then(|x| x.root.clone()).unwrap_or_default();
    let open = if path.is_empty() {
        RowOpen::Text(call_title(m), m.content.clone())
    } else {
        RowOpen::File(absolute(&root, &path))
    };
    (GroupRow::new(artifact_badge(&base_name(&label)), label, verb).change(adds, dels), open)
}

fn ran_row(m: &Msg) -> (GroupRow, RowOpen) {
    let title = call_title(m);
    // The badge the call was filed under is the first half of its own title, so
    // the row cannot drift from what the standalone card said.
    let (icon, body) = match title.split_once(" · ") {
        Some((c, rest)) => (c.to_string(), rest.to_string()),
        None => ("⚙".to_string(), title.clone()),
    };
    // The row says the file's name or the command's first words; the whole call
    // is what the row's click prints.
    let path = m.meta.as_ref().and_then(|x| x.path.clone()).unwrap_or_default();
    let label = short_text(&path, &body);
    let failed = call_failed(m);
    let row = GroupRow::new(icon, label, if failed { "Failed".into() } else { "Complete".into() });
    (if failed { row.failed() } else { row }, RowOpen::Text(title, m.content.clone()))
}

fn fold_run(run: &[Msg], id: u64, live: bool, progress: &fennec_ui::timeline::ProgressLines) -> FoldBlocks {
    // A delivered file is a card of its own below the block, so the rows the
    // counts come from are only the calls.
    let calls: Vec<usize> = run.iter().enumerate()
        .filter(|(_, m)| m.kind != MsgKind::Attach && m.kind != MsgKind::Think)
        .map(|(ri, _)| ri).collect();
    // Which row vocabulary each call sits in, so the stretch can be walked in
    // order while `unique_actions` keeps deciding which calls are kept.
    let mut slot = vec![usize::MAX; run.len()];
    for (j, &ri) in calls.iter().enumerate() {
        slot[ri] = j;
    }
    let shapes: Vec<CallShape> = calls.iter()
        .map(|&ri| CallShape { wrote: is_write(&run[ri]), running: false })
        .collect();
    // One row per file, the last write of it winning, and every command on its
    // own line: a run that rewrote one file six times delivered one file, and a
    // block that counted six would be counting its own repetition.
    let called: Vec<String> = calls.iter().map(|&ri| write_target(&run[ri])).collect();
    let refs: Vec<&str> = called.iter().map(String::as_str).collect();
    let keep: std::collections::HashSet<usize> = unique_actions(&shapes, &refs).into_iter().collect();
    let mut blocks = FoldBlocks { id, rows: Vec::new(), acts: Vec::new(), thoughts: Vec::new(),
                                  live_rows: Vec::new(), live };
    // The stretch only carries a live line while the run is inside it. Once the
    // answer landed the calls are history, and a sentence about what one of them
    // "is doing" would claim work that stopped happening.
    for (ri, m) in run.iter().enumerate() {
        if m.kind == MsgKind::Think {
            // The run's own reasoning, as a row of the block it was thought in —
            // folded to its first words, printed in full by the row's click. It
            // stays whether the answer has landed or not: the block is where the
            // thinking happened, and the answer does not own it.
            if m.content.trim().is_empty() {
                continue;
            }
            blocks.rows.push(GroupRow::thought(&m.content));
            blocks.acts.push(RowOpen::Text("Thinking".into(), m.content.clone()));
            blocks.thoughts.push(ri);
            continue;
        }
        if live && call_running(m) {
            // A call still in flight is not work the run delivered, so it joins no
            // counted bucket and opens nothing on click. What it may contribute is
            // the one line it has reported about itself, dimmed at the block's
            // foot; with no report it keeps its own Running card instead.
            if let Some(id) = m.meta.as_ref().and_then(|x| x.call_id.clone()) {
                if let Some(row) = progress.row(&id, &call_badge(m)) {
                    blocks.live_rows.push(row);
                }
            }
            continue;
        }
        let j = slot[ri];
        if j == usize::MAX || !keep.contains(&j) {
            continue;
        }
        let (row, act) = if shapes[j].wrote { written_row(m) } else { ran_row(m) };
        blocks.rows.push(row.kind(kind_of(&call_title(m), shapes[j].wrote)));
        blocks.acts.push(act);
    }
    blocks
}

fn fold_calls(msgs: &[Msg], working: bool, progress: &fennec_ui::timeline::ProgressLines) -> Folds {
    let mut folds = Folds { hidden: std::collections::HashSet::new(), at: std::collections::HashMap::new() };
    let mut i = 0usize;
    while i < msgs.len() {
        if !folds_up(&msgs[i]) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i;
        while end < msgs.len() && folds_up(&msgs[end]) {
            end += 1;
        }
        // Only the last stretch ticks: an older one that never got its answer is
        // history, and a second live clock would appear as soon as a new run started.
        let live = live_stretch(working, end, msgs.len());
        // The key is the message the stretch answers, not the message it starts
        // with: while the run is inside, its first message is the "Thinking…"
        // placeholder, and the first real event retires it. A block keyed on that
        // would change identity halfway through, so a click made before the swap
        // opened nothing and the answer landed on a folded block.
        let anchor = start.checked_sub(1).map_or(msgs[start].id, |p| msgs[p].id);
        let blocks = fold_run(&msgs[start..end], anchor, live, progress);
        // A delivered file stays a card of its own, so it is not hidden by the
        // block that absorbed the call that wrote it. A thought is hidden only
        // where its own block took it as a row; one the block refused — an empty
        // placeholder — has nothing to show either way. A call still in flight is
        // hidden once its block has a line of it to carry, and otherwise keeps its
        // own Running card, which is all it ever showed.
        let absorbed = &blocks.thoughts;
        let taken = |k: usize| -> bool {
            let m = &msgs[k];
            match m.kind {
                MsgKind::Attach => false,
                MsgKind::Think => absorbed.contains(&(k - start)),
                _ if call_running(m) => m.meta.as_ref()
                    .and_then(|x| x.call_id.as_ref())
                    .is_some_and(|id| progress.get(id).is_some()),
                _ => true,
            }
        };
        // A block draws when it has rows to count or a live line to carry. A
        // stretch whose only message is a running call with nothing to report is
        // left as that call's own card, exactly as it was before.
        // Which rows the block took is decided before the block is moved into
        // `folds`: the answer reads `blocks.thoughts`, so it cannot be worked out
        // after the insert has consumed it.
        let hidden = (start..end).filter(|&k| taken(k)).collect::<Vec<_>>();
        if !blocks.rows.is_empty() || !blocks.live_rows.is_empty() {
            folds.at.insert(start, blocks);
            folds.hidden.extend(hidden);
        }
        i = end;
    }
    folds
}

#[derive(Clone)]
struct Toast {
    text: String,
    good: bool,
    at: std::time::Instant,
}

/// One gated call the agent loop stopped on. Written from the core's
/// `approval_required` step, so every field is a word the loop itself chose.
#[derive(Clone, Serialize, Deserialize, Debug)]
struct ApprovalRequest {
    id: String,
    action: String,
    risk_level: String,
    command: Option<String>,
    resolved: Option<ApprovalResolution>,
    /// The tool table's own badge (`CMD`, `EDIT`, `READ`, …).
    #[serde(default)]
    category: String,
    /// Why the gate stopped the run, in the loop's words.
    #[serde(default)]
    reason: String,
    #[serde(default)]
    path: Option<String>,
    /// The question is no longer live: the run ended, or the loop's own timeout
    /// refused the call while nobody answered. That is not the same as the user
    /// not having answered, and the card has to say which it is.
    #[serde(default)]
    expired: bool,
    #[serde(default)]
    note: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ApprovalResolution {
    choice_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Task {
    id: u64,
    title: String,
    status: String,
    repo: String,
    msgs: Vec<Msg>,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    started_at: Option<i64>,
    #[serde(default)]
    approvals: Vec<ApprovalRequest>,
    /// The supervised session this run belongs to on the agent loop
    /// (`/api/agent/*`). Only a run with one can be answered or cancelled.
    #[serde(default)]
    core_sid: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    /// The loop's stage list for this run: each stage's text and whether it is
    /// done. `loop.py` owns the list and republishes it as stages finish; this
    /// shell only draws the last one it received.
    #[serde(default)]
    plan: Vec<(String, bool)>,
    /// What each in-flight call last said it was doing, keyed by the loop's
    /// `call_id`. Live state only — a report replaces the line before it and
    /// leaves when its card settles — so it is not written to the history file.
    #[serde(skip, default)]
    progress: fennec_ui::timeline::ProgressLines,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
enum QueueOwner {
    User,
    Agent,
}

#[derive(Clone, Serialize, Debug, PartialEq, Eq)]
struct QueueEntry {
    text: String,
    owner: QueueOwner,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum QueueEntryWire {
    Legacy(String),
    Current { text: String, owner: QueueOwner },
}

impl<'de> Deserialize<'de> for QueueEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match QueueEntryWire::deserialize(deserializer)? {
            QueueEntryWire::Legacy(text) => Ok(Self { text, owner: QueueOwner::User }),
            QueueEntryWire::Current { text, owner } => Ok(Self { text, owner }),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct History {
    tasks: Vec<Task>,
    active: Option<u64>,
    #[serde(default)]
    queued: Vec<QueueEntry>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    server_url: Option<String>,
    engine_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    access_mode: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    theme: Option<String>,
    font_size: Option<f32>,
    auto_save: Option<bool>,
    notifications: Option<bool>,
    allow_write: Option<bool>,
    allow_shell: Option<bool>,
    allow_web: Option<bool>,
    ask_each: Option<bool>,
    // `default` keeps an older settings.json (which lacks these keys) parseable:
    // without it every stored setting would fall back to its default at once.
    #[serde(default)]
    rail_width: Option<f32>,
    #[serde(default)]
    inspector_width: Option<f32>,
    /// Which inspector view was open last. The PWA keeps the same choice under
    /// the `fennec-insp-tab` key; a settings file from before the tabs exist
    /// simply has no value, and the pane opens on Overview.
    #[serde(default)]
    insp_tab: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Workspace {
    id: String,
    name: String,
    icon: String,
    color: String,
    source_folders: Vec<String>,
    indexed: bool,
    #[serde(default)]
    github_repo: Option<String>,
}

impl Workspace {
    fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            icon: "folder".to_string(),
            color: "orange".to_string(),
            source_folders: vec![],
            indexed: true,
            github_repo: None,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct WorkspaceStore {
    workspaces: Vec<Workspace>,
    active_id: Option<String>,
}

/// Which side pane a divider resizes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Rail,
    Inspector,
}

impl Pane {
    /// The same span the PWA's `paneLimits` gives each side pane.
    fn clamp(self, w: f32) -> f32 {
        match self {
            Pane::Rail => w.clamp(180.0, 440.0),
            Pane::Inspector => w.clamp(220.0, 560.0),
        }
    }

    fn default_width(self) -> f32 {
        match self {
            Pane::Rail => 260.0,
            Pane::Inspector => 300.0,
        }
    }
}

struct App {
    tasks: Vec<Task>,
    active: Option<u64>,
    queued: Vec<QueueEntry>,
    inspector: bool,
    rail_width: f32,
    inspector_width: f32,
    /// The divider being dragged: which pane, the pointer's x at press, and the
    /// pane's width then. Move events land on the root, not the divider.
    pane_drag: Option<(Pane, f32, f32)>,
    attention_only: bool,
    /// What the user last clicked each action block to, keyed on its anchor
    /// message id. Absent means the block follows the rule: open while the run is
    /// inside it, folded once the answer lands.
    block_open: std::collections::HashMap<u64, bool>,
    search_open: bool,
    search: Entity<InputState>,
    composer: Entity<InputState>,
    server_url: String,
    engine_url: String,
    api_key: String,
    model: String,
    access_mode: String,
    /// The reasoning level the model dropdown shows, sent to core as
    /// `reasoning_effort`. `auto` lets the loop pick from the task.
    reasoning_effort: String,
    theme: String,
    font_size: f32,
    auto_save: bool,
    notifications: bool,
    perm_dropdown_open: bool,
    model_dropdown_open: bool,
    stt_recording: bool,
    stt_stop: Arc<AtomicBool>,
    stt_buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    stt_rate: Arc<std::sync::atomic::AtomicU32>,
    stt_stream: Option<cpal::Stream>,
    composer_restore: Option<String>,
    allow_write: bool,
    allow_shell: bool,
    allow_web: bool,
    ask_each: bool,
    settings_path: PathBuf,
    backend_ok: bool,
    /// Legacy core compatibility state. New prompts prefer the Rust task API;
    /// this lane remains only for routes not yet represented by that contract.
    core: Core,
    core_up: bool,
    rust_up: bool,
    /// The directory the loop will accept a task for; anything else is a 403.
    core_root: String,
    /// The browser agents (`web_task` / `coworker_task`) as the loop last
    /// reported them, plus whether it ever reported at all. `outlets_known` is
    /// not derivable from the list: a loop that is down and a loop that named no
    /// installed agent are different claims, and only the second one means the
    /// user has something to install.
    outlets: Vec<Outlet>,
    outlets_known: bool,
    github: GithubAccountStatus,
    github_repositories: GithubRepositoryStatus,
    obsidian: ObsidianStatus,
    capabilities: Option<BackendCapabilities>,
    /// Runs restored from history that the loop may still be working on: each
    /// is this window's task id and the session id it was started as. Consumed
    /// by the first resume attempt, whichever way it goes.
    resume: Vec<(u64, String)>,
    agents: Vec<String>,
    path: PathBuf,
    next_id: u64,
    response_rx: std::sync::mpsc::Receiver<BackendResponse>,
    response_tx: std::sync::mpsc::Sender<BackendResponse>,
    pending_think: Option<(u64, u64)>,
    pending_content: Option<(u64, u64)>,
    tools: std::collections::HashMap<String, (u64, u64)>,
    _task: gpui_kit::Task<()>,
    scroll_handle: ScrollHandle,
    line: String,
    toasts: Vec<Toast>,
    pending_send: Option<(String, std::time::Instant)>,
    workspaces: Vec<Workspace>,
    active_workspace_id: Option<String>,
    /// Rows whose +/×/✎ buttons the pointer is over. The PWA reveals the same
    /// buttons only on row hover, so the native rail hides them until then.
    hovered_workspace_id: Option<String>,
    hovered_task_id: Option<u64>,
    hovered_ws_header: bool,
    workspaces_path: PathBuf,
    /// Engine health, model id, context and throughput, probed with the service.
    engine: EngineState,
    /// Files picked for the prompt being written, not yet sent. They fold into
    /// the one run that sends them and are gone after, so a picked file cannot
    /// ride along into every later prompt.
    pending_atts: Vec<PathBuf>,
    /// Git branch of the directory the runs work in.
    branch: String,
    /// Input field for creating a new GitHub repository from the unbound banner.
    repo_create_input: Entity<InputState>,
    /// Root of the Fennec suite on disk, discovered once at launch. The fallback
    /// working directory when neither a workspace nor the core names one.
    suite_root: PathBuf,
    /// The delivered file whose card the user clicked, shown in the right pane.
    open_file: Option<FileView>,
    /// Which of the three inspector views is showing: the run's context, the
    /// workspace's files, or a page read through the loop.
    insp_tab: InspTab,
    fm: FmState,
    br: BrState,
    br_input: Entity<InputState>,
    /// Listing reads and file reads answer out of order, so each carries the
    /// number its own request took and a late one is dropped rather than painting
    /// a folder or a panel the user has already moved off.
    fm_seq: usize,
    file_seq: usize,
    br_seq: usize,
    save_pending: Cell<bool>,
    last_save: Cell<std::time::Instant>,
}

/// What the right pane holds after a delivered-file card is clicked. The text is
/// read once, at the click, so the pane sits beside the run's context instead of
/// handing the file to an app outside it.
#[derive(Clone)]
struct FileView {
    path: String,
    name: String,
    body: String,
    /// Line count and size, or the reason there is nothing to show.
    meta: String,
    failed: bool,
    /// Set instead of `body` when the file is a picture. Decoded off the UI
    /// thread, because a bitmap costs as much RAM as the file had bytes.
    image: Option<Arc<Image>>,
}

/// One view of the right pane. The panel was one column of run context; Files
/// and Browser each want the whole column, so context became a tab instead of a
/// fourth section nobody scrolls to. Same three names the PWA puts in its
/// `insp-tab-*` buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InspTab {
    Overview,
    Files,
    Browser,
}

impl InspTab {
    const ALL: [InspTab; 3] = [InspTab::Overview, InspTab::Files, InspTab::Browser];

    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Files => "Files",
            Self::Browser => "Browser",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Files => "files",
            Self::Browser => "browser",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.as_str() == value)
    }
}

/// One row of the Files listing, as the workspace jail returns it.
#[derive(Debug)]
struct FmRow {
    name: String,
    is_dir: bool,
    size: u64,
}

/// A listing that answered: the root the jail resolved to, and its rows.
#[derive(Debug)]
struct FmListing {
    root: String,
    rows: Vec<FmRow>,
}

/// A preview that answered: the size line and either the text or the picture.
/// Where the file is comes from the pane, which knows the root — the route
/// echoes the relative path it was handed, not an absolute one.
#[derive(Debug)]
struct FileRead {
    meta: String,
    body: String,
    image: Option<Arc<Image>>,
}

/// A page the loop's reader handed back.
#[derive(Debug)]
struct BrPage {
    url: String,
    status: u16,
    title: String,
    text: String,
    truncated: bool,
}

/// What the Files tab holds. `path` is `.` until a workspace names a folder,
/// which is the jail's own root; `note` says why there are no rows.
#[derive(Debug)]
struct FmState {
    path: String,
    root: String,
    rows: Vec<FmRow>,
    note: String,
    loaded: bool,
}

impl Default for FmState {
    fn default() -> Self {
        Self { path: ".".into(), root: String::new(), rows: Vec::new(), note: String::new(), loaded: false }
    }
}

/// What the Browser tab holds. gpui draws no HTML and a webview is out, so this
/// pane shows the page as the text the loop's reader hands the agent, under the
/// address, status and title the same read reports.
#[derive(Debug, Default)]
struct BrState {
    target: String,
    meta: String,
    title: String,
    body: String,
    /// Addresses the user has read, oldest first, so ← walks back over them.
    hist: Vec<String>,
}

/// The folder to name in a work-route request. Empty asks for the server's own
/// root, which is what `.` means; repeating it would re-encode a folder the jail
/// already resolves to.
fn workspace_param(workspace: &str) -> String {
    let trimmed = workspace.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || trimmed == "." { String::new() } else { workspace.to_string() }
}

/// A listing path made absolute against the root the jail reported, the same way
/// the PWA's `joinPath` does, so both panes name a file the same.
fn jail_path(root: &str, rel: &str) -> String {
    if rel.is_empty() {
        return String::new();
    }
    if root.is_empty() || root == "." {
        return rel.to_string();
    }
    let absolute = rel.as_bytes().get(1) == Some(&b':')
        || rel.starts_with('/')
        || rel.starts_with("\\\\");
    if absolute {
        return rel.to_string();
    }
    let sep = if root.contains('\\') { '\\' } else { '/' };
    let joined = format!(
        "{}{sep}{}",
        root.trim_end_matches(['\\', '/']),
        rel.trim_start_matches(['\\', '/'])
    );
    // A copied path gets pasted into a shell or an Open dialog; one mixed
    // separator in the middle of a Windows path reads as two different things.
    if sep == '\\' { joined.replace('/', "\\") } else { joined }
}

fn decode_listing(value: serde_json::Value) -> Result<FmListing, String> {
    if value["ok"] != serde_json::Value::Bool(true) {
        return Err(value["error"].as_str().unwrap_or("directory listing failed").to_string());
    }
    let mut rows: Vec<FmRow> = value["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| FmRow {
            name: entry["name"].as_str().unwrap_or("").to_string(),
            is_dir: entry["is_dir"].as_bool().unwrap_or(false),
            size: entry["size"].as_u64().unwrap_or(0),
        })
        .collect();
    // Folders first, then by name, ignoring case: the same order the PWA sorts to.
    rows.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(FmListing { root: value["root"].as_str().unwrap_or("").to_string(), rows })
}

/// The answer behind a file preview, turned into what the pane paints: text for
/// a `content` field, a decoded picture for `mime` and base64 `data`. The route
/// used to answer a picture with lossy text, which filled this pane with
/// replacement characters. Decoding happens here, on the caller's thread, so the
/// UI never stops on a multi-megabyte bitmap.
fn decode_file(value: serde_json::Value) -> Result<FileRead, String> {
    if value["ok"] != serde_json::Value::Bool(true) {
        return Err(value["error"].as_str().unwrap_or("file read failed").to_string());
    }
    if value["kind"].as_str() == Some("image") {
        let mime = value["mime"].as_str().unwrap_or_default();
        let format = ImageFormat::from_mime_type(mime)
            .ok_or_else(|| format!("This pane has no decoder for {mime}."))?;
        use base64::Engine as _;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(value["data"].as_str().unwrap_or_default())
            .map_err(|_| "The image arrived incomplete.".to_string())?;
        let bytes = value["bytes"].as_u64().unwrap_or(0);
        return Ok(FileRead {
            meta: format!("{mime} · {}", human_size(bytes)),
            body: String::new(),
            image: Some(Arc::new(Image::from_bytes(format, raw))),
        });
    }
    let body = value["content"].as_str().unwrap_or("").to_string();
    let bytes = value["bytes"].as_u64().unwrap_or_else(|| body.len() as u64);
    let lines = value["lines"].as_u64();
    let meta = match lines {
        Some(n) => format!("{n} {} · {}", if n == 1 { "line" } else { "lines" }, human_size(bytes)),
        None => human_size(bytes),
    };
    Ok(FileRead { meta, body, image: None })
}

fn decode_page(value: serde_json::Value) -> Result<BrPage, String> {
    if value["ok"] != serde_json::Value::Bool(true) {
        return Err(value["error"].as_str().unwrap_or("the page could not be read").to_string());
    }
    Ok(BrPage {
        url: value["url"].as_str().unwrap_or("").to_string(),
        status: value["status"].as_u64().unwrap_or(0) as u16,
        title: value["title"].as_str().unwrap_or("").to_string(),
        text: value["text"].as_str().unwrap_or("").to_string(),
        truncated: value["truncated"].as_bool().unwrap_or(false),
    })
}

#[derive(Debug)]
enum BackendResponse {
    Offline,
    Error(String),
    TaskCreated { task_id: u64, backend_id: String },
    ThinkDelta(String),
    ContentDelta(String),
    ActionStarted { tool_id: String, title: String },
    ActionDone { tool_id: String, duration: Option<String>, detail: String },
    Complete { task_id: u64, status: String, final_text: String },
    Failed(String),
    BackendInfo { names: Vec<String> },
    GithubRepoCreated { workspace_id: String, result: Result<GithubRepository, String> },
    /// Which services answer, re-probed every couple of seconds and reported
    /// only when something changed. `branch` is the workspace's git branch, and
    /// `outlets` is the loop's last browser-agent report (`outlets_known` says
    /// whether there ever was one).
    ServeState { core_up: bool, core_root: String, axum: bool, engine: EngineState,
                 branch: String, outlets: Vec<Outlet>, outlets_known: bool,
                 github: GithubAccountStatus, github_repositories: GithubRepositoryStatus,
                 obsidian: ObsidianStatus, capabilities: Option<BackendCapabilities> },
    Core(CoreEvent),
    Stt(Result<String, String>),
    ObsidianWrite(Result<String, String>),
    /// Inspector reads. Each carries the number its own request took so a late
    /// answer cannot paint a folder or a page the user has navigated away from.
    WorkFiles { seq: usize, result: Result<FmListing, String> },
    WorkFile { seq: usize, name: String, fallback: String, result: Result<FileRead, String> },
    Browse { seq: usize, fallback: String, result: Result<BrPage, String> },
}

/// What the inference engine reports about itself. `vision` is `None` when the
/// engine is silent on modalities, which is not the same as "no images".
#[derive(Debug, Default, Clone, PartialEq)]
struct EngineState {
    up: bool,
    model: String,
    ctx: String,
    tok_s: String,
    vision: Option<bool>,
}

/// `vision` true only when the engine says so, either directly or by listing
/// image input modalities. The image attach path is gated on this: a text-only
/// checkpoint refuses an image part, so the button must not offer one.
fn engine_vision(health: &serde_json::Value) -> Option<bool> {
    if let Some(v) = health["vision"].as_bool() {
        return Some(v);
    }
    health["input_modalities"].as_array().map(|mods| {
        mods.iter().any(|m| m.as_str().unwrap_or("").eq_ignore_ascii_case("image"))
    })
}

/// One thing a supervised core run did, translated off the UI thread.
#[derive(Debug)]
enum CoreEvent {
    /// `/api/agent/start` answered with the session id this run is polled by.
    Started { task_id: u64, sid: String },
    /// The service still holds a run this window restored from history, so its
    /// steps are fetched again and the loop polled from the start.
    Resume { task_id: u64, sid: String },
    /// The first answer about a resumed run: what follows replaces the tail of
    /// the transcript the previous window left behind.
    Replay { task_id: u64 },
    /// The loop's own reasoning, and its answer to the user.
    Reasoning { task_id: u64, text: String },
    Answer { task_id: u64, text: String },
    /// A tool call: opened by `tool_start`, closed by the `tool` step that
    /// carries the result. `ok` is `None` while it is still running.
    Tool { task_id: u64, id: String, title: String, detail: String, ok: Option<bool>, output: String },
    /// What a call still blocked says it is doing, in the loop's own words. It
    /// belongs to the card its `tool_start` opened and replaces the line before
    /// it, so one call never shows two reports.
    Progress { task_id: u64, id: String, line: String },
    /// A file the run put on disk, delivered to the user as its own card. Only
    /// `write_file` and `edit_file` produce one; a read names a path without
    /// changing it.
    Deliver { task_id: u64, name: String, path: String, verb: &'static str, root: String },
    /// The loop's stage list, as it last published it: each entry is a stage's
    /// text and whether the loop reported it done. Republished as stages finish,
    /// so the newest one replaces what is drawn.
    Plan { task_id: u64, entries: Vec<(String, bool)> },
    /// Follow-up prompts the loop proposed for a complex run. It sends these
    /// before the step that ends the run, which is what drains the queue, so
    /// the first one is already waiting when the run finishes. No `task_id`:
    /// the queue belongs to the window, not to one task.
    Queue(Vec<String>),
    /// A question the loop stopped on.
    Approval { task_id: u64, approval: ApprovalRequest },
    /// What happened to an answer this window sent: `error` is the loop's own
    /// refusal, which means the question was no longer live.
    Answered { task_id: u64, approval_id: String, decision: String, error: Option<String> },
    /// The approval ids the loop still holds; anything else can no longer be
    /// answered, which is not the same as unanswered.
    Pending { task_id: u64, ids: Vec<String> },
    Status { task_id: u64, status: String },
    /// The run could not be started, or stopped being pollable.
    Refused { task_id: u64, error: String },
}

impl App {
    fn new(
        path: PathBuf,
        settings_path: PathBuf,
        composer: Entity<InputState>,
        search: Entity<InputState>,
        br_input: Entity<InputState>,
        repo_create_input: Entity<InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();

        let _task = cx.spawn(async move |weak, cx: &mut AsyncApp| {
            let mut last_tick = std::time::Instant::now();
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
                if weak.update(cx, |a, cx| {
                    a.poll_responses(cx);
                    a.flush_save();
                    a.toasts.retain(|t| t.at.elapsed() < std::time::Duration::from_secs(6));
                    if a.working() && last_tick.elapsed() >= std::time::Duration::from_secs(1) {
                        last_tick = std::time::Instant::now();
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
                InputEvent::PressEnter { .. } => this.send(window, cx),
                _ => {}
            }
        }).detach();

        // The Browser tab's address field. Enter reads it, the same way the PWA's
        // form submits it.
        cx.observe(&br_input, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&br_input, window, |this, _, event: &InputEvent, _, cx| {
            match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let target = this.br_input.read(cx).value().to_string();
                    this.browse_go(target);
                }
                _ => {}
            }
        }).detach();

        let settings = load_settings(&settings_path);
        // The environment has to win over the settings file: this app writes its
        // defaults there on first run, so a stored value always exists, and a var
        // that loses to it - `FENNEC_AGENT_URL` pointed at another loop - could
        // never take effect on a machine that has opened the app once.
        let server_url = std::env::var("FENNEC_AGENT_URL").ok()
            .or(settings.server_url)
            .unwrap_or_else(|| "http://127.0.0.1:8787".into());
        let engine_url = std::env::var("FENNEC_ENGINE_URL").ok()
            .or(settings.engine_url)
            .unwrap_or_else(|| "http://127.0.0.1:8184".into());
        let api_key = std::env::var("FENNEC_API_KEY").ok()
            .or_else(|| std::env::var("COLI_API_KEY").ok())
            .or(settings.api_key)
            .unwrap_or_else(|| "local".into());
        let model = settings.model.unwrap_or_else(|| "qwen3_5_moe".into());
        let access_mode = settings.access_mode.unwrap_or_else(|| "ask".into());
        // `auto` is the only level safe to fall back to: core validates the
        // string, and an unrecognised one would 422 the very first run.
        let reasoning_effort = settings.reasoning_effort
            .filter(|l| fennec_ui::controls::REASONING_LEVELS.iter().any(|(id, _)| id == l))
            .unwrap_or_else(|| "auto".into());
        let theme = settings.theme.unwrap_or_else(|| "dark".into());
        let font_size = settings.font_size.unwrap_or(14.0);
        let auto_save = settings.auto_save.unwrap_or(true);
        let notifications = settings.notifications.unwrap_or(true);
        let allow_write = settings.allow_write.unwrap_or(false);
        let allow_shell = settings.allow_shell.unwrap_or(false);
        let allow_web = settings.allow_web.unwrap_or(false);
        let ask_each = settings.ask_each.unwrap_or(false);
        let rail_width = Pane::Rail.clamp(
            settings.rail_width.unwrap_or_else(|| Pane::Rail.default_width()));
        let inspector_width = Pane::Inspector.clamp(
            settings.inspector_width.unwrap_or_else(|| Pane::Inspector.default_width()));
        let insp_tab = settings.insp_tab.as_deref()
            .and_then(InspTab::parse)
            .unwrap_or(InspTab::Overview);
        // Both backends listen on the same port, so the core lane shares this URL.
        let core_base = server_url.clone();

        let mut app = Self {
            tasks: vec![], active: None, queued: vec![], inspector: true,
            rail_width, inspector_width, pane_drag: None,
            open_file: None,
            insp_tab, fm: FmState::default(), br: BrState::default(), br_input,
            fm_seq: 0, file_seq: 0, br_seq: 0,
            attention_only: false, block_open: Default::default(), search_open: false, search, composer,
            server_url, engine_url, api_key, model, access_mode, reasoning_effort,
            theme, font_size, auto_save,
            notifications,
            perm_dropdown_open: false, model_dropdown_open: false,
            stt_recording: false,
            stt_stop: Arc::new(AtomicBool::new(false)),
            stt_buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
            stt_rate: Arc::new(std::sync::atomic::AtomicU32::new(16000)),
            stt_stream: None,
            composer_restore: None,
            allow_write, allow_shell, allow_web, ask_each,
            settings_path: settings_path.clone(), backend_ok: false,
            core: Core { base: core_base }, core_up: false, rust_up: false, core_root: String::new(),
            outlets: Vec::new(), outlets_known: false, github: GithubAccountStatus::default(),
            github_repositories: GithubRepositoryStatus::default(), obsidian: ObsidianStatus::default(),
            capabilities: None, resume: Vec::new(),
            agents: vec![],
            path, next_id: 1, response_rx: rx, response_tx: tx,
            pending_think: None, pending_content: None, tools: std::collections::HashMap::new(),
            _task, scroll_handle: ScrollHandle::new(), line: String::new(), toasts: Vec::new(), pending_send: None,
            workspaces: vec![], active_workspace_id: None,
            hovered_workspace_id: None, hovered_task_id: None, hovered_ws_header: false,
            workspaces_path: settings_path.parent().unwrap_or(&settings_path).join("workspaces.json"),
            engine: EngineState::default(), branch: String::new(), repo_create_input,
            pending_atts: Vec::new(),
            suite_root: suite::discover().root,
            save_pending: Cell::new(false),
            last_save: Cell::new(std::time::Instant::now()),
        };
        app.load_workspaces();
        app.fetch_backend_info();
        app.watch_service();
        app
    }

    /// Probe the Rust task service and the legacy core compatibility routes on
    /// the same loopback URL. `/api/health` identifies the Rust service by its
    /// backend field, while the legacy payload carries `app`.
    fn watch_service(&self) {
        let tx = self.response_tx.clone();
        let base = self.core.base.clone();
        let engine_base = self.engine_url.clone();
        let engine_key = self.api_key.clone();
        let suite_root = self.suite_root.clone();
        std::thread::spawn(move || {
            let core = Core { base: base.clone() };
            let agent = CodingAgentClient::with_base(base);
            let engine = Engine { base: engine_base, key: engine_key };
            let mut last: Option<(
                bool, String, bool, EngineState, String, bool, Vec<Outlet>, GithubAccountStatus,
                GithubRepositoryStatus, ObsidianStatus, Option<BackendCapabilities>,
            )> = None;
            loop {
                let health = core.health();
                let (core_up, core_root, axum) = match &health {
                    Some(h) if h["app"].is_string() =>
                        (true, h["workspace"].as_str().unwrap_or("").to_string(), false),
                    Some(_) => (false, String::new(), true),
                    None => (false, String::new(), false),
                };
                // The branch is read where the work happens: the loop's own
                // workspace when it answers, otherwise the suite checkout.
                let dir = if core_root.is_empty() {
                    suite_root.clone()
                } else {
                    PathBuf::from(&core_root)
                };
                let branch = git::repo_context(&dir.to_string_lossy()).branch;
                let mut engine_state = EngineState::default();
                if let Some(h) = engine.health() {
                    engine_state.up = true;
                    engine_state.vision = engine_vision(&h);
                    if let Some(m) = h["model"].as_str() {
                        engine_state.model = m.to_string();
                    }
                    if let Some((id, len)) = engine.model_id() {
                        if engine_state.model.is_empty() {
                            engine_state.model = id;
                        }
                        if len > 0 {
                            engine_state.ctx = format!("{len} tokens");
                        }
                    }
                    if let Some(mt) = engine.metrics() {
                        if let Some(t) = mt["tokens_per_second"].as_f64() {
                            engine_state.tok_s = format!("{t:.2} tok/s");
                        }
                    }
                }
                // `/api/outlets` is served by whichever loop answers `/api/health` —
                // the Rust backend or the legacy core route — so the rail probes on
                // either. An empty rail here means the loop did not answer, which is
                // a different claim from "not installed" and says so.
                // Core caches the probes; this is one loopback GET.
                let probed = if core_up || axum { core.outlets() } else { None };
                let outlets_known = probed.is_some();
                let outlets: Vec<Outlet> = probed.as_ref()
                    .map(|o| o.list().into_iter().cloned().collect())
                    .unwrap_or_default();
                let github = agent.github_account();
                let github_repositories = agent.github_repositories();
                let obsidian = agent.obsidian_status();
                let capabilities = agent.capabilities();
                let state = (core_up, core_root.clone(), axum, engine_state.clone(), branch.clone(),
                             outlets_known, outlets.clone(), github.clone(), github_repositories.clone(),
                             obsidian.clone(), capabilities.clone());
                if last.as_ref() != Some(&state) {
                    let _ = tx.send(BackendResponse::ServeState {
                        core_up, core_root, axum, engine: engine_state, branch,
                        outlets, outlets_known, github, github_repositories, obsidian, capabilities,
                    });
                    last = Some(state);
                }
                std::thread::sleep(std::time::Duration::from_millis(2000));
            }
        });
    }

    fn load(&mut self) {
        if let Ok(s) = std::fs::read_to_string(&self.path) {
            if let Ok(h) = serde_json::from_str::<History>(&s) {
                self.queued = h.queued;
                self.normalize_queue();
                self.tasks = h.tasks;
                self.active = h.active;
                // A run persisted as Running is stale by definition: the thread
                // that polled it died with the previous process, so it will
                // never report a final status. Restored as Running it
                // phantom-busies the whole app — the queue waits on a run that
                // cannot finish. A run still holding a question is different:
                // the loop outlives this window, so it goes on the list
                // `resume_runs` asks the service about, and its cards stay
                // answerable once the steps arrive.
                self.resume = self
                    .tasks
                    .iter()
                    .filter(|t| t.core_sid.is_some() && matches!(t.status.as_str(), "Running" | "AwaitingApproval"))
                    .filter_map(|t| t.core_sid.clone().map(|s| (t.id, s)))
                    .collect();
                for t in self.tasks.iter_mut() {
                    if t.status == "Running" { t.status = "Idle".into(); }
                }
                // Files written before ids were seeded correctly can hold
                // duplicates; identical ids render as identical a11y node ids
                // (panic in debug builds) and toggle(id) would hit every row.
                let mut next = self
                    .tasks
                    .iter()
                    .map(|t| t.id)
                    .chain(self.tasks.iter().flat_map(|t| t.msgs.iter().map(|m| m.id)))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let mut seen_tasks = std::collections::HashSet::new();
                let mut seen_msgs = std::collections::HashSet::new();
                for t in self.tasks.iter_mut() {
                    let old = t.id;
                    if !seen_tasks.insert(old) {
                        t.id = next;
                        next += 1;
                        if self.active == Some(old) {
                            self.active = Some(t.id);
                        }
                    }
                    for m in t.msgs.iter_mut() {
                        if !seen_msgs.insert(m.id) {
                            m.id = next;
                            next += 1;
                        }
                    }
                }
                self.next_id = next;
                // One delivered file, one card. History written while a delivery
                // was pushed per call holds several cards for a file the run
                // rewrote, and only the last matches what is on disk now.
                for t in self.tasks.iter_mut() {
                    let mut last_of: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
                    for m in t.msgs.iter() {
                        let (Some(path), Some(verb)) = (
                            m.meta.as_ref().and_then(|x| x.path.clone()),
                            m.meta.as_ref().and_then(|x| x.verb.clone()),
                        ) else { continue };
                        if m.kind == MsgKind::Attach && !verb.is_empty() && !path.is_empty() {
                            last_of.insert(path, m.id);
                        }
                    }
                    t.msgs.retain(|m| {
                        if m.kind != MsgKind::Attach { return true; }
                        let path = m.meta.as_ref().and_then(|x| x.path.clone()).unwrap_or_default();
                        let verb = m.meta.as_ref().and_then(|x| x.verb.clone()).unwrap_or_default();
                        if path.is_empty() || verb.is_empty() { return true; }
                        last_of.get(&path).is_none_or(|id| *id == m.id)
                    });
                    // The chip is the delivery report, so the "Delivered N
                    // files" paragraph older history saved inside the answer is
                    // trimmed here rather than shown a second time.
                    let mut hollow: Vec<usize> = Vec::new();
                    for (ri, m) in t.msgs.iter_mut().enumerate() {
                        if m.kind == MsgKind::Attach || m.kind == MsgKind::User { continue; }
                        if fennec_ui::attention::strip_delivery_prose(&mut m.content) {
                            hollow.push(ri);
                        }
                    }
                    for ri in hollow.into_iter().rev() { t.msgs.remove(ri); }
                    // A turn that wrote the call down instead of answering kept
                    // the call object as its own card. Live, the step's call list
                    // filters it; restored history has no call list, so the shape
                    // has to be read from the text — otherwise the same
                    // `{"name": …}` fence is shown over and over as an answer.
                    t.msgs.retain(|m| {
                        m.kind != MsgKind::Result
                            || !fennec_ui::attention::call_blob(&m.content)
                    });
                }
                // Drop empty drafts so a fresh install shows an empty history.
                self.tasks.retain(|t| !t.msgs.is_empty());
                if let Some(a) = self.active {
                    if !self.tasks.iter().any(|t| t.id == a) {
                        self.active = self.tasks.last().map(|t| t.id);
                    }
                }
            } else {
                // A parse failure must not present as "no history yet": the next
                // save would overwrite the file with an empty task list. Back the
                // raw bytes up and say so instead.
                let backup = self.path.with_extension("json.corrupt");
                let _ = std::fs::write(&backup, &s);
                self.toast(
                    "Saved history could not be read; the file was backed up before starting fresh.",
                    false,
                );
            }
        }
        if let Some(t) = self.task_mut() {
            if t.status == "Running" {
                t.status = "Failed".into();
                if let Some(m) = t.msgs.iter_mut().find(|m| m.kind == MsgKind::Think) {
                    m.content = "Interrupted by restart.".into();
                    m.kind = MsgKind::Result;
                }
            }
        }
        self.pending_think = None;
        self.pending_content = None;
        self.tools.clear();
        // No card of a restored history is in flight in this window, so no line
        // it reported can still be true.
        for t in &mut self.tasks {
            t.progress.clear();
        }
    }

    fn save(&self) {
        // `auto_save` off means the transcript stays in memory: the PWA does the
        // same at app.js:302, and a history file written anyway would make the
        // switch a label rather than a setting.
        if !self.auto_save {
            return;
        }
        self.save_pending.set(true);
        if self.last_save.get().elapsed() >= SAVE_INTERVAL {
            self.flush_save();
        }
    }

    fn flush_save(&self) {
        if !self.save_pending.get() {
            return;
        }
        // Empty drafts are not history: first run stays empty, real
        // conversations persist across restarts. Writes are coalesced so a
        // streamed answer does not serialize and rewrite the whole transcript
        // for every delta.
        let tasks: Vec<Task> = self.tasks.iter().filter(|t| !t.msgs.is_empty()).cloned().collect();
        let active = self.active.filter(|id| tasks.iter().any(|t| t.id == *id));
        let h = History { tasks, active, queued: self.queued.clone() };
        if let Ok(s) = serde_json::to_string_pretty(&h) {
            // Replace through a temp file + rename: a crash mid-write would
            // otherwise leave truncated JSON that the next start reads as empty.
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.save_pending.set(false);
                self.last_save.set(std::time::Instant::now());
            }
        }
    }

    fn save_settings(&self) {
        let s = Settings {
            server_url: Some(self.server_url.clone()),
            engine_url: Some(self.engine_url.clone()),
            api_key: Some(self.api_key.clone()),
            model: Some(self.model.clone()),
            access_mode: Some(self.access_mode.clone()),
            reasoning_effort: Some(self.reasoning_effort.clone()),
            theme: Some(self.theme.clone()),
            font_size: Some(self.font_size),
            auto_save: Some(self.auto_save),
            notifications: Some(self.notifications),
            allow_write: Some(self.allow_write),
            allow_shell: Some(self.allow_shell),
            allow_web: Some(self.allow_web),
            ask_each: Some(self.ask_each),
            rail_width: Some(self.rail_width),
            inspector_width: Some(self.inspector_width),
            insp_tab: Some(self.insp_tab.as_str().to_string()),
        };
        if let Ok(s) = serde_json::to_string_pretty(&s) {
            let tmp = self.settings_path.with_extension("json.tmp");
            let _ = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, &self.settings_path));
        }
    }

    fn load_workspaces(&mut self) {
        if let Ok(s) = std::fs::read_to_string(&self.workspaces_path) {
            if let Ok(store) = serde_json::from_str::<WorkspaceStore>(&s) {
                self.workspaces = store.workspaces;
                self.active_workspace_id = store.active_id;
            } else {
                // save_workspaces() below replaces an unreadable store with the
                // default workspace, so back the raw bytes up first or the
                // user's workspace bindings are lost.
                let backup = self.workspaces_path.with_extension("json.corrupt");
                let _ = std::fs::write(&backup, &s);
                self.toast(
                    "Saved workspaces could not be read; the file was backed up before starting fresh.",
                    false,
                );
            }
        }
        if self.workspaces.is_empty() {
            let name = self.suite_root.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Fennec".to_string());
            let default = Workspace::new(name);
            self.workspaces.push(default);
            self.active_workspace_id = Some(self.workspaces[0].id.clone());
            self.save_workspaces();
        }
        self.repoint_active_workspace();
        self.sync_workspace_github_repos();
    }

    fn save_workspaces(&self) {
        let store = WorkspaceStore {
            workspaces: self.workspaces.clone(),
            active_id: self.active_workspace_id.clone(),
        };
        if let Ok(s) = serde_json::to_string_pretty(&store) {
            let tmp = self.workspaces_path.with_extension("json.tmp");
            let _ = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, &self.workspaces_path));
        }
    }

    fn active_workspace(&self) -> Option<&Workspace> {
        self.active_workspace_id.as_ref()
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
    }

    fn sync_workspace_github_repos(&mut self) {
        let suite_root = self.suite_root.clone();
        let mut changed = false;
        for workspace in &mut self.workspaces {
            if workspace.github_repo.is_some() {
                continue;
            }
            let dir = workspace.source_folders.first()
                .map(PathBuf::from)
                .unwrap_or_else(|| suite_root.clone());
            if let Some(repository) = git::repo_context(&dir.to_string_lossy()).github_repo {
                workspace.github_repo = Some(repository);
                changed = true;
            }
        }
        if changed {
            self.save_workspaces();
        }
    }

    fn active_github_repo(&self) -> Option<String> {
        self.active_workspace().and_then(|workspace| workspace.github_repo.clone())
    }

    fn add_workspace(&mut self, name: String, source_folders: Vec<String>, icon: String, color: String, indexed: bool) {
        let mut ws = Workspace::new(name);
        ws.source_folders = source_folders;
        ws.icon = icon;
        ws.color = color;
        ws.indexed = indexed;
        let dir = ws.source_folders.first().map(PathBuf::from)
            .unwrap_or_else(|| self.suite_root.clone());
        ws.github_repo = git::repo_context(&dir.to_string_lossy()).github_repo;
        let id = ws.id.clone();
        self.workspaces.push(ws);
        self.active_workspace_id = Some(id);
        self.save_workspaces();
    }

    fn delete_workspace(&mut self, id: &str) {
        self.workspaces.retain(|w| w.id != id);
        self.repoint_active_workspace();
        self.save_workspaces();
    }

    /// The active id must name a workspace that exists. Deleting the active one
    /// is the obvious case, but a hand-edited or half-written store can leave
    /// the same dangling reference, and then every pane that asks for the active
    /// workspace answers "No workspace" even though the list is full.
    fn repoint_active_workspace(&mut self) {
        if self.active_workspace().is_none() {
            self.active_workspace_id = self.workspaces.first().map(|w| w.id.clone());
        }
    }

    fn select_workspace(&mut self, id: &str) {
        // The row's click handler also runs when one of its own buttons is
        // clicked, so a delete bubbles into here with an id that no longer
        // exists. Selecting it would leave the active reference dangling and
        // every pane would answer "No workspace".
        if !self.workspaces.iter().any(|w| w.id == id) {
            return;
        }
        self.active_workspace_id = Some(id.to_string());
        self.save_workspaces();
    }

    fn fetch_backend_info(&self) {
        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);
            let names = client.list_agents().iter()
                .filter_map(|v| v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect();
            let _ = tx.send(BackendResponse::BackendInfo { names });
        });
    }

    fn poll_responses(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(resp) = self.response_rx.try_recv() {
            changed = true;
            match resp {
                BackendResponse::Stt(result) => {
                    self.stt_recording = false;
                    self.line = String::new();
                    match result {
                        Ok(text) if text.is_empty() => self.toast("Didn't catch any words.", false),
                        Ok(text) => {
                            // Dictation extends what the user already typed, the
                            // same way the PWA composer does; a replaced value
                            // silently deletes the prompt half-written.
                            let prior = self.composer.read(cx).value().to_string();
                            let sep = if prior.is_empty()
                                || prior.chars().last().map_or(false, |c| c.is_whitespace())
                            { "" } else { " " };
                            self.composer_restore = Some(format!("{prior}{sep}{text}"));
                        }
                        Err(e) => self.toast(format!("Speech recognition failed: {e}"), false),
                    }
                }
                BackendResponse::BackendInfo { names } => {
                    self.agents = names;
                }
                BackendResponse::GithubRepoCreated { workspace_id, result } => {
                    match result {
                        Ok(repository) => {
                            if let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == workspace_id) {
                                workspace.github_repo = Some(repository.full_name.clone());
                                self.save_workspaces();
                                self.toast(format!("GitHub repository connected: {}", repository.full_name), true);
                            }
                        }
                        Err(error) => self.toast(format!("GitHub repository creation failed: {error}"), false),
                    }
                }
                BackendResponse::ObsidianWrite(result) => {
                    match result {
                        Ok(path) => self.toast(format!("Obsidian note written: {path}"), true),
                        Err(error) => self.toast(format!("Obsidian write failed: {error}"), false),
                    }
                }
                BackendResponse::WorkFiles { seq, result } => {
                    if seq != self.fm_seq { continue; }
                    match result {
                        Ok(listing) => {
                            self.fm.root = listing.root;
                            self.fm.rows = listing.rows;
                            self.fm.note = if self.fm.rows.is_empty() {
                                "This folder is empty.".to_string()
                            } else {
                                String::new()
                            };
                        }
                        Err(error) => self.fm.note = format!("Could not list this folder: {error}"),
                    }
                }
                BackendResponse::WorkFile { seq, name, fallback, result } => {
                    if seq != self.file_seq { continue; }
                    self.open_file = Some(match result {
                        Ok(read) => FileView {
                            path: fallback, name, body: read.body, meta: read.meta,
                            failed: false, image: read.image,
                        },
                        Err(error) => FileView {
                            path: fallback, name, body: String::new(), meta: error,
                            failed: true, image: None,
                        },
                    });
                }
                BackendResponse::Browse { seq, fallback, result } => {
                    if seq != self.br_seq { continue; }
                    match result {
                        Ok(page) => {
                            // The reader follows redirects, so the address it
                            // reports is the one actually shown — the pane's field
                            // has to say that, not what was typed.
                            self.br.target = page.url.clone();
                            self.br.title = page.title;
                            self.br.body = page.text;
                            self.br.meta = format!(
                                "{} · {}{}", page.status, page.url,
                                if page.truncated { " · truncated" } else { "" });
                            if let Some(last) = self.br.hist.last_mut() {
                                if *last == fallback { *last = self.br.target.clone(); }
                            }
                        }
                        Err(error) => {
                            self.br.title.clear();
                            self.br.body.clear();
                            self.br.meta = error;
                        }
                    }
                }
                BackendResponse::ServeState { core_up, core_root, axum, engine, branch,
                                             outlets, outlets_known, github, github_repositories,
                                             obsidian, capabilities } => {
                    self.core_up = core_up;
                    self.rust_up = axum;
                    self.core_root = core_root;
                    self.sync_workspace_github_repos();
                    self.outlets = outlets;
                    self.outlets_known = outlets_known;
                    self.github = github;
                    self.github_repositories = github_repositories;
                    self.obsidian = obsidian;
                    self.capabilities = capabilities;
                    self.engine = engine;
                    self.branch = branch;
                    // Either service answering means the composer can send;
                    // which one decides where the prompt goes.
                    self.backend_ok = axum || core_up;
                    if axum { self.fetch_backend_info(); }
                    self.flush_warm_queue();
                    // A queue read back from the history file has no finishing
                    // run to hand it off to, so it would sit at "Next" until some
                    // later run ended. Once the backend answers and nothing is
                    // live, the head goes to work — the rule the PWA applies on
                    // its health poll.
                    let live = self.tasks.iter()
                        .any(|t| matches!(t.status.as_str(), "Running" | "AwaitingApproval"));
                    if self.backend_ok && !live && !self.queued.is_empty() {
                        let next = self.queued.remove(0).text;
                        self.save();
                        self.dispatch(next, cx);
                    }
                }
                BackendResponse::Core(ev) => self.apply_core(ev, cx),
                BackendResponse::Offline => {
                    self.finish_think("Backend offline — start `fennec-agent` or set the server URL in Settings.");
                    if let Some((tid, _)) = self.pending_think { self.set_task_status(tid, "Failed"); }
                    self.pending_think = None;
                    self.pending_content = None;
                }
                BackendResponse::Error(e) => {
                    self.finish_think(&format!("Backend error: {e}"));
                    if let Some((tid, _)) = self.pending_think { self.set_task_status(tid, "Failed"); }
                    self.pending_think = None;
                    self.pending_content = None;
                }
                BackendResponse::TaskCreated { task_id, backend_id } => {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                        t.backend_id = Some(backend_id);
                    }
                }
                BackendResponse::ThinkDelta(text) => {
                    if let Some((tid, mid)) = self.pending_think {
                        if let Some(m) = self.msg_mut(tid, mid) {
                            if m.content == "Thinking…" { m.content = text; }
                            else { m.content.push_str(&text); }
                        }
                    }
                }
                BackendResponse::ContentDelta(text) => {
                    let (tid, mid) = match self.pending_content {
                        Some(pair) => pair,
                        None => {
                            let Some((tid, _)) = self.pending_think else { continue };
                            let mid = self.next_id;
                            self.next_id += 1;
                            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
                                t.msgs.push(Msg {
                                    id: mid, author: "Fennec".into(), content: String::new(),
                                    kind: MsgKind::Result, time: now(), collapsed: false, meta: None,
                                });
                            }
                            self.pending_content = Some((tid, mid));
                            self.scroll_handle.scroll_to_bottom();
                            (tid, mid)
                        }
                    };
                    if let Some(m) = self.msg_mut(tid, mid) {
                        m.content.push_str(&text);
                    }
                    self.scroll_handle.scroll_to_bottom();
                }
                BackendResponse::ActionStarted { tool_id, title } => {
                    let Some((tid, _)) = self.pending_think else { continue };
                    let mid = self.next_id;
                    self.next_id += 1;
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
                        t.msgs.push(Msg {
                            id: mid, author: "Fennec".into(), content: String::new(),
                            kind: MsgKind::Action, time: now(), collapsed: true,
                            meta: Some(MsgMeta {
                                duration: None, title: Some(title),
                                add: None, del: None, path: None, size: None, verb: None, root: None, call_id: None,
                            }),
                        });
                    }
                    self.tools.insert(tool_id, (tid, mid));
                }
                BackendResponse::ActionDone { tool_id, duration, detail } => {
                    if let Some(&(tid, mid)) = self.tools.get(&tool_id) {
                        if let Some(m) = self.msg_mut(tid, mid) {
                            // `duration` doubles as the row's done-flag: `None` is
                            // what makes it read "Running…", so a finished tool
                            // with no timing still has to be marked Some.
                            if let Some(meta) = m.meta.as_mut() { meta.duration = Some(duration.unwrap_or_default()); }
                            m.content = detail;
                        }
                    }
                    self.tools.remove(&tool_id);
                }
                BackendResponse::Complete { task_id, status, final_text } => {
                    let (think, content) = (self.pending_think, self.pending_content);
                    if let Some((tid, mid)) = think {
                        if status == "Failed" && content.is_none() && final_text.is_empty() {
                            // A failed send with nothing streamed keeps an error
                            // row in the timeline instead of vanishing.
                            self.finish_think("The engine did not respond to this task.");
                        } else {
                            // Thoughts stay in the timeline after completion; drop
                            // the row only when no thought text ever streamed in.
                            if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
                                if t.msgs.iter().any(|m| m.id == mid && m.content == "Thinking…") {
                                    t.msgs.retain(|m| m.id != mid);
                                }
                            }
                        }
                    }
                    match content {
                        Some((tid, mid)) => {
                            if !final_text.is_empty() {
                                if let Some(m) = self.msg_mut(tid, mid) { m.content = final_text; }
                            }
                        }
                        None => {
                            if !final_text.is_empty() {
                                let mid = self.next_id;
                                self.next_id += 1;
                                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                                    t.msgs.push(Msg {
                                        id: mid, author: "Fennec".into(), content: final_text,
                                        kind: MsgKind::Result, time: now(), collapsed: false, meta: None,
                                    });
                                }
                            }
                        }
                    }
                    self.set_task_status(task_id, &status);
                    self.pending_think = None;
                    self.pending_content = None;
                    self.fetch_backend_info();
                    if !self.queued.is_empty() {
                        let next = self.queued.remove(0).text;
                        self.save();
                        self.dispatch(next, cx);
                    }
                }
                BackendResponse::Failed(e) => {
                    self.finish_think(&format!("Send failed: {e}"));
                    if let Some((tid, _)) = self.pending_think { self.set_task_status(tid, "Failed"); }
                    self.pending_think = None;
                    self.pending_content = None;
                }
            }
            self.save();
        }
        if changed {
            cx.notify();
        }
    }

    fn msg_mut(&mut self, task_id: u64, msg_id: u64) -> Option<&mut Msg> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
            .and_then(|t| t.msgs.iter_mut().find(|m| m.id == msg_id))
    }

    fn finish_think(&mut self, text: &str) {
        let Some((tid, mid)) = self.pending_think else { return };
        if let Some(m) = self.msg_mut(tid, mid) {
            m.content = text.into();
            m.kind = MsgKind::Result;
            m.collapsed = false;
        }
    }

    fn set_task_status(&mut self, task_id: u64, status: &str) {
        let mut notice: Option<String> = None;
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            // One toast per run: the status is written from several paths (the SSE
            // event, the poll response, the queue drain), and a repeat of the same
            // word is not a completion.
            if t.status != status {
                t.status = status.into();
                if self.notifications && status == "Complete" {
                    notice = Some(format!("{} is complete", t.title));
                }
            }
        }
        if let Some(text) = notice {
            self.toast(text, true);
        }
    }

    fn task_mut(&mut self) -> Option<&mut Task> {
        self.active.and_then(|id| self.tasks.iter_mut().find(|t| t.id == id))
    }

    fn task(&self) -> Option<&Task> {
        self.active.and_then(|id| self.tasks.iter().find(|t| t.id == id))
    }

    fn ensure_task(&mut self) {
        if self.active.is_none() {
            let id = self.next_id;
            self.next_id += 1;
            let repo = self.repo_label();
            self.tasks.push(Task { id, title: "New task".into(), status: "Running".into(), repo, msgs: vec![], backend_id: None, started_at: None, approvals: vec![], core_sid: None, workspace_id: self.active_workspace_id.clone(), plan: vec![], progress: Default::default() });
            self.active = Some(id);
        }
    }

    fn add_msg(&mut self, m: Msg, cx: &mut Context<Self>) {
        self.ensure_task();
        if let Some(t) = self.task_mut() { t.msgs.push(m); }
        self.scroll_handle.scroll_to_bottom();
        self.save();
        cx.notify();
    }

    fn toggle(&mut self, id: u64) {
        if let Some(t) = self.task_mut() {
            if let Some(m) = t.msgs.iter_mut().find(|m| m.id == id) { m.collapsed = !m.collapsed; }
        }
        self.save();
    }

    /// What the user clicked on an action block: the opposite of what was on
    /// screen, remembered as their own override of the rule that a live block is
    /// open and a settled one is folded. A block presents several messages and
    /// owns none of them, so its state is keyed on the id of the message the run
    /// answers rather than stored in a message.
    fn toggle_block(&mut self, id: u64, shown_open: bool) {
        self.block_open.insert(id, !shown_open);
    }

    /// Show what git says about the file an edit touched. A single-file diff is
    /// normally a few milliseconds, so the fetch happens in the click handler and
    /// the dialog opens with its content already resolved.
    fn review_edit(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.work_dir().to_string_lossy().into_owned();
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
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
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

    fn new_task_in_workspace(&mut self, workspace_id: &str) {
        let id = self.next_id;
        self.next_id += 1;
        let repo = self.repo_label_for(Some(workspace_id));
        self.tasks.push(Task { id, title: "New task".into(), status: "Idle".into(), repo, msgs: vec![], backend_id: None, started_at: None, approvals: vec![], core_sid: None, workspace_id: Some(workspace_id.to_string()), plan: vec![], progress: Default::default() });
        self.active = Some(id);
        self.attention_only = false;
        self.save();
    }

    fn select_task(&mut self, id: u64) {
        self.active = Some(id);
        self.scroll_handle.scroll_to_bottom();
        self.save();
    }

    /// A run stopped on an approval is still this task's turn: the next prompt
    /// belongs behind it in the warm queue, not in a second concurrent run.
    fn working(&self) -> bool {
        self.task().map(|t| matches!(t.status.as_str(), "Running" | "AwaitingApproval")).unwrap_or(false)
    }

    /// How long this turn has been running, in whole seconds — what the folded
    /// block's timer counts.
    fn elapsed_secs(&self) -> u64 {
        self.task().and_then(|t| t.started_at)
            .map(|start| (chrono::Utc::now().timestamp() - start).max(0) as u64)
            .unwrap_or(0)
    }

    fn elapsed(&self) -> String {
        clock(self.elapsed_secs())
    }

    /// Skills the run reached for beyond the tools it always has. Reading,
    /// writing, searching and running a command are on from the first token, so
    /// listing them as skills would say nothing.
    fn skills(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        if let Some(t) = self.task() {
            for m in &t.msgs {
                if m.kind == MsgKind::Action {
                    if let Some(meta) = &m.meta {
                        if let Some(title) = &meta.title {
                            if !fennec_ui::inspector::title_is_default(title) && !out.contains(title) {
                                out.push(title.clone());
                            }
                        }
                    }
                }
            }
        }
        out
    }

    fn sources(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        if let Some(t) = self.task() {
            for m in &t.msgs {
                let p = match &m.kind {
                    MsgKind::Edit => m.meta.as_ref().and_then(|x| x.path.clone()),
                    MsgKind::Attach => m.meta.as_ref().and_then(|x| x.path.clone()),
                    _ => None,
                };
                if let Some(p) = p {
                    if !out.contains(&p) { out.push(p); }
                }
            }
        }
        out
    }

    /// A task is the user's business while it has a question they can still
    /// answer, or a status that says it stopped to wait for one. An expired
    /// question is not attention — there is nothing left to do with it.
    fn task_needs_attention(&self, t: &Task) -> bool {
        let pending = t.approvals.iter().filter(|a| a.resolved.is_none() && !a.expired).count();
        needs_attention(&t.status, pending)
    }

    fn attention_count(&self) -> usize {
        self.tasks.iter().filter(|t| self.task_needs_attention(t)).count()
    }

    fn approved_commands(&self) -> Vec<ApprovedCommand> {
        self.tasks.iter()
            .flat_map(|t| t.approvals.iter().filter_map(move |a| {
                a.resolved.as_ref().map(|r| (t, a, r.choice_id.clone()))
            }))
            .map(|(t, a, decision)| ApprovedCommand {
                task_id: t.id.to_string(),
                task_title: t.title.clone(),
                command: gate_title(&a.action, a.command.as_deref()),
                risk: a.risk_level.clone(),
                decision,
            })
            .collect()
    }

    /// What the rail lists right now. `query_lower` is the search text already
    /// downcased; My Attention and Search each narrow the list their own way.
    fn task_shown(&self, t: &Task, query_lower: &str) -> bool {
        (!self.attention_only || self.task_needs_attention(t))
            && (query_lower.is_empty()
                || t.title.to_lowercase().contains(query_lower)
                || t.repo.to_lowercase().contains(query_lower))
    }

    fn filtered_tasks(&self, query: &str) -> Vec<&Task> {
        let q = query.to_lowercase();
        self.tasks.iter()
            .filter(|t| self.task_shown(t, &q))
            .rev()
            .collect()
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.tasks.clear();
        self.active = None;
        self.queued.clear();
        self.next_id = 1;
        self.pending_think = None;
        self.pending_content = None;
        self.tools.clear();
        self.save();
        cx.notify();
    }

    fn set_access_mode(&mut self, mode: &str) {
        self.access_mode = mode.to_string();
        match mode {
            "auto" => { self.allow_write = true; self.allow_shell = true; self.allow_web = true; self.ask_each = false; }
            "full" => { self.allow_write = true; self.allow_shell = true; self.allow_web = true; self.ask_each = false; }
            _ => { self.allow_write = false; self.allow_shell = false; self.allow_web = false; self.ask_each = true; }
        }
        self.save_settings();
    }

    fn access_label(&self) -> &'static str {
        fennec_ui::controls::access_label(&self.access_mode)
    }

    fn access_icon(&self) -> &'static str {
        fennec_ui::controls::access_icon(&self.access_mode)
    }

    fn access_color(&self, t: &gpui_kit::component::Theme) -> gpui::Hsla {
        fennec_ui::controls::access_color(&self.access_mode, t)
    }

    fn model_display_name(&self) -> String {
        fennec_ui::controls::model_display_name(self.active_model())
    }

    /// The checkpoint the engine proved it holds. Fennec serves one model and
    /// answers every request as it, so an id the health probe did not report
    /// would name a model nothing is running.
    fn served_model(&self) -> Option<&str> {
        if self.engine.up && !self.engine.model.is_empty() {
            Some(self.engine.model.as_str())
        } else {
            None
        }
    }

    fn active_model(&self) -> &str {
        self.served_model().unwrap_or(&self.model)
    }

    fn toggle_stt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stt_recording {
            self.stt_stop.store(true, Ordering::Relaxed);
            self.stt_stream = None;
            self.stt_recording = false;
            self.line = "Transcribing…".to_string();
            cx.notify();
            return;
        }
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let device = match host.default_input_device() {
            Some(d) => d,
            None => { window.push_notification(Notification::info("No microphone found."), cx); return; }
        };
        let config = match device.default_input_config() {
            Ok(c) => c,
            Err(e) => { window.push_notification(Notification::info(format!("Mic config error: {e}")), cx); return; }
        };
        let rate = config.sample_rate().0;
        self.stt_rate.store(rate, Ordering::Relaxed);
        self.stt_buffer.lock().unwrap().clear();
        self.stt_stop.store(false, Ordering::Relaxed);
        let channels = config.channels() as usize;
        let max_samples = rate as usize * MAX_STT_SECONDS;
        let buffer = self.stt_buffer.clone();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(&config.into(), move |data: &[f32], _| {
                let mut b = buffer.lock().unwrap();
                for frame in data.chunks(channels) {
                    if b.len() >= max_samples { break; }
                    b.push(frame[0]);
                }
            }, |e| eprintln!("stt error: {e}"), None),
            cpal::SampleFormat::I16 => {
                let buffer = self.stt_buffer.clone();
                device.build_input_stream(&config.into(), move |data: &[i16], _| {
                    let mut b = buffer.lock().unwrap();
                    for frame in data.chunks(channels) {
                        if b.len() >= max_samples { break; }
                        b.push(frame[0] as f32 / 32768.0);
                    }
                }, |e| eprintln!("stt error: {e}"), None)
            },
            _ => { window.push_notification(Notification::info("Unsupported mic format."), cx); return; }
        };
        let stream = match stream { Ok(s) => s, Err(e) => { window.push_notification(Notification::info(format!("Mic open error: {e}")), cx); return; } };
        if stream.play().is_err() { window.push_notification(Notification::info("Could not start mic."), cx); return; }
        self.stt_recording = true;
        self.stt_stream = Some(stream);
        self.line = "Recording… click the mic again to stop and transcribe.".to_string();
        cx.notify();
        let stop = self.stt_stop.clone();
        let buffer = self.stt_buffer.clone();
        let rate_arc = self.stt_rate.clone();
        let core_base = self.core.base.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) { std::thread::sleep(std::time::Duration::from_millis(50)); }
            let samples = buffer.lock().unwrap().clone();
            let rate = rate_arc.load(Ordering::Relaxed);
            // Whisper answers an empty clip with HTTP 200 and no text, so without
            // this the user sees "Didn't catch any words" for a muted or wrong
            // input device and blames the recogniser.
            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            if peak < 0.001 {
                let _ = tx.send(BackendResponse::Stt(Err(format!(
                    "the microphone captured silence ({} samples at {} Hz, peak {peak:.5}) — check the input device and its volume",
                    samples.len(), rate))));
                return;
            }
            let text = fennec_ui::controls::transcribe_samples(&samples, rate, &core_base);
            // Whisper answers a near-inaudible clip with HTTP 200 and no text.
            // That is a level problem, not the recogniser's, so report the
            // number that proves it instead of "Didn't catch any words".
            let text = match text {
                Ok(words) if words.is_empty() && peak < 0.02 => Err(format!(
                    "the microphone barely heard anything (peak {peak:.4} at {rate} Hz) — raise the input level or speak closer",
                )),
                other => other,
            };
            let _ = tx.send(BackendResponse::Stt(text));
        });
    }

    fn normalize_queue(&mut self) {
        let mut agent = Vec::new();
        let mut user = Vec::new();
        for item in self.queued.drain(..) {
            match item.owner {
                QueueOwner::Agent => agent.push(item),
                QueueOwner::User => user.push(item),
            }
        }
        agent.extend(user);
        self.queued = agent;
    }

    fn enqueue_user(&mut self, text: String) {
        self.queued.push(QueueEntry { text, owner: QueueOwner::User });
        self.normalize_queue();
        self.save();
    }

    fn enqueue_agent(&mut self, items: Vec<String>) {
        self.normalize_queue();
        let mut incoming = Vec::new();
        for text in items {
            if !text.is_empty() && !self.queued.iter().any(|item| item.text == text) {
                incoming.push(QueueEntry { text, owner: QueueOwner::Agent });
            }
        }
        if !incoming.is_empty() {
            let at = self.queued.iter().position(|item| item.owner == QueueOwner::User)
                .unwrap_or(self.queued.len());
            self.queued.splice(at..at, incoming);
            self.save();
        }
    }

    fn move_user_queue_item(&mut self, idx: usize, direction: isize) {
        if self.queued.get(idx).is_none_or(|item| item.owner != QueueOwner::User) { return; }
        let target = if direction < 0 {
            (0..idx).rev().find(|&i| self.queued[i].owner == QueueOwner::User)
        } else {
            ((idx + 1)..self.queued.len()).find(|&i| self.queued[i].owner == QueueOwner::User)
        };
        if let Some(target) = target {
            self.queued.swap(idx, target);
            self.save();
        }
    }

    fn edit_queue_item(&mut self, idx: usize, text: String) {
        if let Some(item) = self.queued.get_mut(idx) {
            if item.owner == QueueOwner::User && !text.trim().is_empty() {
                item.text = text.trim().to_string();
                self.save();
            }
        }
    }

    fn remove_from_queue(&mut self, idx: usize) {
        if self.queued.get(idx).is_some_and(|item| item.owner == QueueOwner::User) {
            self.queued.remove(idx);
            self.save();
        }
    }

    fn open_edit_queue_dialog(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.queued.get(idx) else { return };
        if item.owner != QueueOwner::User { return; }
        let initial_text = item.text.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial_text, window, cx);
            state
        });
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let input = input.clone();
            let weak = weak.clone();
            dialog.title("Edit queued task")
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                .content({
                let input = input.clone();
                let weak = weak.clone();
                move |content, _, _| {
                    let save_input = input.clone();
                    let save_weak = weak.clone();
                    content.flex().flex_col().gap_4()
                        .child(div().flex().flex_col().gap_1()
                            .child(Label::new("Task").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                            .child(Input::new(&input).h(px(32.0))))
                        .child(div().flex().items_center().justify_end().gap_2()
                            .child(Button::new("queue-edit-cancel").label("Cancel").ghost().small()
                                .on_click(|_, window, cx| window.close_dialog(cx)))
                            .child(Button::new("queue-edit-save").label("Save").small()
                                .on_click(move |_, window, cx| {
                                    if let Some(app) = save_weak.upgrade() {
                                        let text = save_input.read(cx).value().trim().to_string();
                                        app.update(cx, |a, cx| {
                                            a.edit_queue_item(idx, text);
                                            cx.notify();
                                        });
                                    }
                                    window.close_dialog(cx);
                                })))
                }
            }).on_close(|_, window, cx| window.close_dialog(cx))
        });
    }

    fn create_github_repository(&self, workspace_id: String, name: String) {
        let base = self.server_url.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let result = CodingAgentClient::with_base(base).create_github_repository(&name, true);
            let _ = tx.send(BackendResponse::GithubRepoCreated { workspace_id, result });
        });
    }

    fn open_repository_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace_id) = self.active_workspace_id.clone() else { return };
        let repositories: Vec<fennec_ui::repository::RepositoryEntry> = self
            .github_repositories
            .repositories
            .iter()
            .map(|repository| fennec_ui::repository::RepositoryEntry {
                full_name: repository.full_name.clone(),
                private: repository.private,
            })
            .collect();
        let current = self.active_github_repo();
        let select_app = cx.entity().downgrade();
        let create_app = select_app.clone();
        let select_ws = workspace_id.clone();
        fennec_ui::repository::open_repository_dialog(
            window,
            cx,
            repositories,
            current,
            move |full_name, cx| {
                if let Some(app) = select_app.upgrade() {
                    let _ = app.update(cx, |app, cx| {
                        if let Some(workspace) = app.workspaces.iter_mut().find(|w| w.id == select_ws) {
                            workspace.github_repo = Some(full_name);
                            app.save_workspaces();
                            cx.notify();
                        }
                    });
                }
            },
            move |name, cx| {
                if let Some(app) = create_app.upgrade() {
                    let _ = app.update(cx, |app, _| {
                        app.create_github_repository(workspace_id.clone(), name);
                        app.toast("Creating GitHub repository…", true);
                    });
                }
            },
        );
    }

    fn queue_action(&mut self, action: QueueAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            QueueAction::Edit(idx) => self.open_edit_queue_dialog(idx, window, cx),
            QueueAction::MoveUp(idx) => self.move_user_queue_item(idx, -1),
            QueueAction::MoveDown(idx) => self.move_user_queue_item(idx, 1),
            QueueAction::Remove(idx) => self.remove_from_queue(idx),
        }
    }

    fn delete_task(&mut self, id: u64) {
        self.tasks.retain(|t| t.id != id);
        if self.active == Some(id) {
            self.active = self.tasks.last().map(|t| t.id);
        }
        self.save();
    }

    fn confirm_delete_task(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.tasks.iter().find(|t| t.id == id).map(|t| t.title.clone()).unwrap_or_default();
        let weak = cx.entity().downgrade();
        open_confirm(window, cx, Confirm {
            title: "Delete Task",
            detail: "The task and everything it did are removed from this device. This can't be undone.",
            subject: title,
            action: "Delete",
            danger: true,
        }, move |cx| {
            let Some(app) = weak.upgrade() else { return };
            let _ = app.update(cx, |a, cx| {
                a.delete_task(id);
                cx.notify();
            });
        });
    }

    fn rename_task(&mut self, id: u64, new_title: String) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.title = new_title;
            self.save();
        }
    }

    fn compact_active(&mut self) {
        let Some(task) = self.tasks.iter().find(|t| Some(t.id) == self.active) else { return };
        let msgs: Vec<serde_json::Value> = task.msgs.iter()
            .filter(|m| matches!(m.kind, MsgKind::User | MsgKind::Result))
            .map(|m| serde_json::json!({"role": if m.kind == MsgKind::User { "user" } else { "assistant" }, "content": m.content.clone()}))
            .collect();
        let opts = fennec_economy::Options::sized(fennec_economy::DEFAULT_BUDGET_CHARS, fennec_economy::DEFAULT_RESULT_CHARS);
        let (_payload, stats) = fennec_economy::compact(&msgs, &opts, &mut std::collections::HashMap::new());
        self.line = format!("Compacted {} → {} chars ({} elided, {} deduped).",
            stats.chars_before, stats.chars_after, stats.elided, stats.deduped);
    }

    fn toast(&mut self, text: impl Into<String>, good: bool) {
        self.toasts.push(Toast { text: text.into(), good, at: std::time::Instant::now() });
        if self.toasts.len() > 4 {
            let extra = self.toasts.len() - 4;
            self.toasts.drain(..extra);
        }
    }

    fn flush_warm_queue(&mut self) {
        let Some((text, queued)) = self.pending_send.take() else { return };
        if self.backend_ok {
            self.send_text(text);
        } else if queued.elapsed() < WARM_WINDOW {
            self.pending_send = Some((text, queued));
        } else {
            self.line = "The backend did not answer within the warm-up window. \
                The queued prompt has been discarded."
                .to_string();
        }
    }

    fn send_text(&mut self, text: String) {
        if text.is_empty() { return; }
        let task_id = self.next_id;
        self.next_id += 1;
        let repo = self.repo_label();
        self.tasks.push(Task {
            id: task_id,
            title: text.chars().take(40).collect(),
            status: "Running".into(),
            repo,
            msgs: vec![Msg {
                id: 1,
                author: "You".into(),
                content: text.clone(),
                kind: MsgKind::User,
                time: now(),
                collapsed: false,
                meta: None,
            }],
            backend_id: None,
            started_at: Some(chrono::Utc::now().timestamp()),
            approvals: vec![],
            core_sid: None,
            workspace_id: self.active_workspace_id.clone(),
            plan: vec![],
            progress: Default::default(),
        });
        self.active = Some(task_id);
        self.save();
    }

    fn open_rename_task_dialog(&mut self, task_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.tasks.iter().find(|t| t.id == task_id).map(|t| t.title.clone()).unwrap_or_default();
        let name_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(title, window, cx);
            s
        });
        let weak = cx.entity().downgrade();

        window.open_dialog(cx, move |dialog, _, _| {
            let name_input = name_input.clone();
            let weak = weak.clone();
            dialog
                .title("Rename Task")
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                .content({
                    let name_input = name_input.clone();
                    let weak = weak.clone();
                    move |content, _window, _cx| {
                        content
                            .flex().flex_col().gap_4()
                            .child(div().flex().flex_col().gap_1()
                                .child(Label::new("Task name").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                .child(Input::new(&name_input).h(px(32.0))))
                            .child(div().flex().items_center().justify_end().gap_2()
                                .child(Button::new("rename-cancel").label("Cancel").ghost().small()
                                    .on_click({
                                        move |_, window, cx| {
                                            window.close_dialog(cx);
                                        }
                                    }))
                                .child(Button::new("rename-save").label("Save").small()
                                    .on_click({
                                        let name_input = name_input.clone();
                                        let weak = weak.clone();
                                        move |_, window, cx| {
                                            if let Some(app) = weak.upgrade() {
                                                let new_title = name_input.read(cx).value().trim().to_string();
                                                if !new_title.is_empty() {
                                                    app.update(cx, |a, cx| {
                                                        a.rename_task(task_id, new_title);
                                                        cx.notify();
                                                    });
                                                }
                                            }
                                            window.close_dialog(cx);
                                        }
                                    }))
                            )
                    }
                })
                .on_close(move |_, window, cx| {
                    window.close_dialog(cx);
                })
        });
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() { return; }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        if self.working() {
            self.enqueue_user(text);
            cx.notify();
            return;
        }
        // A prompt needs somewhere to go. The services are what gate calls and
        // write files; the engine alone answers nothing but a question, so it
        // counts as somewhere to go, and the rail keeps reporting the backend
        // offline instead of pretending a chat is a run.
        if !self.backend_ok && !self.engine.up {
            if self.pending_send.is_some() {
                self.line = "The backend is still warming and it already holds a prompt. \
                    This one stays in the composer until that one sends."
                    .to_string();
                self.composer.update(cx, |state, cx| state.set_value(&text, window, cx));
                return;
            }
            self.pending_send = Some((text, std::time::Instant::now()));
            self.line = "Backend is offline - prompt queued. It will send once the backend answers.".to_string();
            cx.notify();
            return;
        }
        self.dispatch(text, cx);
    }

    fn dispatch(&mut self, text: String, cx: &mut Context<Self>) {
        self.ensure_task();
        let backend_id = self.task().and_then(|t| t.backend_id.clone());
        let task_id = self.active.unwrap_or(0);
        let first_prompt = self.task().map(|t| t.title == "New task").unwrap_or(true);

        let uid = self.next_id; self.next_id += 1;
        self.add_msg(Msg { id: uid, author: "You".into(), content: text.clone(), kind: MsgKind::User, time: now(), collapsed: false, meta: None }, cx);

        // The files the picker handed over get cards of their own, under the
        // prompt they belong to. They carry no verb — the run did not make them,
        // the user did — and their path is the real one the chooser returned, so
        // the card opens the file on disk instead of a copy of its bytes.
        let atts = std::mem::take(&mut self.pending_atts);
        for path in &atts {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let id = self.next_id; self.next_id += 1;
            self.add_msg(Msg {
                id, author: "You".into(), content: String::new(), kind: MsgKind::Attach,
                time: now(), collapsed: false,
                meta: Some(MsgMeta {
                    duration: None, title: Some(name), add: None, del: None,
                    path: Some(path.to_string_lossy().into_owned()),
                    size: Some(human_size(size)), verb: None, root: None, call_id: None,
                }),
            }, cx);
        }

        if first_prompt {
            if let Some(t) = self.task_mut() { t.title = text.clone(); }
        }
        if let Some(t) = self.task_mut() {
            t.status = "Running".into();
            t.started_at = Some(chrono::Utc::now().timestamp());
        }

        let think_id = self.next_id; self.next_id += 1;
        self.add_msg(Msg { id: think_id, author: "Fennec".into(), content: "Thinking…".into(), kind: MsgKind::Think, time: now(), collapsed: true, meta: None }, cx);

        self.pending_think = Some((task_id, think_id));
        self.pending_content = None;
        self.save();
        cx.notify();

        // The picked files belong to the prompt being sent, not to every prompt
        // after it. A text file always travels as text. An image travels two
        // ways: the agent loop accepts only a string, so it gets the data URL
        // inlined, while the engine gets a real image part — the only shape
        // whose pixels reach the model. An engine that has said it is text-only
        // gets the name and the size, never a base64 wall it would refuse.
        let prompt = text;
        let mut notes = String::new();
        let mut urls = String::new();
        let mut images: Vec<serde_json::Value> = Vec::new();
        for path in &atts {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let mime = attachment_mime(path);
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let shown = format!("{name} ({})", human_size(size));
            if mime.starts_with("image/") {
                if self.engine.vision == Some(false) {
                    notes.push_str(&format!("\n\n[attached image {shown} — the engine reports no image input, so this file's pixels cannot reach the model]"));
                } else if let Ok(bytes) = std::fs::read(path) {
                    use base64::Engine as _;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    notes.push_str(&format!("\n\n[attached image {shown}]"));
                    urls.push_str(&format!("\ndata:{mime};base64,{b64}"));
                    images.push(serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{mime};base64,{b64}") }
                    }));
                }
            } else if let Ok(content) = std::fs::read_to_string(path) {
                notes.push_str(&format!("\n\n[attached file {shown}]\n{content}"));
            }
        }
        let text = format!("{prompt}{notes}{urls}");

        // The Rust task service is canonical for new prompts. The legacy core
        // lane remains available only when the Rust service is not running.
        if self.rust_up {
            self.dispatch_rust(text, task_id, backend_id);
            return;
        }
        if self.core_up {
            self.dispatch_core(text, task_id);
            return;
        }
        // With neither service answering, the engine alone can still reply. It
        // fronts every run, so a bare engine is a degraded but honest chat: no
        // tools, no gating, no files — and the rail keeps reporting the backend
        // offline rather than pretending otherwise.
        if !self.backend_ok && self.engine.up {
            self.line = "Only the engine answers here: this is a chat, so nothing runs or writes files until an agent service is up.".to_string();
            let spoken = format!("{prompt}{notes}");
            self.dispatch_engine(self.engine_turn(uid, &spoken, images), task_id);
            return;
        }

        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);

            if client.health().is_none() {
                let _ = tx.send(BackendResponse::Offline);
                return;
            }

            let bt = match backend_id {
                Some(id) if client.send_message(&id, &text).is_ok() => id,
                _ => {
                    let task = match client.create_task(&text, Some(&text), "local", None) {
                        Ok(t) => t,
                        Err(e) => { let _ = tx.send(BackendResponse::Error(e)); return; }
                    };
                    let id = task.id.clone();
                    let _ = tx.send(BackendResponse::TaskCreated { task_id, backend_id: id.clone() });
                    if let Err(e) = client.start_task(&id) {
                        let _ = tx.send(BackendResponse::Error(e));
                        return;
                    }
                    if let Err(e) = client.send_message(&id, &text) {
                        let _ = tx.send(BackendResponse::Failed(e));
                        return;
                    }
                    id
                }
            };

            // SSE reader: live reasoning/content/tool deltas for this task.
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let cancel = cancel.clone();
                let bt = bt.clone();
                let tx = tx.clone();
                let sse_client = client.clone();
                std::thread::spawn(move || {
                    let _ = sse_client.subscribe_events(&cancel, move |ev| {
                        if ev.task_id.as_deref() != Some(bt.as_str()) { return; }
                        match ev.event_type.as_str() {
                            "MessageStreamed" => {
                                let Some(payload) = &ev.payload else { return };
                                let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                if text.is_empty() { return; }
                                match payload.get("kind").and_then(|k| k.as_str()) {
                                    Some("reasoning") => { let _ = tx.send(BackendResponse::ThinkDelta(text.to_string())); }
                                    Some("content") => { let _ = tx.send(BackendResponse::ContentDelta(text.to_string())); }
                                    _ => {}
                                }
                            }
                            "ToolStarted" => {
                                let title = ev.payload.as_ref()
                                    .and_then(|p| p.get("name").and_then(|n| n.as_str()))
                                    .unwrap_or("Working").to_string();
                                let tool_id = ev.payload.as_ref()
                                    .and_then(|p| p.get("id").and_then(|n| n.as_str()))
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| ev.event_id.clone());
                                let _ = tx.send(BackendResponse::ActionStarted { tool_id, title });
                            }
                            "ToolCompleted" | "ToolProgressUpdated" => {
                                let Some(payload) = &ev.payload else { return };
                                let tool_id = payload.get("id").and_then(|n| n.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| ev.event_id.clone());
                                let detail = payload.get("result").and_then(|v| v.as_str())
                                    .or_else(|| payload.get("detail").and_then(|v| v.as_str()))
                                    .unwrap_or("").to_string();
                                let duration = payload.get("duration_ms").and_then(|v| v.as_u64())
                                    .map(|ms| format!("{ms} ms"));
                                let _ = tx.send(BackendResponse::ActionDone { tool_id, duration, detail });
                            }
                            _ => {}
                        }
                    });
                });
            }

            // Terminal watcher: authoritative status + final message.
            let mut attempts = 0usize;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                attempts += 1;
                if let Some(task) = client.get_task(&bt) {
                    if task.status == "Complete" || task.status == "Failed" {
                        cancel.store(true, Ordering::Relaxed);
                        let final_text = task.messages.iter().rev()
                            .find(|m| m.role == "Fennec")
                            .map(|m| m.content.as_text())
                            .unwrap_or_default();
                        let _ = tx.send(BackendResponse::Complete { task_id, status: task.status, final_text });
                        return;
                    }
                }
                if attempts > 900 {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = tx.send(BackendResponse::Complete {
                        task_id, status: "Failed".into(),
                        final_text: "Timed out waiting for the backend to finish.".into(),
                    });
                    return;
                }
            }
        });
    }

    fn dispatch_rust(&self, text: String, task_id: u64, backend_id: Option<String>) {
        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);
            if client.health().is_none() {
                let _ = tx.send(BackendResponse::Offline);
                return;
            }

            let existing = backend_id.filter(|id| client.get_task(id).is_some());
            let (backend_task_id, created) = match existing {
                Some(id) => {
                    if let Err(error) = client.send_message(&id, &text) {
                        let _ = tx.send(BackendResponse::Error(error));
                        return;
                    }
                    (id, false)
                }
                None => {
                    let task = match client.create_task(&text, Some(&text), "local", None) {
                        Ok(task) => task,
                        Err(error) => {
                            let _ = tx.send(BackendResponse::Error(error));
                            return;
                        }
                    };
                    let id = task.id.clone();
                    if let Err(error) = client.start_task(&id) {
                        let _ = tx.send(BackendResponse::Error(error));
                        return;
                    }
                    if let Err(error) = client.send_message(&id, &text) {
                        let _ = tx.send(BackendResponse::Error(error));
                        return;
                    }
                    (id, true)
                }
            };
            if created {
                let _ = tx.send(BackendResponse::TaskCreated {
                    task_id,
                    backend_id: backend_task_id.clone(),
                });
            }

            let cancel = Arc::new(AtomicBool::new(false));
            {
                let cancel = cancel.clone();
                let tx = tx.clone();
                let stream_client = client.clone();
                let stream_task_id = backend_task_id.clone();
                std::thread::spawn(move || {
                    let _ = stream_client.subscribe_events(&cancel, move |event| {
                        if event.task_id.as_deref() != Some(stream_task_id.as_str()) {
                            return;
                        }
                        let Some(payload) = event.payload.as_ref() else { return };
                        match event.event_type.as_str() {
                            "MessageStreamed" => {
                                let Some(text) = payload.get("text").and_then(|v| v.as_str()) else { return };
                                if text.is_empty() { return; }
                                match payload.get("kind").and_then(|v| v.as_str()) {
                                    Some("reasoning") => { let _ = tx.send(BackendResponse::ThinkDelta(text.to_string())); }
                                    Some("content") => { let _ = tx.send(BackendResponse::ContentDelta(text.to_string())); }
                                    _ => {}
                                }
                            }
                            "TaskStatusChanged" if payload.get("status").and_then(|v| v.as_str()) == Some("complete") => {}
                            "ToolStarted" => {
                                let tool_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or(&event.event_id).to_string();
                                let title = payload.get("name").and_then(|v| v.as_str()).unwrap_or("Working").to_string();
                                let _ = tx.send(BackendResponse::ActionStarted { tool_id, title });
                            }
                            "ToolCompleted" | "ToolProgressUpdated" => {
                                let tool_id = payload.get("id").and_then(|v| v.as_str()).unwrap_or(&event.event_id).to_string();
                                let detail = payload.get("result").and_then(|v| v.as_str())
                                    .or_else(|| payload.get("detail").and_then(|v| v.as_str()))
                                    .unwrap_or("").to_string();
                                let duration = payload.get("duration_ms").and_then(|v| v.as_u64()).map(|ms| format!("{ms} ms"));
                                let _ = tx.send(BackendResponse::ActionDone { tool_id, duration, detail });
                            }
                            _ => {}
                        }
                    });
                });
            }

            let mut attempts = 0usize;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(350));
                attempts += 1;
                if let Some(task) = client.get_task(&backend_task_id) {
                    if matches!(task.status.as_str(), "Complete" | "Failed" | "Cancelled") {
                        cancel.store(true, Ordering::Relaxed);
                        let final_text = task.messages.iter().rev()
                            .find(|message| message.role == "Fennec")
                            .map(|message| message.content.as_text())
                            .unwrap_or_default();
                        let _ = tx.send(BackendResponse::Complete { task_id, status: task.status, final_text });
                        return;
                    }
                }
                if attempts > 900 {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = tx.send(BackendResponse::Complete {
                        task_id,
                        status: "Failed".into(),
                        final_text: "Timed out waiting for the Rust backend to finish.".into(),
                    });
                    return;
                }
            }
        });
    }

    /// The directory to hand the loop for the active task. The loop refuses
    /// (403) anything outside the root it was started with, so a workspace
    /// folder it cannot reach is not offered — its own root is, which says where
    /// the run can actually work instead of where a rail shows a picture.
    fn core_workspace(&self) -> String {
        self.core_workspace_for(self.active_workspace_id.as_deref())
    }

    fn core_workspace_for(&self, workspace_id: Option<&str>) -> String {
        let root = self.core_root.trim_end_matches(['\\', '/']).to_string();
        let folder = workspace_id
            .and_then(|id| self.workspaces.iter().find(|w| &w.id == id))
            .and_then(|w| w.source_folders.first())
            .map(|s| s.trim_end_matches(['\\', '/']).to_string())
            .unwrap_or_default();
        let reachable = !root.is_empty()
            && folder.to_lowercase().starts_with(&root.to_lowercase());
        if reachable || root.is_empty() { folder } else { root }
    }

    /// Directory the runs work in: the workspace's folder, else the loop's
    /// own workspace, else the suite checkout. Every label that names a repo is
    /// derived from here so the rail cannot claim a project the app is not in.
    fn work_dir_for(&self, workspace_id: Option<&str>) -> PathBuf {
        let ws = self.core_workspace_for(workspace_id);
        if !ws.is_empty() { return PathBuf::from(ws); }
        self.suite_root.clone()
    }

    /// Name shown where a full path would be too wide to read.
    fn repo_label_for(&self, workspace_id: Option<&str>) -> String {
        let dir = self.work_dir_for(workspace_id);
        dir.file_name().map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "—".to_string())
    }

    fn work_dir(&self) -> PathBuf {
        self.work_dir_for(self.active_workspace_id.as_deref())
    }

    fn repo_label(&self) -> String {
        self.repo_label_for(self.active_workspace_id.as_deref())
    }

    /// Start a supervised run on the agent loop and follow it. This is the lane
    /// that gates: `loop.py` lifts the `allow_*` switches and stops before every
    /// write, so what admits a call is the answer on the card, not a box ticked
    /// when the run started.
    fn dispatch_core(&self, text: String, task_id: u64) {
        let (core, tx) = (self.core.clone(), self.response_tx.clone());
        let workspace = self.core_workspace();
        let (allow_write, allow_shell, allow_web) = (self.allow_write, self.allow_shell, self.allow_web);
        let ask = self.ask_each || self.access_mode == "ask";
        let effort = self.reasoning_effort.clone();
        std::thread::spawn(move || {
            let sid = match core.start(&text, &workspace, allow_write, allow_shell, allow_web, ask, &effort) {
                Ok(sid) => sid,
                Err(e) => {
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Refused { task_id, error: e }));
                    return;
                }
            };
            let _ = tx.send(BackendResponse::Core(CoreEvent::Started { task_id, sid: sid.clone() }));
            Self::poll_core(core, tx, sid, task_id, workspace, false);
        });
    }

    /// Follow a supervised run: pull the steps the loop kept, say which questions
    /// it still holds, and stop when the run is no longer running. It serves a
    /// task started here and one the service already knew about, which is why
    /// restarting this window does not strand a run that outlived it.
    /// `replay` means the transcript on screen is stale and the steps sent now
    /// replace it — only once the service has actually answered, so a session
    /// that is gone leaves the history it left behind rather than an empty row.
    fn poll_core(core: Core, tx: std::sync::mpsc::Sender<BackendResponse>, sid: String,
                 task_id: u64, root: String, replay: bool) {
        let mut after = 0usize;
        let mut replay = replay;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(350));
            let v = match core.poll(&sid, after) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Refused { task_id, error: e }));
                    return;
                }
            };
            if replay {
                replay = false;
                let _ = tx.send(BackendResponse::Core(CoreEvent::Replay { task_id }));
            }
            let steps = v["steps"].as_array().cloned().unwrap_or_default();
            // Steps first: a card this round sent has to exist before the
            // pending list says which cards are live.
            for s in &steps {
                translate_step(&tx, task_id, s, &root);
            }
            if !steps.is_empty() {
                after += steps.len();
            }
            // `pending` is what the loop still holds. A service that reports
            // no list is not the same as an empty one — except once the run
            // is over, when it cannot be waiting on anyone.
            let finished = v["status"].as_str().map(|s| s != "running").unwrap_or(true);
            match (v["pending"].as_array(), finished) {
                (Some(list), _) => {
                    let ids = list.iter().filter_map(serde_json::Value::as_str)
                        .map(str::to_string).collect::<Vec<String>>();
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Pending { task_id, ids }));
                }
                (None, true) => {
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Pending { task_id, ids: vec![] }));
                }
                (None, false) => {}
            }
            if finished {
                let status = v["status"].as_str().unwrap_or("done").to_string();
                let _ = tx.send(BackendResponse::Core(CoreEvent::Status { task_id, status }));
                return;
            }
        }
    }

    /// The agent loop keeps working when this window closes, so a run restored
    /// from history may still be live. Ask the service which of those sessions
    /// it still holds and pick them back up; without this, a run that was
    /// thinking at shutdown freezes mid-thought and the question it stopped on
    /// has no card left to answer from. Bounded, because a service that never
    /// answers is not a run to resume — the history stays as it was.
    fn resume_runs(&self) {
        if self.resume.is_empty() {
            return;
        }
        let want = self.resume.clone();
        let (core, tx) = (self.core.clone(), self.response_tx.clone());
        std::thread::spawn(move || {
            for _ in 0..8 {
                if core.health().is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    continue;
                }
                let live = core.sessions();
                for (task_id, sid) in want {
                    if live.iter().any(|s| s["id"].as_str() == Some(sid.as_str())) {
                        let _ = tx.send(BackendResponse::Core(CoreEvent::Resume { task_id, sid }));
                    }
                }
                return;
            }
        });
    }

    /// The conversation as the engine sees it: what was said, in order, with
    /// this prompt last. The calls and the thoughts are this window's
    /// bookkeeping — the model never saw them, so neither does it hear them.
    fn engine_turn(&self, skip_uid: u64, prompt: &str, images: Vec<serde_json::Value>) -> serde_json::Value {
        let mut turns: Vec<serde_json::Value> = Vec::new();
        if let Some(t) = self.task() {
            for m in &t.msgs {
                if m.id == skip_uid { continue; }
                let role = match m.kind {
                    MsgKind::User => "user",
                    MsgKind::Result => "assistant",
                    _ => continue,
                };
                turns.push(serde_json::json!({ "role": role, "content": m.content }));
            }
        }
        let said = if images.is_empty() {
            serde_json::json!({ "role": "user", "content": prompt })
        } else {
            let mut parts = vec![serde_json::json!({ "type": "text", "text": prompt })];
            parts.extend(images);
            serde_json::json!({ "role": "user", "content": parts })
        };
        turns.push(said);
        serde_json::Value::Array(turns)
    }

    /// Ask the engine itself, and stream its answer into the same delta channel
    /// the Rust service feeds, so a reply from a bare engine reads like any
    /// other. Nothing gates or writes here: this lane answers questions only.
    fn dispatch_engine(&self, messages: serde_json::Value, task_id: u64) {
        let engine = Engine { base: self.engine_url.clone(), key: self.api_key.clone() };
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let (ltx, lrx) = std::sync::mpsc::channel();
            // Hanging up is the engine's cancellation signal, so the flag has to
            // outlive the loop below: a channel the reader abandons stops the
            // stream instead of leaving a generation running for nobody.
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let cancel = cancel.clone();
                std::thread::spawn(move || engine.chat_stream(&messages, 512, &ltx, &cancel));
            }
            for d in lrx {
                match d {
                    ChatDelta::Reasoning(r) => { let _ = tx.send(BackendResponse::ThinkDelta(r)); }
                    ChatDelta::Content(c) => { let _ = tx.send(BackendResponse::ContentDelta(c)); }
                    // The answer arrived as deltas, so there is no final text to
                    // hand over — passing one would overwrite what streamed in.
                    ChatDelta::Done => {
                        cancel.store(true, Ordering::Relaxed);
                        let _ = tx.send(BackendResponse::Complete { task_id, status: "Complete".into(), final_text: String::new() });
                        return;
                    }
                    ChatDelta::Failed(e) => {
                        cancel.store(true, Ordering::Relaxed);
                        let _ = tx.send(BackendResponse::Failed(e));
                        return;
                    }
                }
            }
        });
    }

    /// Send one answer to the loop, off-thread so a slow service cannot freeze
    /// the window. The outcome comes back as an `Answered` event: the loop only
    /// accepts a question it still holds, and a 404 is not the same as a yes.
    fn answer(&mut self, task_id: u64, approval_id: String, decision: String) {
        let Some(sid) = self.core_sid_of(task_id) else {
            self.line = "This run has no supervised session on the agent loop, so there is nothing to answer."
                .to_string();
            return;
        };
        let (core, tx) = (self.core.clone(), self.response_tx.clone());
        std::thread::spawn(move || {
            let error = core.approve(&sid, &approval_id, &decision).err();
            let _ = tx.send(BackendResponse::Core(CoreEvent::Answered {
                task_id, approval_id, decision, error,
            }));
        });
    }

    /// Stop a supervised run. The loop's `cancel` ends the turn; the cards it was
    /// holding go expired with it, which `Pending` reports on the next poll.
    fn stop_run(&mut self, task_id: u64) {
        let Some(sid) = self.core_sid_of(task_id) else {
            self.set_task_status(task_id, "Idle");
            return;
        };
        let core = self.core.clone();
        std::thread::spawn(move || { let _ = core.cancel(&sid); });
    }

    /// The supervised session a task is running under, when it has one. Only
    /// such a run can be answered or cancelled — the Rust service has no
    /// question to answer.
    fn core_sid_of(&self, task_id: u64) -> Option<String> {
        self.tasks.iter().find(|t| t.id == task_id).and_then(|t| t.core_sid.clone())
    }

    /// The loop's `assistant` and `error` steps are the only answers it gives;
    /// until one arrives the placeholder is the only thing saying the run is
    /// alive, so the first real event has to retire it.
    fn clear_think_placeholder(&mut self, task_id: u64) {
        let Some((tid, mid)) = self.pending_think else { return };
        if tid != task_id { return; }
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
            if t.msgs.iter().any(|m| m.id == mid && m.content == "Thinking…") {
                t.msgs.retain(|m| m.id != mid);
            }
        }
        self.pending_think = None;
        self.pending_content = None;
    }

    /// Forget one call's live line, because its card just closed.
    fn drop_progress(&mut self, task_id: u64, call_id: &str) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            t.progress.remove(call_id);
        }
    }

    fn push_core_msg(&mut self, task_id: u64, kind: MsgKind, content: String, title: Option<String>) -> u64 {
        let mid = self.next_id;
        self.next_id += 1;
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
            t.msgs.push(Msg {
                id: mid, author: "Fennec".into(), content, kind, time: now(),
                collapsed: true,
                meta: title.map(|title| MsgMeta {
                    duration: None, title: Some(title), add: None, del: None, path: None, size: None, verb: None, root: None, call_id: None,
                }),
            });
        }
        self.scroll_handle.scroll_to_bottom();
        mid
    }

    /// Fold one translated loop event into the task model. Every arm is keyed by
    /// the event's own task id: a run keeps reporting after the user clicks to
    /// another row, and its answer must land in the task it came from.
    fn apply_core(&mut self, ev: CoreEvent, cx: &mut Context<Self>) {
        match ev {
            CoreEvent::Started { task_id, sid } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    t.core_sid = Some(sid);
                }
            }
            CoreEvent::Resume { task_id, sid } => {
                // The loop is working on this run right now, so its steps are
                // the live record and this window's copy is whatever the last
                // process happened to see. `replay` says so; the service's own
                // directory is the one the paths in those steps are relative to.
                let root = self.core_workspace();
                let (core, tx) = (self.core.clone(), self.response_tx.clone());
                std::thread::spawn(move || Self::poll_core(core, tx, sid, task_id, root, true));
            }
            CoreEvent::Replay { task_id } => {
                // Everything the last window watched after the prompt is about
                // to arrive again, so it goes first — otherwise each replayed
                // step would draw a second card. A question already answered
                // stays: the loop cannot resend the decision this user gave, and
                // My Attention reads the answer off that card.
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    match t.msgs.iter().rposition(|m| m.kind == MsgKind::User) {
                        Some(last) => t.msgs.truncate(last + 1),
                        None => t.msgs.clear(),
                    }
                    t.plan.clear();
                    // The lines are live bookkeeping of the steps being replayed,
                    // so the copy this window watched goes with the cards it came
                    // on; the replay re-sends whatever the loop still reports.
                    t.progress.clear();
                    t.approvals.retain(|a| a.resolved.is_some());
                    t.status = "Running".into();
                }
                self.tools.retain(|_, (t, _)| *t != task_id);
            }
            CoreEvent::Reasoning { task_id, text } => {
                self.clear_think_placeholder(task_id);
                self.push_core_msg(task_id, MsgKind::Think, text, None);
            }
            CoreEvent::Answer { task_id, text } => {
                self.clear_think_placeholder(task_id);
                self.push_core_msg(task_id, MsgKind::Result, text, None);
            }
            CoreEvent::Tool { task_id, id, title, detail, ok, output } => {
                self.clear_think_placeholder(task_id);
                match (ok, self.tools.get(&id).copied()) {
                    (None, _) => {
                        let mid = self.push_core_msg(task_id, MsgKind::Action, detail, Some(title));
                        // The card is filed under the loop's own id from the
                        // moment it opens: that is what a later `progress` step
                        // names, and what its block looks its live line up by.
                        if let Some(m) = self.msg_mut(task_id, mid) {
                            if let Some(meta) = m.meta.as_mut() { meta.call_id = Some(id.clone()); }
                        }
                        self.tools.insert(id, (task_id, mid));
                    }
                    (Some(ok), Some((_, mid))) => {
                        self.tools.remove(&id);
                        // Its report goes with the running state: the call said
                        // what it was doing, now it says what it did.
                        self.drop_progress(task_id, &id);
                        if let Some(m) = self.msg_mut(task_id, mid) {
                            m.collapsed = true;
                            if !output.is_empty() { m.content = output; }
                            if let Some(meta) = m.meta.as_mut() {
                                meta.duration = Some(if ok { String::new() } else { "failed".into() });
                            }
                        }
                    }
                    // A result whose `tool_start` this window never saw (a run
                    // selected after a restart): show it closed, not running.
                    (Some(ok), None) => {
                        let mid = self.push_core_msg(task_id, MsgKind::Action, output, Some(title));
                        if let Some(m) = self.msg_mut(task_id, mid) {
                            if let Some(meta) = m.meta.as_mut() {
                                meta.duration = Some(if ok { String::new() } else { "failed".into() });
                            }
                        }
                    }
                }
            }
            CoreEvent::Progress { task_id, id, line } => {
                // The line belongs to the card this window is watching under that
                // id. A report about a call no card of this run opened says
                // nothing, so it draws nothing — and it must not settle or fold
                // the block it landed in, which is why no message is pushed here.
                if self.tools.get(&id).is_some_and(|(t, _)| *t == task_id) {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                        t.progress.record(&id, line);
                    }
                }
            }
            CoreEvent::Deliver { task_id, name, path, verb, root } => {
                self.clear_think_placeholder(task_id);
                // One card per file: a run that rewrote the same file several
                // times delivered one file, and the card carries the last write's
                // words. Keyed on the path, because that is what the user opens.
                let existing = self.tasks.iter().find(|t| t.id == task_id)
                    .and_then(|t| t.msgs.iter().find(|m| {
                        m.kind == MsgKind::Attach
                            && m.meta.as_ref().is_some_and(|x| x.path.as_deref() == Some(path.as_str()))
                    }).map(|m| m.id));
                let mid = match existing {
                    Some(id) => id,
                    None => self.push_core_msg(task_id, MsgKind::Attach, String::new(), Some(name)),
                };
                if let Some(m) = self.msg_mut(task_id, mid) {
                    let meta = m.meta.get_or_insert(MsgMeta {
                        duration: None, title: None, add: None, del: None,
                        path: None, size: None, verb: None, root: None, call_id: None,
                    });
                    meta.verb = Some(verb.to_string());
                    meta.path = Some(path);
                    meta.root = Some(root);
                }
            }
            CoreEvent::Plan { task_id, entries } => {
                // The loop publishes the whole list each time, so the newest one
                // replaces the one on screen instead of extending it.
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    t.plan = entries;
                }
            }
            CoreEvent::Queue(items) => {
                // Complex runs leave unfinished follow-ups here. Agent-owned work
                // stays ahead of user prompts and has no user mutation controls.
                self.enqueue_agent(items);
            }
            CoreEvent::Approval { task_id, approval } => {
                self.clear_think_placeholder(task_id);
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    match t.approvals.iter_mut().find(|a| a.id == approval.id) {
                        // One question, one card: a step that repeats an id
                        // updates the card rather than opening a second way to
                        // answer the same call.
                        Some(open) => { if open.resolved.is_none() && !open.expired { *open = approval; } }
                        None => t.approvals.push(approval),
                    }
                }
            }
            CoreEvent::Answered { task_id, approval_id, decision, error } => {
                // The loop no longer holds the question (a 404), so the answer
                // cannot land. Say which happened, not that an answer was sent.
                let said = match &error {
                    Some(e) => format!("Answer did not land: {e}"),
                    None => format!("Answer sent: {}", fennec_ui::attention::decision_label(&decision)),
                };
                let good = error.is_none();
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                    if let Some(a) = t.approvals.iter_mut().find(|a| a.id == approval_id) {
                        match error {
                            Some(e) => { a.expired = true; a.note = e; }
                            None => a.resolved = Some(ApprovalResolution { choice_id: decision }),
                        }
                    }
                }
                self.toast(said, good);
            }
            CoreEvent::Pending { task_id, ids } => {
                let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) else { return };
                for a in t.approvals.iter_mut() {
                    if a.resolved.is_none() && !a.expired && !ids.contains(&a.id) {
                        a.expired = true;
                        a.note = "the loop no longer holds this question".into();
                    }
                }
                // A run stopped on a question waits on the user, not on the
                // model. That is what the rail, the badge and the filter read.
                if matches!(t.status.as_str(), "Running" | "AwaitingApproval") {
                    t.status = if ids.is_empty() { "Running".into() } else { "AwaitingApproval".into() };
                }
            }
            CoreEvent::Status { task_id, status } => {
                self.clear_think_placeholder(task_id);
                let word = match status.as_str() {
                    "done" => "Complete",
                    "error" => "Failed",
                    "cancelled" => "Idle",
                    _ => "Running",
                };
                self.set_task_status(task_id, word);
                if word != "Running" {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id) {
                        for a in t.approvals.iter_mut() {
                            if a.resolved.is_none() && !a.expired {
                                a.expired = true;
                                a.note = "the run ended before an answer arrived".into();
                            }
                        }
                    }
                    if !self.queued.is_empty() {
                        let next = self.queued.remove(0).text;
                        self.save();
                        self.dispatch(next, cx);
                    }
                }
            }
            CoreEvent::Refused { task_id, error } => {
                self.clear_think_placeholder(task_id);
                self.push_core_msg(task_id, MsgKind::Result,
                    format!("The agent loop refused this run: {error}"), None);
                self.set_task_status(task_id, "Failed");
            }
        }
        self.save();
        cx.notify();
    }
}

/// One loop step → what this shell shows. The words come from the table in
/// `fennec-ui`, so both gpui shells and the PWA name the same `run_shell` the
/// same way; only the envelope is this app's.
fn translate_step(tx: &std::sync::mpsc::Sender<BackendResponse>, task_id: u64, step: &serde_json::Value, root: &str) {
    use fennec_ui::attention::{args_line, clip, describe_call, looks_like_call, path_of, risk_word};
    let args = step["args"].clone();
    let name = step["name"].as_str().unwrap_or("tool");
    let (category, title) = describe_call(name, &args);
    let call_id = step["call_id"].as_str().unwrap_or(name).to_string();
    match step["type"].as_str().unwrap_or("") {
        "assistant" => {
            let reasoning = step["reasoning"].as_str().unwrap_or("").trim();
            if !reasoning.is_empty() {
                let _ = tx.send(BackendResponse::Core(CoreEvent::Reasoning {
                    task_id, text: reasoning.to_string(),
                }));
            }
            let content = step["content"].as_str().unwrap_or("").trim();
            // The loop publishes each tool spelling the model wrote as text as
            // its own assistant step, and a turn that does not land a call
            // spells the same blob again. As an answer that is the same JSON
            // object over and over; the call's own row says what ran, so the
            // blob never becomes an answer card.
            let calls = step["tool_calls"].as_array().is_some_and(|c| !c.is_empty());
            if !content.is_empty() && !looks_like_call(content, calls) {
                let _ = tx.send(BackendResponse::Core(CoreEvent::Answer {
                    task_id, text: content.to_string(),
                }));
            }
        }
        "tool_start" => {
            let _ = tx.send(BackendResponse::Core(CoreEvent::Tool {
                task_id, id: call_id, title: format!("{category} · {title}"),
                detail: args_line(&args), ok: None, output: String::new(),
            }));
        }
        "progress" => {
            // A report belongs to the call its own `call_id` opened, and says
            // nothing without one — a step that omitted it would otherwise be
            // filed under the tool's name and open a line no card owns. The
            // sentence is the loop's: `record_step` stores it verbatim and puts
            // the `step`/`of` meter beside it only when both halves arrived.
            let id = match step["call_id"].as_str() {
                Some(id) if !id.is_empty() => id.to_string(),
                _ => return,
            };
            let mut lines = fennec_ui::timeline::ProgressLines::default();
            if lines.record_step(step) {
                if let Some(line) = lines.get(&id) {
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Progress {
                        task_id, id, line: line.to_string(),
                    }));
                }
            }
        }
        "tool" => {
            let ok = step["ok"].as_bool().unwrap_or(false);
            let output = fennec_ui::attention::clip(&step["result"].to_string(), 4000);
            let _ = tx.send(BackendResponse::Core(CoreEvent::Tool {
                task_id, id: call_id, title: format!("{category} · {title}"),
                detail: args_line(&args), ok: Some(ok), output,
            }));
            // A write that succeeded put a file on disk: deliver it as its own
            // card, so the user gets the artifact and not just a tool row.
            if ok {
                if let (Some(verb), Some(path)) = (fennec_ui::attention::delivery_verb(name), path_of(&args)) {
                    let name = std::path::Path::new(&path).file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.clone());
                    let _ = tx.send(BackendResponse::Core(CoreEvent::Deliver {
                        task_id, name, path, verb, root: root.to_string(),
                    }));
                }
            }
        }
        "plan" => {
            // The loop republishes its whole stage list each time a stage
            // finishes, so the newest list replaces the one on screen. "done"
            // is the loop's own word for a finished stage.
            let mut entries = Vec::new();
            if let Some(rows) = step["entries"].as_array() {
                for r in rows {
                    if let Some(content) = r["content"].as_str() {
                        entries.push((content.to_string(), r["status"].as_str() == Some("completed")));
                    }
                }
            }
            if !entries.is_empty() {
                let _ = tx.send(BackendResponse::Core(CoreEvent::Plan { task_id, entries }));
            }
        }
        "queue" => {
            // The loop's own follow-ups for a run it judged complex. They are
            // prompts, not timeline rows, so they go to the queue strip.
            let items = step["items"].as_array().map(|list| {
                list.iter().filter_map(|v| v.as_str())
                    .map(str::to_string).collect::<Vec<String>>()
            }).unwrap_or_default();
            if !items.is_empty() {
                let _ = tx.send(BackendResponse::Core(CoreEvent::Queue(items)));
            }
        }
        "approval_required" => {
            let id = step["approval_id"].as_str().unwrap_or("");
            // Same rule the desktop and the PWA apply: a question with no id
            // cannot be answered and one with no name cannot be acted on, so
            // neither becomes a card.
            if id.is_empty() || name.is_empty() { return; }
            let _ = tx.send(BackendResponse::Core(CoreEvent::Approval {
                task_id,
                approval: ApprovalRequest {
                    id: clip(id, 64),
                    action: name.to_string(),
                    risk_level: risk_word(step["risk"].as_str()),
                    command: Some(title.clone()),
                    resolved: None,
                    category,
                    reason: clip(step["reason"].as_str().unwrap_or(""), 400),
                    path: path_of(&args),
                    expired: false,
                    note: String::new(),
                },
            }));
        }
        _ => {}
    }
}

fn now() -> String { chrono::Local::now().format("%H:%M").to_string() }

fn load_settings(settings_path: &Path) -> Settings {
    std::fs::read_to_string(settings_path).ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .unwrap_or_default()
}

/// A unified diff, line-tinted. Long diffs are truncated with a note rather
/// than paged: this is a preview, and the full text is one `git diff` away.
/// Each line keeps its kind — 0 context, 1 added, 2 removed, 3 hunk header —
/// so the dialog colours it without parsing twice.
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

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB { format!("{:.1} MB", b / MB) }
    else if b >= KB { format!("{:.1} KB", b / KB) }
    else { format!("{bytes} B") }
}

/// Content type of a picked file, from its extension. Only the image spellings
/// the engine accepts are called images here; everything else travels as text or
/// as a named file, so the prompt never claims a picture the model will not get.
fn attachment_mime(path: &Path) -> &'static str {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json" | "toml"
        | "yaml" | "yml" | "csv" | "log" | "sh" | "ps1" | "c" | "h" | "cpp" | "hpp"
        | "go" | "java" | "kt" | "swift" => "text/plain",
        _ => "application/octet-stream",
    }
}


/// The `.task-status-dot` colour: the run in flight is the brand, a finished one
/// is green, a queued one blue, a failed one red, and anything waiting for the
/// user stays muted.
fn task_dot(status: &str, t: &gpui_kit::component::Theme) -> Hsla {
    match status {
        "Running" | "Planning" => t.accent,
        "Complete" => t.green,
        "Failed" => t.red,
        "Queued" => t.blue,
        _ => t.muted_foreground,
    }
}
/// `.inspector-section-header`: the 10px semibold muted caption every right-
/// panel section opens with, plus the green count the PWA sets on the right.
fn insp_head(label: &'static str, count: Option<&'static str>, t: &Theme) -> impl IntoElement {
    // The caption and its count are two `Label`s with no element id of their
    // own, so all three heads in the panel would hash to one a11y path and a
    // debug build aborts on the duplicate once a UIA client attaches. The
    // caption text is unique per section, so it namespaces them.
    div().id(ElementId::Name(SharedString::from(format!("insp-head-{label}"))))
        .flex().items_center().justify_between().mb_1()
        .child(Label::new(label).text_size(px(10.0)).font_weight(FontWeight::SEMIBOLD)
            .text_color(t.muted_foreground))
        .children(count.map(|c| Label::new(c).text_size(px(11.0))
            .font_weight(FontWeight::MEDIUM).text_color(t.green)))
}

impl Render for App {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(text) = self.composer_restore.take() {
            self.composer.update(cx, |state, cx| state.set_value(text, _w, cx));
        }
        let t = cx.theme().clone();
        // While a divider is dragged its move/up tracking lives on the root:
        // gpui dispatches mouse events to ancestors under the pointer, so the
        // pane keeps following the cursor after it leaves the 6px grab strip,
        // and an accidental release outside a pane still ends the drag.
        let dragging = self.pane_drag.is_some();
        div().flex().flex_col().size_full().bg(t.background).text_color(t.foreground)
            .when(dragging, |root| root
                .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _w, cx| {
                    let Some((pane, start_x, start_w)) = this.pane_drag else { return };
                    if !e.dragging() {
                        this.pane_drag = None;
                        this.save_settings();
                        cx.notify();
                        return;
                    }
                    let dx = f32::from(e.position.x) - start_x;
                    let w = match pane {
                        Pane::Rail => start_w + dx,
                        Pane::Inspector => start_w - dx,
                    };
                    this.set_pane_width(pane, w);
                    cx.notify();
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _e: &MouseUpEvent, _w, cx| {
                    if this.pane_drag.take().is_some() {
                        this.save_settings();
                        cx.notify();
                    }
                })))
            .child(self.topbar(cx))
            .child(div().flex().flex_1().overflow_hidden()
                .child(self.left(cx))
                .child(self.resizer(Pane::Rail, cx))
                .child(self.center(cx))
                .when(self.inspector, |row| row
                    .child(self.resizer(Pane::Inspector, cx))
                    .child(self.right(cx))))
            .child(self.dropdowns(cx))
    }
}

impl App {
    fn set_pane_width(&mut self, pane: Pane, w: f32) {
        match pane {
            Pane::Rail => self.rail_width = pane.clamp(w),
            Pane::Inspector => self.inspector_width = pane.clamp(w),
        }
    }

    /// The 6px grab strip straddling a side pane's inner edge — the native twin
    /// of the PWA's `.pane-resizer`: see-through until hovered, a lit rule while
    /// dragged, col-resize cursor, and a double-click hands the default width
    /// back. The negative margins overlap the border so the panes stay flush.
    fn resizer(&self, pane: Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let dragging = self.pane_drag.map(|(p, ..)| p == pane).unwrap_or(false);
        let start = match pane {
            Pane::Rail => self.rail_width,
            Pane::Inspector => self.inspector_width,
        };
        div().flex_shrink_0().w(px(6.0)).mx(px(-3.0)).cursor_col_resize()
            .when(!dragging, |d| d.hover(|s| s.bg(t.accent.opacity(0.18))))
            .child(div().w(px(2.0)).h_full().when(dragging, |d| d.bg(t.accent)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _w, cx| {
                if e.click_count == 2 {
                    this.pane_drag = None;
                    this.set_pane_width(pane, pane.default_width());
                    this.save_settings();
                } else {
                    this.pane_drag = Some((pane, f32::from(e.position.x), start));
                }
                cx.notify();
            }))
    }

    fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let status_text = if self.backend_ok { "Backend connected" } else { "Backend offline" };
        let status_color = if self.backend_ok { t.green } else { t.red };
        
        div().h(px(56.0)).flex_shrink_0().px_4().flex().items_center().justify_between()
            .bg(fennec_ui::theme::panel()).border_b_1().border_color(t.border)
            .child(div().flex().items_center().gap_3()
                .child(brand_fox(&t, 48.0))
                .child(div().flex().flex_col().gap_0()
                    .child(Label::new("Fennec").font_weight(FontWeight::SEMIBOLD).text_size(px(14.0)))
                    .child(div().flex().items_center().gap_2()
                        .child(div().w(px(6.0)).h(px(6.0)).rounded_full().flex_shrink_0().bg(status_color))
                        .child(Label::new(status_text).text_size(px(11.0)).text_color(t.muted_foreground)))))
            .child(div().flex().items_center().gap_2()
                .child(Button::new("settings").ghost().xsmall().w(px(36.0)).h(px(36.0)).rounded_md()
                    .border_1().border_color(t.border).justify_center()
                    .child(ui_icon("settings", 18.0, t.muted_foreground))
                    .on_click(cx.listener(|a, _, window, cx| { a.open_settings(window, cx); })))
                .child(Button::new("inspector-toggle").ghost().xsmall().w(px(36.0)).h(px(36.0)).rounded_md()
                    .border_1().border_color(t.border).justify_center()
                    .when(self.inspector, |b| b.bg(t.accent.opacity(0.15)).border_color(t.accent))
                    .child(ui_icon("panel", 18.0, if self.inspector { t.accent } else { t.foreground }))
                    .on_click(cx.listener(|a, _, _, cx| { a.inspector = !a.inspector; cx.notify(); })))
                .child(div().w(px(1.0)).h(px(20.0)).bg(t.border).mx_1())
                .child(Button::new("minimize").ghost().xsmall().w(px(32.0)).h(px(32.0)).rounded_md()
                    .justify_center()
                    .child(Label::new("—").text_size(px(14.0)).text_color(t.muted_foreground))
                    .on_click(cx.listener(|_, _, window, _| { window.minimize_window(); })))
                .child(Button::new("maximize").ghost().xsmall().w(px(32.0)).h(px(32.0)).rounded_md()
                    .justify_center()
                    .child(Label::new("□").text_size(px(14.0)).text_color(t.muted_foreground))
                    .on_click(cx.listener(|_, _, window, _| { window.toggle_fullscreen(); })))
                .child(Button::new("close").ghost().xsmall().w(px(32.0)).h(px(32.0)).rounded_md()
                    .justify_center()
                    .child(Label::new("×").text_size(px(16.0)).text_color(t.red))
                    .on_click(cx.listener(|_, _, window, cx| { window.remove_window(); cx.quit(); }))))
    }

    fn left(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let panel = fennec_ui::theme::panel();
        let hover = fennec_ui::theme::hover();
        let surface = t.muted;
        let query = self.search.read(cx).value().trim().to_string();
        let q_lower = query.to_lowercase();
        let attention = self.attention_count();
        let tasks = self.filtered_tasks(&query);
        let branch = if self.branch.is_empty() { "main".to_string() } else { self.branch.clone() };
        let working_in = {
            let ws_name = self.active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_else(|| "Workspace".into());
            let on_change = {
                let weak = cx.entity().downgrade();
                move |window: &mut Window, cx: &mut gpui_kit::App| {
                    if let Some(app) = weak.upgrade() {
                        let _ = app.update(cx, |app, cx| app.open_repository_dialog(window, cx));
                    }
                }
            };
            let on_create = {
                let weak = cx.entity().downgrade();
                move |name: String, _window: &mut Window, cx: &mut gpui_kit::App| {
                    if let Some(app) = weak.upgrade() {
                        let _ = app.update(cx, |app, _cx| {
                            let ws_id = app.active_workspace_id.clone().unwrap_or_default();
                            app.create_github_repository(ws_id, name);
                        });
                    }
                }
            };
            fennec_ui::repository::working_in_banner(
                &t,
                &ws_name,
                self.active_github_repo().as_deref(),
                &branch,
                &self.repo_create_input,
                on_change,
                on_create,
            )
        };
        div().w(px(self.rail_width)).flex_shrink_0().bg(panel).border_r_1().border_color(t.border)
            .flex().flex_col().p_3().gap_4()
            .child(working_in)
            // `.nav-section`: attention and search remain the primary rail actions.
            .child(div().flex().flex_col().gap_1()
                .child(Button::new("nav-attention").ghost().w_full().min_h(px(36.0)).rounded_md().px_3().py_2()
                    .justify_start()
                    .when(self.attention_only, |b| b.bg(surface))
                    .child(div().w_full().flex().items_center().gap_3()
                        .child(div().w(px(18.0)).flex_shrink_0().flex().justify_center()
                            .child(ui_icon("clock", 16.0, if self.attention_only { t.accent } else { t.muted_foreground })))
                        .child(Label::new("My attention").text_size(px(13.0)).font_weight(FontWeight::MEDIUM))
                        .children(attention_badge(&t, attention).map(|badge| badge.ml_auto())))
                    .on_click(cx.listener(|a, _, _, cx| {
                        a.attention_only = !a.attention_only;
                        a.search_open = false;
                        cx.notify();
                    })))
                .child(Button::new("nav-search").ghost().w_full().min_h(px(36.0)).rounded_md().px_3().py_2()
                    .justify_start()
                    .when(self.search_open, |b| b.bg(surface))
                    .child(div().w_full().flex().items_center().gap_3()
                        .child(div().w(px(18.0)).flex_shrink_0().flex().justify_center()
                            .child(ui_icon("search", 16.0, if self.search_open { t.accent } else { t.muted_foreground })))
                        .child(Label::new("Search").text_size(px(13.0)).font_weight(FontWeight::MEDIUM)))
                    .on_click(cx.listener(|a, _, _, cx| {
                        a.search_open = !a.search_open;
                        a.attention_only = false;
                        cx.notify();
                    }))))
            // `.section-header`: the Workspaces label and the add-workspace button.
            .child(div().id("ws-section-header").flex().items_center().justify_between()
                .child(Label::new("Workspaces").text_size(px(10.0)).font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.muted_foreground))
                .when(self.hovered_ws_header, |d| d
                    .child(Button::new("new-workspace-header").ghost().xsmall()
                        .w(px(24.0)).h(px(24.0)).rounded_md().justify_center()
                        .border_1().border_color(t.border)
                        .child(ui_icon("plus", 14.0, t.muted_foreground))
                        .on_click(cx.listener(|_a, _, window, cx| {
                            let weak = cx.entity().downgrade();
                            open_workspace_dialog(window, cx, move |name, folders, icon, color, indexed, cx| {
                                let _ = weak.update(cx, |a, cx| {
                                    a.add_workspace(name, folders, icon, color, indexed);
                                    cx.notify();
                                });
                            });
                        }))))
                .when(!self.hovered_ws_header, |d| d
                    .child(div().w(px(24.0)).h(px(24.0)).flex_shrink_0()))
                .on_hover(cx.listener(|a, hovered, _, cx| {
                    a.hovered_ws_header = *hovered;
                    cx.notify();
                })))
            // `#task-search`: the filter box only shows while Search is open.
            .children(self.search_open.then(|| {
                // Same wrapper-focus workaround as the browser address field:
                // the editor's hitbox alone misses clicks on this row.
                let mut row = div().relative().flex().items_center().rounded_md().bg(surface)
                    .border_1().border_color(if query.is_empty() { t.border } else { t.accent })
                    .px_2().min_h(px(30.0))
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                        this.search.update(cx, |state, cx| window.focus(&state.focus_handle(cx), cx));
                    }));
                row = row.child(div().flex_1().child(
                    Input::new(&self.search).bordered(false).appearance(false)
                        .h(px(30.0)).text_size(px(12.0)),
                ));
                if query.is_empty() {
                    row = row.child(div().absolute().top_0().left_0().flex().items_center().h(px(30.0)).px_2()
                        .child(Label::new("Filter tasks…").text_size(px(12.0)).text_color(t.muted_foreground)));
                }
                div().flex().flex_col().gap_1()
                    .child(row)
                    .child(Label::new(if query.is_empty() { String::new() } else { format!("{} tasks", tasks.len()) })
                        .text_size(px(10.0)).text_color(t.muted_foreground))
            }))
            // `.workspace-list`: attention replaces the workspace list in this same area.
            .child(if self.attention_only {
                let weak = cx.entity().downgrade();
                approved_commands_panel(&t, &self.approved_commands(), move |task_id, _window, cx| {
                    let Ok(id) = task_id.parse::<u64>() else { return };
                    let _ = weak.update(cx, |a, cx| {
                        a.select_task(id);
                        a.attention_only = false;
                        cx.notify();
                    });
                }).flex_1().overflow_y_scrollbar().into_any_element()
            } else {
                div().flex().flex_col().gap_3().flex_1().overflow_y_scrollbar()
                .children(self.workspaces.iter().map(|ws| { 
                    let t = t.clone();
                    let ws_id = ws.id.clone();
                    let ws_id2 = ws_id.clone();
                    let ws_id3 = ws_id.clone();
                    let ws_name = ws.name.clone();
                    let is_active = self.active_workspace_id.as_deref() == Some(&ws.id);
                    let ws_hovered = self.hovered_workspace_id.as_deref() == Some(ws.id.as_str());
                    let ws_id4 = ws.id.clone();
                    let ws_tasks: Vec<&Task> = self.tasks.iter()
                        .filter(|task| task.workspace_id.as_deref() == Some(&ws.id))
                        .filter(|task| self.task_shown(task, &q_lower))
                        .collect();

                    div().flex().flex_col().gap_1()
                        .child(div().id(ElementId::Name(format!("ws-row-{ws_id}").into())).relative()
                            .flex().items_center().gap_2().py_1().px_3().rounded_md().cursor_pointer()
                            .hover(|d| d.bg(hover))
                            .when(is_active, |d| d.bg(surface))
                            .on_click(cx.listener(move |a, _, _, cx| {
                                a.select_workspace(&ws_id);
                                cx.notify();
                            }))
                            .on_hover(cx.listener(move |a, hovered, _, cx| {
                                if *hovered {
                                    a.hovered_workspace_id = Some(ws_id4.clone());
                                } else if a.hovered_workspace_id.as_deref() == Some(ws_id4.as_str()) {
                                    a.hovered_workspace_id = None;
                                }
                                cx.notify();
                            }))
                            .when(is_active, |d| d.child(
                                div().absolute().left_0().top_0().h_full().w(px(3.0)).bg(t.accent),
                            ))
                            // The PWA paints every workspace with the same
                            // folder line icon, tinted by the row's state.
                            .child(ui_icon("workspace", 14.0, if is_active { t.accent } else { t.muted_foreground }))
                            .child(Label::new(ws_name.as_str()).text_size(px(13.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_active { t.accent } else { t.foreground }))
                            .child(div().flex_1())
                            // The +/× sit in the row only while the pointer is
                            // over it; the placeholder keeps the label from
                            // shifting when they appear.
                            .when(ws_hovered, |d| d
                                .child(Button::new(format!("ws-new-task-{}", ws.id)).ghost().xsmall()
                                    .w(px(20.0)).h(px(20.0)).rounded_md().justify_center()
                                    .border_1().border_color(t.border)
                                    .child(ui_icon("plus", 12.0, t.muted_foreground))
                                    .on_click(cx.listener(move |a, _, _, cx| {
                                        a.new_task_in_workspace(&ws_id2);
                                        cx.notify();
                                    })))
                                .child(Button::new(format!("ws-delete-{}", ws.id)).label("×").ghost().xsmall()
                                    .w(px(20.0)).h(px(20.0)).rounded_md().justify_center()
                                    .border_1().border_color(t.border)
                                    .on_click(cx.listener(move |a, _, _, cx| {
                                        a.delete_workspace(&ws_id3);
                                        cx.notify();
                                    }))))
                            .when(!ws_hovered, |d| d
                                .child(div().w(px(48.0)).h(px(20.0)).flex_shrink_0())))
                        .children(ws_tasks.iter().map(|task| {
                            let t = t.clone();
                            let id = task.id;
                            let title = task.title.clone();
                            let status = task.status.clone();
                            let repo = task.repo.clone();
                            let is_active = self.active == Some(id);
                            let task_hovered = self.hovered_task_id == Some(id);

                            let mut info = div().flex().items_center().gap_2()
                                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().flex_shrink_0()
                                    .bg(task_dot(&status, &t)))
                                .child(Label::new(status.as_str()).text_size(px(11.0))
                                    .text_color(t.muted_foreground).flex_shrink_0())
                                .child(Label::new("·").text_size(px(11.0)).text_color(t.muted_foreground).flex_shrink_0());
                            if !repo.is_empty() {
                                info = info.child(Label::new(repo.as_str()).text_size(px(11.0))
                                    .text_color(t.muted_foreground).truncate());
                            }
                            let meta = div().flex().flex_col().gap_1().flex_1().overflow_hidden()
                                .child(Label::new(title.as_str()).text_size(px(13.0))
                                    .font_weight(FontWeight::MEDIUM).truncate())
                                .child(info);
                            let rename = Button::new(format!("task-rename-{}", id)).label("✎").ghost().xsmall()
                                .w(px(20.0)).h(px(20.0)).rounded_sm().justify_center()
                                .on_click(cx.listener(move |a, _, window, cx| {
                                    a.open_rename_task_dialog(id, window, cx);
                                }));
                            let remove = Button::new(format!("task-delete-{}", id)).label("×").ghost().xsmall()
                                .w(px(20.0)).h(px(20.0)).rounded_sm().justify_center()
                                .on_click(cx.listener(move |a, _, window, cx| {
                                    a.confirm_delete_task(id, window, cx);
                                }));
                            let acts = div().flex().items_center().gap_1().flex_shrink_0()
                                .child(rename)
                                .child(remove);

                            div().id(ElementId::NamedInteger("ws-task-".into(), id)).relative()
                                .flex().items_start().gap_2().ml_4().py_2().px_3().rounded_md().cursor_pointer()
                                .hover(|d| d.bg(hover))
                                .when(is_active, |d| d.bg(surface))
                                .on_click(cx.listener(move |a, _, _, cx| {
                                    a.select_task(id);
                                    cx.notify();
                                }))
                                .on_hover(cx.listener(move |a, hovered, _, cx| {
                                    if *hovered {
                                        a.hovered_task_id = Some(id);
                                    } else if a.hovered_task_id == Some(id) {
                                        a.hovered_task_id = None;
                                    }
                                    cx.notify();
                                }))
                                .when(is_active, |d| d.child(
                                    div().absolute().left_0().top_0().h_full().w(px(2.0)).bg(t.accent),
                                ))
                                .child(meta)
                                .when(task_hovered, |d| d.child(acts))
                                .when(!task_hovered, |d| d
                                    .child(div().w(px(44.0)).h(px(20.0)).flex_shrink_0()))
                        }))
                }))
                .into_any_element()
            })
            // `.task-empty`: the rail says so when there is nothing to list.
            .children((self.tasks.is_empty() && !self.attention_only).then(|| {
                Label::new("No tasks yet. Start with a prompt.")
                    .text_size(px(12.0)).text_color(t.muted_foreground).py_3()
            }))
            // `.user-section`: two outline chips, the PWA's `.github-account-button`
            // shape — the account chip green-outlined once GitHub is connected,
            // and a terracotta "Configure" chip that opens Settings.
            .child({
                let connected = self.github.authenticated;
                let github_label = self.github.account.as_ref()
                    .map(|account| format!("@{}", account.login))
                    .unwrap_or_else(|| "Local account".into());
                let chip_color = if connected { t.green } else { t.muted_foreground };
                let chip_border = if connected { t.green.opacity(0.45) } else { t.border };
                let github = Button::new("github-account").ghost().xsmall()
                    .flex().items_center().gap_2().px_2().py_1().rounded(px(5.0))
                    .border_1().border_color(chip_border)
                    .child(ui_icon("github", 14.0, chip_color))
                    .child(Label::new(github_label).text_size(px(11.0)).text_color(chip_color))
                    .on_click(cx.listener(|a, _, _, cx| {
                        if a.github.authenticated {
                            a.toast("GitHub is connected through the local GitHub CLI.", false);
                        } else {
                            a.toast("Run `gh auth login`, then reopen Fennec to refresh GitHub.", false);
                        }
                        cx.notify();
                    }));
                let configure = Button::new("account-configure").ghost().xsmall()
                    .flex().items_center().gap_2().px_2().py_1().rounded(px(5.0))
                    .border_1().border_color(t.accent.opacity(0.5))
                    .child(ui_icon("settings", 14.0, t.accent))
                    .child(Label::new("Configure").text_size(px(11.0)).text_color(t.accent))
                    .on_click(cx.listener(|a, _, window, cx| { a.open_settings(window, cx); }));
                div().mt_auto().flex().items_center().justify_between().gap_3().pt_4().border_t_1().border_color(t.border)
                    .child(github)
                    .child(configure)
            })
    }

    fn center(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let surface = t.muted;
        let tk = self.task();
        let is_working = self.working();
        // Queue items outlive the run that made them: a cancelled or failed run
        // can still leave follow-ups behind, and the user needs to see and
        // remove them. The strip renders nothing when the queue is empty.
        let show_queue = !self.queued.is_empty();
        let ws = self.active_workspace();
        // `.timeline-header-title` names the repository and branch the run works
        // in, in monospace, above the task's own title.
        let repo_slug = tk.map(|tk| tk.repo.clone())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| {
                ws.map(|w| w.name.clone()).unwrap_or_else(|| "fennec".into())
                    .to_lowercase().replace(' ', "-")
            });
        let branch = if self.branch.is_empty() { "main".to_string() } else { self.branch.clone() };
        let repo_line = format!("{repo_slug} · {branch} · 1.0.0");
        let title = tk.map(|tk| tk.title.clone()).unwrap_or_else(|| "New task".into());
        let status = tk.map(|tk| tk.status.clone()).unwrap_or_default();
        let badge_dot = task_dot(&status, &t);

        let stop = Button::new("stop").label("Stop").ghost().small()
            .when(!is_working, |b| b.disabled(true))
            .on_click(cx.listener(|a, _, _, cx| {
                // Cancel means asking the loop to stop, not relabeling
                // the row. A run this window never supervised only has
                // its own status to change.
                let task_id = a.active.unwrap_or(0);
                a.stop_run(task_id);
                cx.notify();
            }));
        let compact = Button::new("compact").label("⊟").ghost().small()
            .on_click(cx.listener(|a, _, _, cx| {
                a.compact_active();
                cx.notify();
            }));
        let badge = tk.map(|_| {
            div().flex().items_center().gap_2().px_3().py_1().rounded_full().bg(surface)
                .border_1().border_color(t.border)
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().flex_shrink_0().bg(badge_dot))
                .child(Label::new(status.clone()).text_size(px(12.0)).font_weight(FontWeight::MEDIUM))
        });
        // The PWA's `.timeline-empty`: with no messages yet there is no run to
        // head, so the header stays hidden and the pane asks the question
        // instead. Once a run exists the header returns above the timeline.
        let empty = tk.map(|tk| tk.msgs.is_empty()).unwrap_or(true) && !is_working;

        div().flex().flex_col().flex_1().overflow_hidden()
            .children((!empty).then(|| div().flex().items_start().justify_between().gap_3().px_6().pt_4().pb_2()
                .border_b_1().border_color(t.border)
                .child(div().flex().flex_col().gap_1().flex_1().overflow_hidden()
                    .child(Label::new(repo_line).font_family("monospace").text_size(px(11.0))
                        .text_color(t.muted_foreground))
                    .child(Label::new(title).text_size(px(20.0)).font_weight(FontWeight::SEMIBOLD))
                    .child(Label::new("Chronological local-agent activity and verification for the active repository.")
                        .text_size(px(13.0)).text_color(t.muted_foreground).mt_1()))
                .child(div().flex().items_center().gap_2().flex_shrink_0()
                    .child(stop)
                    .child(compact)
                    .children(badge))))
            .child(if empty {
                div().flex_1().flex().items_center().justify_center().px_6()
                    .child(div().flex().flex_col().items_center().gap_2().max_w(px(520.0)).py_10()
                        .child(Label::new("What would you like to do?")
                            .text_size(px(24.0)).font_weight(FontWeight::SEMIBOLD).text_color(t.foreground))
                        .child(Label::new("Start with a small, concrete ask. Your work will stay here.")
                            .text_size(px(15.0)).text_color(t.muted_foreground).text_center()))
                    .into_any_element()
            } else {
                self.timeline(cx).into_any_element()
            })
            .children(show_queue.then(|| self.queue(cx)))
            .child(self.composer(cx))
    }

    fn timeline(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let task = self.task();
        let empty_progress = fennec_ui::timeline::ProgressLines::default();
        let msgs = task.map(|task| task.msgs.as_slice()).unwrap_or(&[]);
        // The live lines the run's in-flight calls last reported, read off the
        // task so a block can carry its own call's line while it draws.
        let progress = task.map(|task| &task.progress).unwrap_or(&empty_progress);
        let weak = cx.entity().downgrade();
        let mut list = div().id("timeline-scroll").flex_1().track_scroll(&self.scroll_handle).overflow_y_scrollbar().px_6().py_4().flex().flex_col().gap_3();
        // The calls folded into one block per run, drawn where the run happened.
        let folds = fold_calls(&msgs, self.working(), &progress);
        // The clock the live block carries, read once for the whole draw.
        let run_secs = self.elapsed_secs();
        for (mi, m) in msgs.iter().enumerate() {
            let id = m.id;
            // A settled call renders as a row inside its run's block, not as a
            // card of its own. The block the run is still inside is open, and the
            // answer that ends the run is what folds it.
            if let Some(b) = folds.at.get(&mi) {
                let key = b.id;
                let open = block_open(b.live, self.block_open.get(&key).copied());
                let wt = weak.clone();
                let on_toggle = move |_: &mut Window, cx: &mut gpui::App| {
                    let _ = wt.update(cx, |a, cx| {
                        a.toggle_block(key, open);
                        cx.notify();
                    });
                };
                // A hidden call still has to be readable: a write's row opens the
                // diff of the file it changed, any other row prints the whole call
                // it stands for plus what it returned.
                let opens = b.acts.clone();
                // The live lines are appended after the calls, so they need their
                // own index space: a click on one of them leads nowhere, and the
                // settled rows keep opening what they always opened.
                let mut drawn = b.rows.clone();
                drawn.extend(b.live_rows.iter().cloned());
                let wr = weak.clone();
                let on_row = move |i: usize, window: &mut Window, cx: &mut gpui::App| {
                    match opens.get(i) {
                        Some(RowOpen::Text(title, text)) => show_output(title, text, window, cx),
                        Some(RowOpen::File(path)) if !path.is_empty() => {
                            let path = path.clone();
                            let _ = wr.update(cx, |a, cx| {
                                a.review_edit(path, window, cx);
                            });
                        }
                        _ => {}
                    }
                };
                // The counts come off the settled calls only: a report of what a
                // call is doing is not a thing the run ran.
                let title = block_title(&b.rows, b.live, run_secs);
                list = list.child(tool_group(&t, key, "⚙", &title, &drawn, open, b.live, on_toggle, Some(on_row)));
            }
            if folds.hidden.contains(&mi) {
                continue;
            }
            match &m.kind {
                MsgKind::User => {
                    // The user's own words sit in an accent-tinted card with no
                    // identity chip, just the time and the message.
                    list = list.child(user_card(&t, &m.time, &m.content, id as usize));
                }
                MsgKind::Think => {
                    // A thought with words in it is a row of its run's block,
                    // hidden here by the block that took it. What reaches this
                    // arm is a placeholder with nothing yet to say.
                }
                MsgKind::Action => {
                    let title = m.meta.as_ref().and_then(|x| x.title.clone()).unwrap_or_else(|| "Action".into());
                    let dur = m.meta.as_ref().and_then(|x| x.duration.clone());
                    let running = dur.is_none();
                    let dur = dur.unwrap_or_default();
                    let failed = dur == "failed";
                    let status_text = if running { "Running…".to_string() }
                        else if failed { "Failed".to_string() }
                        else if dur.is_empty() { "Complete".to_string() }
                        else { format!("Complete · {dur}") };
                    let status_color = if running { t.yellow } else if failed { t.red } else { t.green };
                    let collapsed = m.collapsed;
                    let weak2 = weak.clone();
                    // The running call keeps its short words in the header and
                    // prints the whole call in its body once opened.
                    let path = m.meta.as_ref().and_then(|x| x.path.clone()).unwrap_or_default();
                    let short = short_text(&path, &title);
                    let shown = reveal_text(&short, &title, &m.content);
                    list = list.child(action_card(&t, id, "🔍", &short, &shown, &status_text, status_color, !collapsed,
                        move |_, cx| {
                            let _ = weak2.update(cx, |a, cx| { a.toggle(id); cx.notify(); });
                        }));
                }
                MsgKind::Attach => {
                    // The delivered file's own card, next to the block holding
                    // the call that wrote it: the chip says which verb the run
                    // used, and a file the user attached has none to say.
                    let name = m.meta.as_ref().and_then(|x| x.title.clone()).unwrap_or_else(|| "file".into());
                    let path = m.meta.as_ref().and_then(|x| x.path.clone()).unwrap_or_default();
                    let verb = m.meta.as_ref().and_then(|x| x.verb.clone()).unwrap_or_default();
                    let subtitle = attachment_subtitle(&path, &name);
                    // The card names the directory the run wrote into. Only a card
                    // that predates that field falls back, and `work_dir` keeps the
                    // path from resolving against the directory the app was launched
                    // from, which is nowhere near the workspace.
                    let root = m.meta.as_ref().and_then(|x| x.root.clone()).unwrap_or_default();
                    let dir = if root.is_empty() {
                        self.work_dir().to_string_lossy().into_owned()
                    } else {
                        root
                    };
                    let open_path = absolute(&dir, &path);
                    // A picked file has no verb, and an ellipsis there reads as a
                    // cut-off word; its size is the one fact the card can put.
                    let trailing = if verb.is_empty() {
                        m.meta.as_ref().and_then(|x| x.size.clone()).unwrap_or_default()
                    } else {
                        verb
                    };
                    let weak3 = weak.clone();
                    let view_name = name.clone();
                    list = list.child(attachment_card(&t, id, &name, &subtitle, &trailing,
                        Some(move |_: &mut Window, cx: &mut gpui::App| {
                            if !open_path.is_empty() {
                                let name = view_name.clone();
                                let path = open_path.clone();
                                let _ = weak3.update(cx, move |a, cx| {
                                    a.show_delivered(path, name);
                                    cx.notify();
                                });
                            }
                        })));
                }
                MsgKind::Edit => {
                    // A write from a history file: folded into its action block
                    // above, so nothing renders here.
                }
                MsgKind::Result => {
                    let speak = m.content.clone();
                    // The answer carries only its own words. What the run thought
                    // on the way here is a row of the block above it, not a
                    // second copy attached to the message it led to.
                    let block = div()
                        .child(rich_div(&t, &m.content, false, id as usize))
                        .child(msg_footer(&t, id, &m.time, Some(Box::new(
                            move |window: &mut Window, cx: &mut gpui::App| {
                                speak_tts(&speak, window, cx);
                            },
                        ))));
                    list = list.child(msg_card(&t, block)
                        .id(ElementId::NamedInteger("msg".into(), id)));
                }
            }
        }
        // The loop's own stage list, republished as stages finish. It is the one
        // part of the run that says what comes next rather than what happened,
        // so it reads as a checklist, not another card in the timeline.
        let plan = self.task().map(|tk| tk.plan.clone()).unwrap_or_default();
        if !plan.is_empty() {
            let mut head = div().rounded_md().border_1().border_color(t.border).bg(t.muted).p_3().flex().flex_col().gap_1()
                .child(Label::new("Plan").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)).text_color(t.muted_foreground));
            for (pi, (text, done)) in plan.iter().enumerate() {
                head = head.child(div().id(ElementId::NamedInteger("plan".into(), pi as u64)).flex().items_center().gap_2()
                    .child(Label::new(if *done { "✓" } else { "→" }).text_size(px(12.0)).text_color(if *done { t.green } else { t.muted_foreground }))
                    .child(Label::new(text.clone()).text_size(px(12.5)).text_color(if *done { t.muted_foreground } else { t.foreground })));
            }
            list = list.child(head);
        }
        // A gated call the loop is stopped on, last in the timeline because it is
        // the one thing here that cannot move until the user does. The card comes
        // from `fennec-ui`, so the same question reads and answers identically in
        // the desktop shell and here.
        let task_id = self.active.unwrap_or(0);
        let approvals = self.task().map(|t| t.approvals.clone()).unwrap_or_default();
        for a in &approvals {
            let state = match (&a.resolved, a.expired) {
                (Some(_), _) => CardState::Answered,
                (None, true) => CardState::Expired,
                (None, false) => CardState::Pending,
            };
            // An answer the user already sent is not a question: the card leaves
            // the run and My Attention keeps it in the answered list.
            if !card_shows(state) {
                continue;
            }
            let title = gate_title(&a.action, a.command.as_deref());
            let shared = ApprovalCard {
                id: &a.id,
                category: &a.category,
                title: &title,
                risk: &a.risk_level,
                reason: &a.reason,
                path: a.path.as_deref(),
                detail: &title,
                state,
                decision: a.resolved.as_ref().map(|r| r.choice_id.as_str()).unwrap_or(""),
                note: &a.note,
            };
            let weak2 = weak.clone();
            let approval_id = a.id.clone();
            list = list.child(approval_card(&t, &shared, move |decision, _window, cx| {
                let decision = decision.to_string();
                let _ = weak2.update(cx, |a, cx| {
                    a.answer(task_id, approval_id.clone(), decision);
                    cx.notify();
                });
            }));
        }
        list
    }

    fn queue(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        if self.queued.is_empty() {
            return div().into_any_element();
        }
        let items: Vec<QueueItem> = self.queued.iter().map(|item| QueueItem {
            text: item.text.clone(),
            owner: if item.owner == QueueOwner::Agent { UiQueueOwner::Agent } else { UiQueueOwner::User },
        }).collect();
        let weak = cx.entity().downgrade();
        let wr = weak.clone();
        queue_strip(&t, &items, move |window, cx| {
            if let Some(app) = weak.upgrade() {
                let n = app.read(cx).queued.len();
                window.push_notification(Notification::info(format!("{n} tasks in queue")), cx);
            }
        }, move |action, window, cx| {
            let _ = wr.update(cx, |a, cx| {
                a.queue_action(action, window, cx);
                cx.notify();
            });
        }).into_any_element()
    }

    fn composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let send_enabled = !self.composer.read(cx).value().trim().is_empty();
        let mut col = div().border_t_1().border_color(t.border).bg(fennec_ui::theme::panel()).px_6().py_3().flex().flex_col().gap_2();
        for (ti, toast) in self.toasts.iter().enumerate() {
            col = col.child(div().id(ElementId::NamedInteger("toast".into(), ti as u64)).flex().items_center().gap_2()
                .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(if toast.good { t.success } else { t.yellow }))
                .child(Label::new(toast.text.clone()).text_size(px(12.0)).text_color(t.foreground)));
        }
        if !self.line.is_empty() {
            col = col.child(Label::new(self.line.clone()).text_size(px(12.0)).text_color(t.yellow));
        }
        let weak = cx.entity().downgrade();
        let w_attach = weak.clone();
        let w_perm = weak.clone();
        let w_model = weak.clone();
        let w_mic = weak.clone();
        // The picked files, shown as chips with a way to take one back out.
        // They are the prompt's, not the task's, so they leave with the send.
        if !self.pending_atts.is_empty() {
            let w_att = weak.clone();
            let items: Vec<(String, bool)> = self.pending_atts.iter().map(|p| {
                (p.file_name().map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".to_string()),
                 attachment_mime(p).starts_with("image/"))
            }).collect();
            col = col.child(attachment_chips(&t, &items, move |i, _window, cx| {
                let _ = w_att.update(cx, |a, cx| a.remove_attachment(i, cx));
            }));
        }
        let w_send = weak;
        col
            .child(composer_dock(&t, &self.composer, "What are you trying to get done?", !send_enabled))
            .child(composer_toolbar(
                &t,
                &fennec_ui::plugin::default_plugins(),
                self.access_icon(), self.access_label(), self.access_color(&t),
                &self.model_display_name(),
                self.stt_recording,
                send_enabled,
                move |_window, cx| { let _ = w_attach.update(cx, |a, cx| a.attach_file(cx)); },
                move |_window, cx| { let _ = w_perm.update(cx, |a, cx| {
                    a.perm_dropdown_open = !a.perm_dropdown_open;
                    a.model_dropdown_open = false;
                    cx.notify();
                }); },
                move |_window, cx| { let _ = w_model.update(cx, |a, cx| {
                    a.model_dropdown_open = !a.model_dropdown_open;
                    a.perm_dropdown_open = false;
                    cx.notify();
                }); },
                move |window, cx| { let _ = w_mic.update(cx, |a, cx| a.toggle_stt(window, cx)); },
                move |window, cx| { let _ = w_send.update(cx, |a, cx| a.send(window, cx)); },
            ))
    }

    /// Permission + model dropdowns rendered at the window root so they are not
    /// clipped by the centre pane's overflow_hidden.
    fn dropdowns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let weak = cx.entity().downgrade();
        let mut overlay = div();
        if self.perm_dropdown_open {
            let w = weak.clone();
            overlay = overlay.child(fennec_ui::controls::permission_dropdown(
                &t,
                &self.access_mode,
                move |mode, _window, cx| {
                    let _ = w.update(cx, |a, cx| {
                        a.set_access_mode(&mode);
                        a.perm_dropdown_open = false;
                        cx.notify();
                    });
                },
            ));
        }
        if self.model_dropdown_open {
            let w_sel = weak.clone();
            let w_dl = weak.clone();
            let w_effort = weak;
            overlay = overlay.child(fennec_ui::controls::model_dropdown(
                &t,
                self.active_model(),
                self.served_model(),
                move |model, _window, cx| {
                    let _ = w_sel.update(cx, |a, cx| {
                        a.model = model;
                        a.model_dropdown_open = false;
                        a.save_settings();
                        cx.notify();
                    });
                },
                move |window, cx| {
                    window.push_notification(Notification::info("Model download coming soon."), cx);
                    let _ = w_dl.update(cx, |a, cx| {
                        a.model_dropdown_open = false;
                        cx.notify();
                    });
                },
                &self.reasoning_effort,
                move |level, _window, cx| {
                    let _ = w_effort.update(cx, |a, cx| {
                        a.reasoning_effort = level;
                        a.model_dropdown_open = false;
                        a.save_settings();
                        cx.notify();
                    });
                },
            ));
        }
        overlay
    }

    fn attach_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true, directories: false, multiple: true, prompt: None,
        });
        cx.spawn(async move |weak, cx: &mut AsyncApp| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = weak.update(cx, |a, cx| {
                    for p in paths {
                        if !a.pending_atts.iter().any(|s| s == &p) {
                            a.pending_atts.push(p);
                        }
                    }
                    cx.notify();
                });
            }
        }).detach();
    }

    /// Take a picked file back out again, by its position in the chip row.
    fn remove_attachment(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.pending_atts.len() {
            self.pending_atts.remove(index);
            cx.notify();
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fontsize_input = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            s.set_value(self.font_size.to_string(), window, cx);
            s
        });
        let note_title_input = cx.new(|cx| InputState::new(window, cx));
        let note_folder_input = cx.new(|cx| InputState::new(window, cx));
        let note_content_input = cx.new(|cx| InputState::new(window, cx));
        let note_overwrite = Rc::new(Cell::new(false));
        let backend_url = self.server_url.clone();
        let response_tx = self.response_tx.clone();
        let github = self.github.clone();
        let github_repositories = self.github_repositories.clone();
        let obsidian = self.obsidian.clone();
        let capabilities = self.capabilities.clone();
        let weak = cx.entity().downgrade();
        let current_theme: Rc<RefCell<String>> = Rc::new(RefCell::new(self.theme.clone()));
        // The content builder can re-run when the dialog layer is rebuilt, so the
        // switches read their state from a mirror that outlives one build, not from
        // a value captured when the dialog opened.
        let autosave: Rc<Cell<bool>> = Rc::new(Cell::new(self.auto_save));
        let notify: Rc<Cell<bool>> = Rc::new(Cell::new(self.notifications));
        let settings_path_for_theme = self.settings_path.clone();
        let active_tab: Rc<Cell<usize>> = Rc::new(Cell::new(0));

        window.open_dialog(cx, move |dialog, _, _| {
            let fontsize_input = fontsize_input.clone();
            let note_title_input = note_title_input.clone();
            let note_folder_input = note_folder_input.clone();
            let note_content_input = note_content_input.clone();
            let note_overwrite = note_overwrite.clone();
            let backend_url = backend_url.clone();
            let response_tx = response_tx.clone();
            let github = github.clone();
            let github_repositories = github_repositories.clone();
            let obsidian = obsidian.clone();
            let capabilities = capabilities.clone();
            let weak = weak.clone();
            let current_theme = current_theme.clone();
            let settings_path_for_theme = settings_path_for_theme.clone();
            let active_tab = active_tab.clone();
            let autosave = autosave.clone();
            let notify = notify.clone();

            dialog
                .w(px(640.0))
                .title("Settings")
                // The entrance animation drives the surface's alpha, and a
                // dialog left mid-fade reads as see-through over the timeline;
                // `None` paints the final opaque state in one frame.
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                .content({
                    let fontsize_input = fontsize_input.clone();
                    let note_title_input = note_title_input.clone();
                    let note_folder_input = note_folder_input.clone();
                    let note_content_input = note_content_input.clone();
                    let note_overwrite = note_overwrite.clone();
                    let backend_url = backend_url.clone();
                    let response_tx = response_tx.clone();
                    let github = github.clone();
                    let github_repositories = github_repositories.clone();
                    let obsidian = obsidian.clone();
                    let capabilities = capabilities.clone();
                    let weak = weak.clone();
                    let current_theme = current_theme.clone();
                    let settings_path_for_theme = settings_path_for_theme.clone();
                    let active_tab = active_tab.clone();
                    let autosave = autosave.clone();
                    let notify = notify.clone();
                    move |content, _window, cx| {
                        let tab = active_tab.get();
                        let tab_names = ["Appearance", "GitHub", "Obsidian", "Behavior", "Advanced", "Integrations"];
                        // Cards need a definite width, and so does card prose.
                        // gpui resolves `w_full()` and measures a `Label` against
                        // the parent's *padding* box, not its content box: inside
                        // the content area a `w_full()` card came out 468px wide
                        // while only ~454px is available, so it ran 11px past the
                        // dialog's own right border and read as a card with no
                        // right edge. Pinning the width instead keeps every tab's
                        // cards on the same edges as the Close and Save controls.
                        // 608 (row) - 140 (tab column, border included) - 12
                        // (content-area inset) - 12 (right gutter) = 444.
                        const CARD_W: f32 = 444.0;
                        // Card content box: 444 - 2 (`border_1`) - 24 (`p_3`).
                        const PROSE_W: f32 = 418.0;

                        content
                            .flex()
                            .flex_row()
                            // The dialog already insets `.content(...)` by 16px on
                            // each side, so a fixed 640 row overflowed it by 32px and
                            // the card text was clipped mid-word at the border. It also
                            // sizes to its content, so the row needs a definite width:
                            // `w_full()` here collapsed the dialog to min-content.
                            .w(px(608.0))
                            // `.settings-layout { min-height: 400px }`: short panels
                            // hold the floor, tall ones grow and the dialog caps them.
                            .min_h(px(400.0))
                            // Vertical tabs on the left
                            .child(
                                div().w(px(140.0)).flex_shrink_0().p_3().border_r_1().border_color(cx.theme().border)
                                    .flex().flex_col().gap_0p5()
                                    .children(tab_names.iter().enumerate().map(|(i, name)| {
                                        let active_tab = active_tab.clone();
                                        let is_active = tab == i;
                                        let t = cx.theme().clone();
                                        // Not a Button: it hard-codes `justify_center`
                                        // after user styling, so the label stays centred.
                                        div().id(ElementId::Name(format!("settings-tab-{i}").into()))
                                            .w_full().h(px(36.0)).px_3().rounded_lg().cursor_pointer()
                                            .flex().items_center()
                                            .hover(|d| d.bg(fennec_ui::theme::hover()))
                                            .when(is_active, |d| d.bg(t.muted).border_l_2().border_color(t.accent))
                                            .child(
                                                Label::new(*name)
                                                    .id(ElementId::Name(format!("settings-tab-{i}-label").into()))
                                                    .text_size(px(13.0)).text_color(t.foreground),
                                            )
                                            .on_click(move |_, window, _cx| {
                                                active_tab.set(i);
                                                window.refresh();
                                            })
                                    }))
                            )
                            // Content area on the right
                            .child(
                                div().flex_1().h_full().overflow_y_scrollbar().py_2()
                                    .children(match tab {
                                        0 => vec![
                                            // Appearance section
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("Appearance").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Theme").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(div().mt_2().gap_1().flex().flex_col()
                                                        .children({
                                                            let presets = theme_presets();
                                                            let current_theme_cell = current_theme.clone();
                                                            let settings_path_clone = settings_path_for_theme.clone();
                                                            let dialog_theme = cx.theme().clone();
                                                            presets.into_iter().enumerate().map(move |(i, preset)| {
                                                                let settings_path_clone = settings_path_clone.clone();
                                                                let current_theme_cell_click = current_theme_cell.clone();
                                                                let preset_id = preset.id().to_string();
                                                                let preset_name = preset.name;
                                                                let preset_bg = preset.background;
                                                                let is_selected = preset_id == *current_theme_cell.borrow();
                                                                // `.theme-item`: 36px tall, 12px side padding,
                                                                // radius 8, active = surface + the accent edge.
                                                                Button::new(format!("theme-{}", i)).ghost()
                                                                    .w_full().h(px(36.0)).px_3().rounded_lg().justify_start()
                                                                    .when(is_selected, |b| b.bg(dialog_theme.muted).border_l_2().border_color(dialog_theme.accent))
                                                                    .on_click(move |_, window, cx| {
                                                                        preset.apply(cx);
                                                                        *current_theme_cell_click.borrow_mut() = preset.id().to_string();
                                                                        window.refresh();
                                                                        let theme_id = preset.id().to_string();
                                                                        let path = settings_path_clone.clone();
                                                                        std::thread::spawn(move || {
                                                                            if let Ok(contents) = std::fs::read_to_string(&path) {
                                                                                if let Ok(mut settings) = serde_json::from_str::<Settings>(&contents) {
                                                                                    settings.theme = Some(theme_id);
                                                                                    if let Ok(json) = serde_json::to_string_pretty(&settings) {
                                                                                        let _ = std::fs::write(&path, json);
                                                                                    }
                                                                                }
                                                                            }
                                                                        });
                                                                    })
                                                                    .child({
                                                                        let t = dialog_theme.clone();
                                                                        // `.theme-swatch`: a 20px square, radius 4,
                                                                        // hairline border, painted with that theme's
                                                                        // own `--bg` ground.
                                                                        div().flex().items_center().gap_3().w_full()
                                                                            .child(div().w(px(20.0)).h(px(20.0)).rounded_sm().border_1().border_color(t.border)
                                                                                .bg(hsla(preset_bg[0], preset_bg[1], preset_bg[2], 1.0)))
                                                                            .child(Label::new(preset_name).text_size(px(13.0)))
                                                                    })
                                                            })
                                                        })
                                                    )
                                                    .child(Label::new("Select a color theme. Changes apply immediately.")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground).mt_1())
                                                )
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Font Size").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Input::new(&fontsize_input).h(px(36.0)))
                                                    .child(Label::new("UI font size in pixels. Default: 14")
                                                        .text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                )
                                                .into_any_element(),
                                        ],
                                        1 => vec![
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("GitHub").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Account").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new(if github.authenticated {
                                                        github.account.as_ref().map(|a| format!("Connected as @{}", a.login)).unwrap_or_else(|| "GitHub is connected.".into())
                                                    } else {
                                                        github.error.clone().unwrap_or_else(|| "GitHub is not authenticated.".into())
                                                    }).w(px(PROSE_W)).text_size(px(13.0)).text_color(if github.authenticated { cx.theme().green } else { cx.theme().muted_foreground }))
                                                    .child(Label::new("Uses the local GitHub CLI. Fennec never stores or displays your token.")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground)))
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Repositories").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .children(if github_repositories.repositories.is_empty() {
                                                        vec![Label::new(github_repositories.error.clone().unwrap_or_else(|| "No repositories were returned.".into())).w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground).into_any_element()]
                                                    } else {
                                                        // One `.id()` per row: `Label::new` is
                                                        // `#[track_caller]`, so twelve repos built
                                                        // from this one call site would all claim
                                                        // the same a11y id and abort the app.
                                                        github_repositories.repositories.iter().take(12).enumerate().map(|(i, repo)| {
                                                            Label::new(format!("{}{}", repo.full_name, if repo.private { " · private" } else { "" }))
                                                                .id(ElementId::Name(format!("gh-repo-{i}").into()))
                                                                .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().foreground).into_any_element()
                                                        }).collect()
                                                    }))
                                                .into_any_element(),
                                        ],
                                        2 => vec![
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("Obsidian").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Local vault").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new(if obsidian.configured {
                                                        format!("Vault ready: {}", obsidian.vault.clone().unwrap_or_default())
                                                    } else {
                                                        obsidian.message.clone()
                                                    }).w(px(PROSE_W)).text_size(px(13.0)).text_color(if obsidian.configured { cx.theme().green } else { cx.theme().muted_foreground }))
                                                    .child(Label::new("Fennec writes only to the configured vault; Obsidian handles account login and Sync.")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground)))
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Write a note").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new("Title").w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                    .child(Input::new(&note_title_input).h(px(36.0)))
                                                    .child(Label::new("Folder (optional)").w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                    .child(Input::new(&note_folder_input).h(px(36.0)))
                                                    .child(Label::new("Content").w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                    .child(Input::new(&note_content_input).h(px(36.0)))
                                                    .child({
                                                        let note_overwrite = note_overwrite.clone();
                                                        Button::new("obsidian-overwrite").ghost().small().justify_start()
                                                            .label(if note_overwrite.get() { "Replace existing note: On" } else { "Replace existing note: Off" })
                                                            .on_click(move |_, window, _cx| {
                                                                note_overwrite.set(!note_overwrite.get());
                                                                window.refresh();
                                                            })
                                                    })
                                                    .child({
                                                        let note_title_input = note_title_input.clone();
                                                        let note_folder_input = note_folder_input.clone();
                                                        let note_content_input = note_content_input.clone();
                                                        let note_overwrite = note_overwrite.clone();
                                                        let backend_url = backend_url.clone();
                                                        let response_tx = response_tx.clone();
                                                        Button::new("obsidian-write").primary().small().label("Write to Obsidian")
                                                            .on_click(move |_, window, cx| {
                                                                let title = note_title_input.read(cx).value().trim().to_string();
                                                                let folder = note_folder_input.read(cx).value().trim().to_string();
                                                                let content = note_content_input.read(cx).value().to_string();
                                                                if title.is_empty() || content.trim().is_empty() {
                                                                    window.push_notification(Notification::error("Enter a note title and content."), cx);
                                                                    return;
                                                                }
                                                                let folder = (!folder.is_empty()).then_some(folder);
                                                                let overwrite = note_overwrite.get();
                                                                let backend_url = backend_url.clone();
                                                                let response_tx = response_tx.clone();
                                                                std::thread::spawn(move || {
                                                                    let result = CodingAgentClient::with_base(backend_url)
                                                                        .obsidian_write_note(&title, &content, folder.as_deref(), overwrite)
                                                                        .map(|note| note.path);
                                                                    let _ = response_tx.send(BackendResponse::ObsidianWrite(result));
                                                                });
                                                            })
                                                    }))
                                                .into_any_element(),
                                        ],
                                        3 => vec![
                                            // Behavior section
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("Behavior").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child({
                                                        let autosave = autosave.clone();
                                                        let weak = weak.clone();
                                                        Switch::new("auto-save").small().checked(autosave.get())
                                                            .label("Auto-save tasks")
                                                            .on_click(move |checked, window, cx| {
                                                                let on = *checked;
                                                                autosave.set(on);
                                                                let _ = weak.update(cx, |a, _| {
                                                                    a.auto_save = on;
                                                                    // Switching it back on writes what the
                                                                    // off interval accumulated; switching
                                                                    // it off writes nothing new.
                                                                    a.save();
                                                                    a.flush_save();
                                                                    a.save_settings();
                                                                });
                                                                window.refresh();
                                                            })
                                                    })
                                                    .child(Label::new("Automatically save task history to disk")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                )
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child({
                                                        let notify = notify.clone();
                                                        let weak = weak.clone();
                                                        Switch::new("notifications").small().checked(notify.get())
                                                            .label("Desktop notifications")
                                                            .on_click(move |checked, window, cx| {
                                                                let on = *checked;
                                                                notify.set(on);
                                                                let _ = weak.update(cx, |a, _| {
                                                                    a.notifications = on;
                                                                    a.save_settings();
                                                                });
                                                                window.refresh();
                                                            })
                                                    })
                                                    .child(Label::new("Show notifications for task completions")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                )
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Clear History").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(div().flex().w_full().mt_1().child(Button::new("clear-history-btn").label("Clear all task history").ghost().small()
                                                        .on_click({
                                                            let weak = weak.clone();
                                                            move |_, window, cx| {
                                                                let _ = weak.update(cx, |a, cx| {
                                                                    a.clear_history(cx);
                                                                    window.push_notification(Notification::info("History cleared."), cx);
                                                                });
                                                            }
                                                        })))
                                                    .child(Label::new("Remove all tasks and conversations.")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground))
                                                )
                                                .into_any_element(),
                                        ],
                                        4 => vec![
                                            // Advanced section. The PWA also shows Debug mode
                                            // and Experimental features here; neither has a
                                            // native row because no code read either flag.
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("Advanced").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Data Directory").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new(format!("{}", settings_path_for_theme.parent().unwrap_or(&settings_path_for_theme).display()))
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground).font_family("monospace"))
                                                )
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Export Settings").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(div().flex().w_full().mt_1().child(Button::new("export-settings-btn").label("Export to JSON").ghost().small()
                                                        .on_click({
                                                            let weak = weak.clone();
                                                            move |_, window, cx| {
                                                                let _ = weak.update(cx, |a, cx| {
                                                                    a.save_settings();
                                                                    window.push_notification(Notification::info("Settings exported."), cx);
                                                                });
                                                            }
                                                        })))
                                                )
                                                .into_any_element(),
                                        ],
                                        5 => vec![
                                            // `.w_full()` on the column and the cards sizes
                                            // them to the content area; the prose labels need
                                            // `PROSE_W` on top of that, because the card's own
                                            // padding is not subtracted from their measure.
                                            div().flex().flex_col().gap_3().w_full()
                                                .child(Label::new("Integrations").font_weight(FontWeight::SEMIBOLD).text_size(px(16.0)).mb_1())
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Nod / n8n bridge").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new("Native Rust stdio MCP server: fennec-nod").w(px(PROSE_W)).text_size(px(13.0)).text_color(cx.theme().green))
                                                    .child(Label::new("Connect n8n or another MCP host to the local Fennec tools without Node.js or stored credentials.")
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground)))
                                                .child(div().flex().flex_col().gap_2().p_3().w(px(CARD_W)).rounded_lg().bg(cx.theme().muted).border_1().border_color(cx.theme().border)
                                                    .child(Label::new("Backend capability contract").font_weight(FontWeight::SEMIBOLD).text_size(px(12.0)))
                                                    .child(Label::new(capabilities.as_ref().map(|c| format!("{} {} · protocol {}", c.backend, c.version, c.protocol_version)).unwrap_or_else(|| "Backend capabilities are unavailable.".into()))
                                                        .w(px(PROSE_W)).text_size(px(13.0)).text_color(if capabilities.is_some() { cx.theme().green } else { cx.theme().muted_foreground }))
                                                    .child(Label::new(capabilities.as_ref().map(|c| c.features.join(" · ")).unwrap_or_else(|| "Start fennec-agent to load integration capabilities.".into()))
                                                        .w(px(PROSE_W)).text_size(px(11.0)).text_color(cx.theme().muted_foreground)))
                                                .into_any_element(),
                                        ],
                                        _ => vec![],
                                    })
                            )
                    }
                })
                .footer(div().flex().justify_end().gap_2().p_3()
                    .child(Button::new("settings-cancel").label("Cancel").ghost()
                        .on_click(|_, window, cx| { window.close_dialog(cx); }))
                    .child(Button::new("settings-save").label("Save").primary()
                        .on_click({
                            let fontsize_input = fontsize_input.clone();
                            let weak = weak.clone();
                            move |_, window, cx| {
                                let font_size_str = fontsize_input.read(cx).value().trim().to_string();
                                let font_size = font_size_str.parse::<f32>().unwrap_or(14.0);

                                let _ = weak.update(cx, |a, cx| {
                                    a.font_size = font_size;
                                    a.save_settings();
                                    a.fetch_backend_info();
                                    cx.notify();
                                });
                                window.close_dialog(cx);
                            }
                        })))
        });
    }

    /// The bytes behind a delivered-file card, read at the click. Capped, so a
    /// log the size of a disk cannot outgrow the pane it opens in or the RAM the
    /// run still needs.
    fn show_delivered(&mut self, path: String, name: String) {
        const MAX_VIEW_BYTES: u64 = 200 * 1024;
        let read = std::fs::File::open(&path).and_then(|mut f| -> std::io::Result<(Vec<u8>, u64)> {
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            let mut kept = Vec::new();
            let mut limited = std::io::Read::take(&mut f, MAX_VIEW_BYTES);
            std::io::Read::read_to_end(&mut limited, &mut kept)?;
            Ok((kept, size))
        });
        let (body, meta, failed) = match read {
            Ok((kept, size)) => {
                let mut text = String::from_utf8_lossy(&kept).into_owned();
                if (size as usize) > kept.len() {
                    text.push_str("\n…[truncated — the file is past the 200 KB view limit]");
                }
                let lines = if text.is_empty() {
                    0
                } else {
                    text.trim_end_matches('\n').matches('\n').count() + 1
                };
                (
                    text,
                    format!("{lines} {} · {}", if lines == 1 { "line" } else { "lines" }, human_size(size)),
                    false,
                )
            }
            Err(e) => (String::new(), e.to_string(), true),
        };
        self.open_file = Some(FileView { path, name, body, meta, failed, image: None });
        // A pane the user had hidden would show nothing; opening it is part of
        // the click that asked to see the file. The viewer lives on the Files tab
        // now, so the card has to reveal the tab it is on or the click reads as
        // doing nothing.
        self.inspector = true;
        self.insp_tab = InspTab::Files;
    }

    /// Switch the inspector to one of its three views. Files asks for its first
    /// listing when it is opened rather than at launch: the loop may not be up
    /// yet, and a listing nobody asked for is a read of the disk.
    fn set_insp_tab(&mut self, tab: InspTab, cx: &mut Context<Self>) {
        let changed = self.insp_tab != tab;
        self.insp_tab = tab;
        if tab == InspTab::Files && !self.fm.loaded {
            let path = self.fm.path.clone();
            self.load_fm(path);
        }
        if changed {
            self.save_settings();
        }
        cx.notify();
    }

    /// One directory of the folder the runs work in, asked of the loop so the
    /// jail that refuses a run's out-of-tree path refuses the pane too.
    fn load_fm(&mut self, path: String) {
        self.fm_seq += 1;
        let seq = self.fm_seq;
        self.fm.path = path.clone();
        self.fm.loaded = true;
        self.fm.rows.clear();
        self.fm.note = "reading…".to_string();
        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        let workspace = workspace_param(&self.core_workspace());
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);
            let result = client.work_files(&path, &workspace).and_then(decode_listing);
            let _ = tx.send(BackendResponse::WorkFiles { seq, result });
        });
    }

    /// The folder a listed entry lives in, in the slash form the jail accepts.
    fn fm_child(&self, name: &str) -> String {
        let base = self.fm.path.trim_end_matches(['\\', '/']);
        if base.is_empty() || base == "." { name.to_string() } else { format!("{base}/{name}") }
    }

    fn fm_up(&mut self) {
        if self.fm.path == "." || self.fm.path.is_empty() {
            return;
        }
        let parts: Vec<&str> = self.fm.path.trim_end_matches(['\\', '/']).split(['/', '\\']).collect();
        let parent = if parts.len() > 1 { parts[..parts.len() - 1].join("/") } else { ".".into() };
        self.load_fm(parent);
    }

    /// A listed file opens in the viewer a delivered card uses — one panel, and
    /// the same cap on what it can hold — but read through the jail route.
    fn open_fm_file(&mut self, path: String, name: String) {
        self.file_seq += 1;
        let seq = self.file_seq;
        let fallback = jail_path(&self.fm.root, &path);
        self.open_file = Some(FileView {
            path: fallback.clone(), name: name.clone(), body: String::new(),
            meta: "reading…".to_string(), failed: false, image: None,
        });
        // The block is pinned above the scrolling list, so it is on screen the
        // moment a file opens, however far the listing below it is scrolled.
        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        let workspace = workspace_param(&self.core_workspace());
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);
            let result = client.work_file(&path, &workspace).and_then(decode_file);
            let _ = tx.send(BackendResponse::WorkFile { seq, name, fallback, result });
        });
    }

    fn browse_go(&mut self, raw: String) {
        let target = raw.trim().to_string();
        if target.is_empty() {
            return;
        }
        self.br_seq += 1;
        let seq = self.br_seq;
        if self.br.hist.last().map(String::as_str) != Some(target.as_str()) {
            self.br.hist.push(target.clone());
        }
        self.br.target = target.clone();
        self.br.meta = "reading…".to_string();
        self.br.title.clear();
        self.br.body.clear();
        let url = self.server_url.clone();
        let tx = self.response_tx.clone();
        std::thread::spawn(move || {
            let client = CodingAgentClient::with_base(url);
            let result = client.browse(&target).and_then(decode_page);
            let _ = tx.send(BackendResponse::Browse { seq, fallback: target, result });
        });
    }

    /// ← over the addresses the user read. Nothing here needs the bound the PWA
    /// puts on its frame: this pane never runs a page's scripts, so the only
    /// navigations it can make are the ones asked for by hand.
    fn browse_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.br.hist.len() < 2 {
            return;
        }
        self.br.hist.pop();
        let Some(prev) = self.br.hist.last().cloned() else { return };
        self.br_input.update(cx, |input, cx| input.set_value(prev.clone(), window, cx));
        self.browse_go(prev);
    }

    fn right(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let working = self.working();
        let elapsed = self.elapsed();
        let skills = self.skills();
        let sources = self.sources();
        let task_title = self.task().map(|tk| tk.title.clone()).unwrap_or_else(|| "No active task".into());

        // Inspectable items for the right panel, collected from the task msgs.
        use fennec_ui::inspector::{InspectItem, InspectSection, inspect_section};
        let msgs = self.task().map(|tk| tk.msgs.clone()).unwrap_or_default();
        let mut links: Vec<String> = Vec::new();
        for m in &msgs {
            for l in fennec_ui::inspector::extract_links(&m.content) {
                if !links.contains(&l) { links.push(l); }
            }
        }
        let sub_items: Vec<InspectItem> = self.agents.iter().skip(1).enumerate().map(|(i, a)| InspectItem { id: format!("{i}"), title: a.clone(), subtitle: "delegated agent".into(), detail: String::new(), url: None }).collect();
        // A process is a command with a live process behind it. Once the call
        // returns there is no process left to watch, so the section empties
        // itself even though the timeline keeps the row.
        let mut proc_items: Vec<InspectItem> = Vec::new();
        for m in msgs.iter().filter(|m| m.kind == MsgKind::Action) {
            let Some(meta) = m.meta.as_ref() else { continue };
            let Some(title) = meta.title.as_ref() else { continue };
            if !fennec_ui::inspector::title_is_command(title) { continue; }
            let running = meta.duration.is_none();
            if !running { continue; }
            proc_items.push(InspectItem {
                id: format!("{}", proc_items.len()),
                title: title.clone(),
                subtitle: "running in background".into(),
                detail: m.content.clone(),
                url: None,
            });
        }
        let skill_items: Vec<InspectItem> = skills.iter().enumerate().map(|(i, s)| InspectItem { id: format!("{i}"), title: s.clone(), subtitle: "skill".into(), detail: String::new(), url: None }).collect();
        let art_items: Vec<InspectItem> = msgs.iter().filter(|m| m.kind == MsgKind::Attach || m.kind == MsgKind::Edit).enumerate().map(|(i, m)| InspectItem { id: format!("{i}"), title: m.meta.as_ref().and_then(|x| x.title.clone()).unwrap_or_else(|| "file".into()), subtitle: m.meta.as_ref().and_then(|x| x.path.clone()).unwrap_or_default(), detail: m.content.clone(), url: None }).collect();
        let web_items: Vec<InspectItem> = links.iter().enumerate().map(|(i, l)| InspectItem { id: format!("{i}"), title: l.clone(), subtitle: "linked reading".into(), detail: String::new(), url: Some(l.clone()) }).collect();
        let src_items: Vec<InspectItem> = sources.iter().enumerate().map(|(i, s)| InspectItem { id: format!("{i}"), title: s.clone(), subtitle: "file inspected".into(), detail: String::new(), url: None }).collect();
        let repo = self.repo_label();
        let branch = if self.branch.is_empty() { "—".to_string() } else { self.branch.clone() };
        // The browser lanes get one row each, in the order core reported them.
        // `fennec_client::outlet_rows` words them so this shell, the egui one and
        // the PWA describe the same probe in the same sentence, and so a loop that
        // has never answered gets one honest row instead of three refusals.
        let mut outlet_rows_ui = Vec::new();
        for (ok, key, detail) in outlet_rows(self.outlets_known, &self.outlets) {
            outlet_rows_ui.push(self.row(key, &detail, ok, cx));
        }

        let agents_count = if working { "1 online" } else { "0 online" };
        // `.file-viewer`: the delivered file whose card was clicked, opened here
        // beside the rest of the run's context instead of in an app outside it.
        let viewed = self.open_file.clone();
        let file_sec = viewed.as_ref().map(|f| {
            let name = f.name.clone();
            let path = f.path.clone();
            let meta = f.meta.clone();
            let body = f.body.clone();
            let note = if f.failed { t.red } else { t.muted_foreground };
            // One frame, two contents. A picture is fitted inside the box and the
            // 300 px ceiling keeps a tall screenshot from pushing the rest of the
            // pane off screen — the reason the PWA caps its `<img>` at 45vh. Text
            // keeps scrolling instead, and no image is asked to scroll.
            let frame: AnyElement = match f.image.clone() {
                Some(image) => div().rounded_md().border_1().border_color(t.border).bg(t.muted)
                    .p_2().child(img(ImageSource::Image(image)).w_full().max_h(px(300.0))
                        .object_fit(ObjectFit::Contain)).into_any_element(),
                None => div().rounded_md().border_1().border_color(t.border).bg(t.muted)
                    .max_h(px(320.0)).overflow_y_scrollbar().p_2()
                    .child(Label::new(body).font_family("monospace").text_size(px(11.0))
                        .text_color(t.foreground)).into_any_element(),
            };
            div().id("file-view").flex().flex_col().gap_1()
                .child(div().id("file-head").flex().items_center().justify_between().mb_1()
                    .child(Label::new("FILE").text_size(px(10.0)).font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.muted_foreground))
                    .child(Button::new("file-close").ghost().xsmall().w(px(24.0)).h(px(24.0))
                        .rounded_md().border_1().border_color(t.border).justify_center()
                        .child(Label::new("×").text_size(px(14.0)).text_color(t.foreground))
                        .on_click(cx.listener(|a, _, _, cx| { a.open_file = None; cx.notify(); }))))
                .child(Label::new(name).text_size(px(13.0)).font_weight(FontWeight::SEMIBOLD))
                .child(Label::new(path).font_family("monospace").text_size(px(10.5))
                    .text_color(t.muted_foreground))
                .child(Label::new(meta).text_size(px(11.0)).text_color(note))
                .child(frame)
        });
        // The three views live in one column; only the open one is built, so a
        // folder with a thousand rows costs nothing while the user reads the run.
        let mut tabs: Vec<AnyElement> = Vec::new();
        for tab in InspTab::ALL {
            let active = self.insp_tab == tab;
            tabs.push(
                div().id(ElementId::Name(format!("insp-tab-{}", tab.as_str()).into()))
                    .px_2().py_1().rounded_md().cursor_pointer()
                    .when(active, |d| d.bg(t.muted))
                    .child(Label::new(tab.label()).text_size(px(11.5))
                        .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                        .text_color(if active { t.foreground } else { t.muted_foreground }))
                    .on_click(cx.listener(move |a, _, _, cx| { a.set_insp_tab(tab, cx); }))
                    .into_any_element(),
            );
        }

        let mut fm_rows: Vec<AnyElement> = Vec::new();
        if self.insp_tab == InspTab::Files {
            for entry in self.fm.rows.iter() {
                let path = self.fm_child(&entry.name);
                let label = entry.name.clone();
                let name = entry.name.clone();
                let is_dir = entry.is_dir;
                let size = if is_dir { String::new() } else { human_size(entry.size) };
                fm_rows.push(
                    div().id(ElementId::Name(format!("fm-row-{path}").into()))
                        .flex().items_center().gap_2().px_2().py_1().rounded_md().cursor_pointer()
                        .hover(|d| d.bg(t.muted))
                        .on_click(cx.listener(move |a, _, _, cx| {
                            if is_dir {
                                a.load_fm(path.clone());
                            } else {
                                a.open_fm_file(path.clone(), label.clone());
                            }
                            cx.notify();
                        }))
                        .child(ui_icon(if is_dir { "workspace" } else { "file" }, 13.0,
                            if is_dir { t.accent } else { t.muted_foreground }))
                        .child(div().flex_1().overflow_hidden().child(
                            Label::new(name).text_size(px(12.0)).text_color(t.foreground)))
                        .when(!size.is_empty(), |d| d.child(
                            Label::new(size).font_family("monospace").text_size(px(10.5))
                                .text_color(t.muted_foreground).flex_shrink_0()))
                        .into_any_element(),
                );
            }
        }

        let overview = div().flex().flex_col().gap_4()
            // `.agent-item`: the one agent that owns the run, else a note.
            .child(div().flex().flex_col().gap_2()
                .child(insp_head("Working now", Some(agents_count), &t))
                .when(working, |s| s.child(div().p_2().rounded_md().bg(t.muted)
                    .flex().items_center().gap_3()
                    .child(brand_fox(&t, 28.0))
                    .child(div().flex().flex_col().flex_1().overflow_hidden()
                        .child(Label::new("Fennec").text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD))
                        .child(Label::new(task_title.clone()).text_size(px(12.0))
                            .text_color(t.muted_foreground))
                        .child(Label::new(format!("Running · {elapsed}")).text_size(px(11.0))
                            .text_color(t.accent)))))
                .when(!working, |s| s.child(Label::new("Nothing is running right now.")
                    .text_size(px(11.0)).text_color(t.muted_foreground))))
            // `.workspace-context`: one caption, then the six rows the PWA
            // shows under it, each saying what it holds rather than a bare count.
            .child(div().flex().flex_col().gap_1()
                .child(insp_head("At a glance", None, &t))
                .child(inspect_section(&t, InspectSection { id: "subagents", icon: "subagents", label: "Subagents", summary: "No one else is helping yet".into(), items: sub_items }))
                .child(inspect_section(&t, InspectSection { id: "processes", icon: "clock", label: "Processes", summary: "Nothing running in the background".into(), items: proc_items }))
                .child(inspect_section(&t, InspectSection { id: "skills", icon: "tool", label: "Skills", summary: "Nothing used yet".into(), items: skill_items }))
                .child(inspect_section(&t, InspectSection { id: "artifacts", icon: "file", label: "Artifacts", summary: "0 artifacts".into(), items: art_items }))
                .child(inspect_section(&t, InspectSection { id: "web", icon: "search", label: "Web readings", summary: "No readings yet".into(), items: web_items }))
                .child(inspect_section(&t, InspectSection { id: "sources", icon: "queue", label: "Sources", summary: "No files opened yet".into(), items: src_items })))
            .child(div().flex().flex_col().gap_2()
                .child(insp_head("Environment", None, &t))
                .child(self.row("Repository", &repo, false, cx))
                .child(self.row("Branch", &branch, !self.branch.is_empty(), cx))
                .child(self.row("Backend", if self.backend_ok { "Connected" } else { "Offline" }, self.backend_ok, cx))
                .children(outlet_rows_ui));

        let files = div().flex().flex_col().gap_3()
            .child(div().flex().items_center().gap_2()
                .child(Button::new("fm-up").ghost().xsmall().w(px(24.0)).h(px(24.0))
                    .rounded_md().border_1().border_color(t.border).justify_center()
                    .child(Label::new("↑").text_size(px(13.0)).text_color(t.foreground))
                    .on_click(cx.listener(|a, _, _, cx| { a.fm_up(); cx.notify(); })))
                .child(div().flex_1().overflow_hidden().flex().flex_col()
                    .child(Label::new(self.fm.path.clone()).font_family("monospace")
                        .text_size(px(11.5)).text_color(t.foreground))
                    .when(!self.fm.root.is_empty(), |d| d.child(
                        Label::new(self.fm.root.clone()).font_family("monospace")
                            .text_size(px(10.0)).text_color(t.muted_foreground))))
                .child(Button::new("fm-refresh").ghost().xsmall().w(px(24.0)).h(px(24.0))
                    .rounded_md().border_1().border_color(t.border).justify_center()
                    .child(Label::new("⟳").text_size(px(13.0)).text_color(t.foreground))
                    .on_click(cx.listener(|a, _, _, cx| {
                        let path = a.fm.path.clone();
                        a.load_fm(path);
                        cx.notify();
                    }))))
            .children(fm_rows)
            .when(!self.fm.note.is_empty(), |d| d.child(
                Label::new(self.fm.note.clone()).text_size(px(11.0))
                    .text_color(t.muted_foreground)));

        let browser = div().flex().flex_col().gap_2()
            .child(div().flex().items_center().gap_2()
                .child(Button::new("br-back").ghost().xsmall().w(px(24.0)).h(px(28.0))
                    .rounded_md().border_1().border_color(t.border).justify_center()
                    .child(Label::new("←").text_size(px(13.0))
                        .text_color(if self.br.hist.len() > 1 { t.foreground } else { t.muted_foreground }))
                    .on_click(cx.listener(|a, _, window, cx| {
                        a.browse_back(window, cx);
                        cx.notify();
                    })))
                // Clicks landing on the wrapper miss the editor's own hitbox
                // inside the scroll container, so focus the field explicitly.
                .child(div().id("br-address").flex_1()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                        this.br_input.update(cx, |state, cx| window.focus(&state.focus_handle(cx), cx));
                    }))
                    .child(Input::new(&self.br_input).h(px(28.0))))
                .child(Button::new("br-go").label("Go").ghost().xsmall().h(px(28.0))
                    .rounded_md().border_1().border_color(t.border)
                    .text_color(t.accent)
                    .on_click(cx.listener(|a, _, _, cx| {
                        let target = a.br_input.read(cx).value().to_string();
                        a.browse_go(target);
                        cx.notify();
                    }))))
            .when(!self.br.meta.is_empty(), |d| d.child(
                Label::new(self.br.meta.clone()).font_family("monospace").text_size(px(10.5))
                    .text_color(t.muted_foreground)))
            .when(!self.br.title.is_empty(), |d| d.child(
                Label::new(self.br.title.clone()).text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)))
            .when(!self.br.body.is_empty(), |d| d.child(
                div().rounded_md().border_1().border_color(t.border).bg(t.muted)
                    .max_h(px(420.0)).overflow_y_scrollbar().p_2()
                    .child(Label::new(self.br.body.clone()).font_family("monospace")
                        .text_size(px(11.0)).text_color(t.foreground))))
            .when(self.br.body.is_empty() && self.br.meta.is_empty(), |d| d.child(
                Label::new("Type an address and press Enter to read a page here — the same text the loop sees.")
                    .text_size(px(11.0)).text_color(t.muted_foreground)));

        let body: AnyElement = match self.insp_tab {
            InspTab::Overview => overview.into_any_element(),
            InspTab::Files => files.into_any_element(),
            InspTab::Browser => browser.into_any_element(),
        };

        // Pinned between the tabs and the scrolling body, so the preview of the
        // file just opened stays on screen however far the listing below it is
        // scrolled — the shell cannot ask that container to scroll, because a
        // tracked scroll container that grows to its content reports no range.
        let preview = if self.insp_tab == InspTab::Files {
            file_sec.map(|sec| div().px_4().pb_2().flex_shrink_0().child(sec))
        } else {
            None
        };

        div().w(px(self.inspector_width)).flex_shrink_0().bg(fennec_ui::theme::panel())
            .border_l_1().border_color(t.border)
            .flex().flex_col().overflow_hidden()
            .child(div().flex().items_center().gap_1().px_4().pt_4().pb_2().flex_shrink_0()
                .children(tabs))
            .children(preview)
            .child(div().id("inspector-scroll").flex_1().flex().flex_col().p_4().overflow_y_scrollbar().child(body))
    }

    /// `use<>` keeps the returned element from capturing the borrow of `cx` and of
    /// the two `&str`s: an env row owns its theme copy and its text, so the caller
    /// can hold a row per outlet in a `Vec` while still calling `cx` for the rest of
    /// the inspector.
    fn row(&self, label: &str, value: &str, ok: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.theme().clone();
        div().id(ElementId::Name(label.to_string().into()))
            .flex().items_start().justify_between().gap_2()
            .py_1().border_b_1().border_color(t.border)
            .child(Label::new(label).text_size(px(12.0))
                .text_color(t.muted_foreground).flex_shrink_0())
            // The value is the half that can be long - an interpreter path, an
            // outlet detail - so it takes the remaining width and wraps inside it.
            // A block child, not a second flex row: as a flex item the text keeps
            // its natural width and runs past the panel edge. Nothing is cut - the
            // row that hid a path said less than the path.
            .child(div().flex_1().child(
                Label::new(value).font_family("monospace").text_size(px(11.0)).text_right()
                    .text_color(if ok { t.green } else { t.muted_foreground })))
    }
}

fn main() {
    // One data directory for the one desktop app. This shell lived under the
    // coding-agent's name before the two windows merged, so a first launch here
    // adopts that folder rather than opening on an empty task list.
    let root = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    let own = root.join("fennec-desktop");
    let legacy = root.join("fennec-coding-agent");
    let data_dir = if own.join("history.json").exists() || !legacy.join("history.json").exists() {
        own
    } else {
        legacy
    };
    std::fs::create_dir_all(&data_dir).ok();
    let path = data_dir.join("history.json");
    let settings_path = data_dir.join("settings.json");

    application().with_assets(fennec_ui::icons::FennecAssets).run(move |cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        let bounds = Bounds::centered(None, size(px(1400.0), px(900.0)), cx);
        
        let settings_path_clone = settings_path.clone();
        cx.open_window(WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                // The caption itself is hidden, but the platform still shows this
                // in the taskbar, the window switcher and any window picker.
                title: Some("Fennec".into()),
                appears_transparent: true,
                ..Default::default()
            }),
            ..Default::default()
        }, |window, cx| {
            let composer = cx.new(|cx| InputState::new(window, cx));
            let search = cx.new(|cx| InputState::new(window, cx));
            let br_input = cx.new(|cx| InputState::new(window, cx));
            let repo_create_input = cx.new(|cx| InputState::new(window, cx));
            let view = cx.new(|cx| {
                let mut app = App::new(path, settings_path_clone.clone(), composer, search, br_input, repo_create_input, window, cx);
                app.load();
                app.resume_runs();
                app
            });
            cx.new(|cx| Root::new(view, window, cx))
        }).expect("Failed to open window");
        
        // gpui-kit's own dark is a neutral ground with a blue accent; every
        // colour the shell draws comes from a preset, so a fresh install with no
        // settings.json yet must still land on "dark" rather than the built-in.
        let saved_theme = std::fs::read_to_string(&settings_path).ok()
            .and_then(|c| serde_json::from_str::<Settings>(&c).ok())
            .and_then(|s| s.theme)
            .unwrap_or_else(|| "dark".into());
        if let Some(preset) = theme_presets().get(find_preset_index(&saved_theme)) {
            preset.apply(cx);
        }
    });
}

#[cfg(test)]
mod fold_tests {
    // Named imports, not `use super::*`: the glob pulls gpui-kit's own `test`
    // macro into scope and `#[test]` then expands against it forever.
    use crate::{fold_calls, fold_run, Msg, MsgKind, MsgMeta, RowOpen};
    use fennec_ui::timeline::{block_title, live_stretch};

    fn msg(id: u64, kind: MsgKind, content: &str) -> Msg {
        Msg { id, author: "Fennec".into(), content: content.into(), kind, time: String::new(), collapsed: true, meta: None }
    }

    fn done_call(id: u64) -> Msg {
        Msg {
            id, author: "Fennec".into(), content: "7 files".into(), kind: MsgKind::Action,
            time: String::new(), collapsed: true,
            meta: Some(MsgMeta {
                duration: Some("1.2s".into()), title: Some("CMD · Run command ls".into()),
                add: None, del: None, path: None, size: None, verb: None, root: None, call_id: None,
            }),
        }
    }

    /// The loop retires the "Thinking…" placeholder as soon as real text arrives,
    /// which used to swap the live block's key and orphan whatever the user had
    /// just clicked open. The prompt it answers never moves, so the key holds.
    #[test]
    fn a_live_block_keeps_its_key_when_the_placeholder_retires() {
        let with = vec![msg(1, MsgKind::User, "Count the files"), msg(2, MsgKind::Think, "Thinking…"), done_call(3)];
        let without = vec![msg(1, MsgKind::User, "Count the files"), done_call(3)];
        let quiet = fennec_ui::timeline::ProgressLines::default();
        let before = fold_calls(&with, true, &quiet).at[&1].id;
        let after = fold_calls(&without, true, &quiet).at[&1].id;
        assert_eq!(before, after);
    }

    /// The reasoning is a row of the block it was thought in, between the calls
    /// around it, and it stays after the answer lands: the block is where the run
    /// thought it. It is counted in no bucket, and its card is hidden so the same
    /// words never read twice.
    #[test]
    fn a_settled_block_keeps_its_thoughts_where_they_were_thought() {
        let msgs = vec![
            msg(1, MsgKind::User, "Count the files"),
            msg(2, MsgKind::Think, "I should list the directory first, then count."),
            done_call(3),
            msg(4, MsgKind::Think, "The count is the answer now."),
            msg(5, MsgKind::Result, "7 files"),
        ];
        let folds = fold_calls(&msgs, false, &fennec_ui::timeline::ProgressLines::default());
        let b = &folds.at[&1];
        let labels: Vec<&str> = b.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["I should list the directory first, then …", "Run command ls", "The count is the answer now."]);
        assert_eq!(block_title(&b.rows, false, 0), "Ran 1 tool");
        assert!(folds.hidden.contains(&1) && folds.hidden.contains(&3), "a thought the block took still shows as a card too");
        match &b.acts[0] {
            RowOpen::Text(title, text) => {
                assert_eq!(title, "Thinking");
                assert_eq!(text, "I should list the directory first, then count.");
            }
            _ => panic!("a thought row opens a file, not its reasoning"),
        }
    }

    /// Two runs in one task answer different prompts, so their blocks must not
    /// share a key — opening one cannot open the other.
    #[test]
    fn runs_in_one_task_get_different_keys() {
        let msgs = vec![
            msg(1, MsgKind::User, "First"), done_call(2), msg(3, MsgKind::Result, "Answer"),
            msg(4, MsgKind::User, "Second"), done_call(5), msg(6, MsgKind::Result, "Answer"),
        ];
        let folds = fold_calls(&msgs, false, &fennec_ui::timeline::ProgressLines::default());
        assert_ne!(folds.at[&1].id, folds.at[&4].id);
    }

    /// A `queue` step is the loop handing work to the window's queue, not to the
    /// transcript. The wording arrives whole: these are prompts the agent will
    /// run next, and a clipped one is a different instruction.
    #[test]
    fn a_queue_step_goes_to_the_queue_and_nowhere_else() {
        use crate::{BackendResponse, CoreEvent, translate_step};
        use serde_json::json;
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        let step = json!({"type": "queue", "i": 4, "t": 1.0,
                          "items": ["add the regression test the run skipped", "run cargo test --offline"]});
        translate_step(&tx, 7, &step, "D:/Fennec-Suite");
        match rx.recv().expect("the queue step reached the window") {
            BackendResponse::Core(CoreEvent::Queue(items)) => assert_eq!(
                items,
                vec!["add the regression test the run skipped", "run cargo test --offline"]),
            other => panic!("a queue step became {other:?}, not a queue event"),
        }
        // Nothing to queue sends nothing, rather than an empty strip.
        translate_step(&tx, 7, &json!({"type": "queue", "items": []}), "D:/Fennec-Suite");
        assert!(rx.try_recv().is_err());
    }

    /// A call the loop is still inside, as its `tool_start` left it: no duration,
    /// so nothing folds it, and filed under the id a later report names.
    fn running_call(id: u64, call_id: &str) -> Msg {
        Msg {
            id, author: "Fennec".into(), content: r#"{"goal":"book it"}"#.into(),
            kind: MsgKind::Action, time: String::new(), collapsed: true,
            meta: Some(MsgMeta {
                duration: None, title: Some("WEB · Run browser task book it".into()),
                add: None, del: None, path: None, size: None, verb: None, root: None,
                call_id: Some(call_id.into()),
            }),
        }
    }

    /// The line of the call a live block is waiting on rides in that block, in
    /// the loop's own words, and leaves when the card settles.
    #[test]
    fn a_live_block_carries_the_line_of_the_call_it_is_waiting_on() {
        let mut lines = fennec_ui::timeline::ProgressLines::default();
        lines.record("call_7", "Deciding step 3 of 12 3/12");
        let msgs = vec![msg(1, MsgKind::User, "Book it"), running_call(2, "call_7")];
        let (start, end) = (1, msgs.len());
        // Only the last stretch ticks, and it is the only one a live line rides:
        // an older call that never got its answer is history.
        assert!(live_stretch(true, end, msgs.len()));
        let b = fold_run(&msgs[start..end], msgs[start].id, true, &lines);
        let live: Vec<&str> = b.live_rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(live, ["Deciding step 3 of 12 3/12"]);
        assert!(b.live_rows[0].live);
        // The in-flight call itself contributes no settled row to count, and no
        // click target: the line is not a record of a call that returned.
        assert!(b.rows.is_empty(), "a running call has no row of its own yet");
        assert!(b.acts.is_empty());
        // Once the run is history the line says nothing true, so it is not drawn.
        assert!(fold_run(&msgs[start..end], msgs[start].id, false, &lines).live_rows.is_empty());
        // And a line about a call this stretch never held draws nothing.
        let other = vec![msg(1, MsgKind::User, "Book it"), running_call(2, "call_8")];
        assert!(fold_run(&other[1..2], 1, true, &lines).live_rows.is_empty());
    }

    /// The loop's `progress` step as this shell takes it in: the sentence and its
    /// meter, sent under the id of the card it belongs to, and nothing at all for
    /// a step that cannot be filed under one.
    #[test]
    fn a_progress_step_becomes_one_line_for_one_card() {
        use crate::{BackendResponse, CoreEvent, translate_step};
        use serde_json::json;
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        translate_step(&tx, 7, &json!({"type": "progress", "name": "web_task", "call_id": "call_7",
                                       "text": "Deciding step 3 of 12", "step": 3, "of": 12,
                                       "i": 7, "t": 41.2}), "D:/Fennec-Suite");
        let first = match rx.recv().expect("the report reached the window") {
            BackendResponse::Core(CoreEvent::Progress { task_id, id, line }) => {
                assert_eq!((task_id, id), (7, "call_7".to_string()));
                line
            }
            other => panic!("a progress step became {other:?}, not a live line"),
        };
        assert_eq!(first, "Deciding step 3 of 12 3/12");
        // A sentence too long for a row is still the whole sentence.
        let long = "x".repeat(900);
        translate_step(&tx, 7, &json!({"type": "progress", "call_id": "call_7", "text": &long}),
                       "D:/Fennec-Suite");
        match rx.recv().expect("the long report reached the window") {
            BackendResponse::Core(CoreEvent::Progress { line, .. }) => assert_eq!(line, long),
            other => panic!("a progress step became {other:?}, not a live line"),
        }
        // Either counter alone is not a position, so no meter is put there.
        translate_step(&tx, 7, &json!({"type": "progress", "call_id": "call_7",
                                       "text": "Typing into the search box", "step": 4}),
                       "D:/Fennec-Suite");
        match rx.recv().expect("the second report reached the window") {
            BackendResponse::Core(CoreEvent::Progress { id, line, .. }) => {
                assert_eq!((id.as_str(), line.as_str()), ("call_7", "Typing into the search box"));
            }
            other => panic!("a progress step became {other:?}, not a live line"),
        }
        for orphan in [
            json!({"type": "progress", "text": "no call id"}),
            json!({"type": "progress", "call_id": "", "text": "empty call id"}),
            json!({"type": "progress", "call_id": "call_7"}),
            json!({"type": "progress", "call_id": "call_7", "text": null}),
            json!({"type": "progress", "call_id": "call_7", "text": 7}),
            json!({"type": "progress", "call_id": "call_7", "text": ""}),
        ] {
            translate_step(&tx, 7, &orphan, "D:/Fennec-Suite");
            assert!(rx.try_recv().is_err(), "{orphan} should send nothing");
        }
    }
    /// The complaint the line answers is a long call with nothing but a Running
    /// card, so a block has to be able to carry its line before any settled row
    /// exists — and without counting the line as something the run ran.
    #[test]
    fn a_progress_line_opens_a_block_that_held_nothing_and_counts_nothing() {
        let mut lines = fennec_ui::timeline::ProgressLines::default();
        // A stretch of one call that is still in flight: nothing settled in it,
        // so on its own it opens no block, exactly as it always did.
        let quiet = fennec_ui::timeline::ProgressLines::default();
        let msgs = vec![running_call(2, "call_7")];
        assert!(fold_run(&msgs, 1, true, &quiet).rows.is_empty());
        assert!(fold_run(&msgs, 1, true, &quiet).live_rows.is_empty());
        // The loop says what the call is doing, and the block has something to show.
        lines.record("call_7", "Waiting for the page to answer");
        let b = fold_run(&msgs, 1, true, &lines);
        assert!(b.rows.is_empty() && !b.live_rows.is_empty());
        assert_eq!(b.live_rows[0].label, "Waiting for the page to answer");
        // The header keeps ticking rather than settling into a count.
        assert_eq!(block_title(&b.rows, true, 41), "Running · 00:41");
        // A settled block counts the calls only: the same rows read the same
        // whether or not some call was reporting while it ran.
        let done = vec![done_call(2)];
        let alone = fold_run(&done, 1, false, &quiet);
        let talked = fold_run(&done, 1, false, &lines);
        assert_eq!(block_title(&alone.rows, false, 0), "Ran 1 tool");
        assert_eq!(block_title(&talked.rows, false, 0), "Ran 1 tool");
        assert_eq!(alone.rows.len(), talked.rows.len());
        // `fold_calls` only draws a block with rows or a live line to show, so a
        // call whose report arrived does open one where it would not have; and it
        // hides that call's own card, because the block now says both things.
        let whole = vec![msg(1, MsgKind::User, "Book it"), running_call(2, "call_7")];
        assert!(fold_calls(&whole, true, &quiet).at.is_empty());
        assert!(!fold_calls(&whole, true, &quiet).hidden.contains(&1),
                "a call with nothing to report keeps its own Running card");
        let folds = fold_calls(&whole, true, &lines);
        assert!(!folds.at.is_empty());
        assert!(folds.hidden.contains(&1), "the block carries the line, not two rows");
    }
}

/// The right pane's Files and Browser tabs read through the same two work routes
/// and the same browse route as the PWA, so the shapes they agree on are pinned
/// here rather than left to whatever the server happens to answer.
#[cfg(test)]
mod pane_tests {
    // Named imports, not `use super::*`: the glob pulls gpui-kit's own `test`
    // macro into scope and `#[test]` then expands against it forever.
    use crate::{decode_file, decode_listing, decode_page, jail_path, workspace_param, BrState, FmState, InspTab};
    use serde_json::json;

    /// The pane's stored choice has to survive a restart, and the stored spelling
    /// is the PWA's `fennec-insp-tab` value, so one settings file opened in the
    /// other surface lands on the same tab.
    #[test]
    fn the_tab_names_are_the_ones_the_pwa_stores() {
        let names: Vec<&str> = InspTab::ALL.iter().map(|tab| tab.as_str()).collect();
        assert_eq!(names, vec!["overview", "files", "browser"]);
        for tab in InspTab::ALL {
            assert_eq!(InspTab::parse(tab.as_str()), Some(tab));
        }
        assert_eq!(InspTab::parse("plan"), None, "a stale name opens Overview, not a blank pane");
    }

    /// Folders above files and case kept out of the order, because a listing that
    /// reshuffles itself between two refreshes cannot be scanned.
    #[test]
    fn a_listing_sorts_folders_first_and_ignores_case() {
        let listing = decode_listing(json!({
            "ok": true,
            "root": "D:/work/site",
            "entries": [
                { "name": "zeta", "path": "zeta", "is_dir": false, "size": 10 },
                { "name": "Alpha", "path": "Alpha", "is_dir": true, "size": 0 },
                { "name": "beta.md", "path": "beta.md", "is_dir": false, "size": 2048 },
            ],
        }))
        .unwrap();
        let names: Vec<&str> = listing.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta.md", "zeta"]);
        assert_eq!(listing.root, "D:/work/site");
        assert_eq!(super::human_size(listing.rows[1].size), "2.0 KB");
    }

    /// A refusal carries the server's sentence, and an empty folder is not a
    /// refusal — the pane words them differently, so the two must not collapse.
    #[test]
    fn an_empty_folder_and_a_refusal_stay_apart() {
        let empty = decode_listing(json!({ "ok": true, "root": "D:/work", "entries": [] })).unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(
            decode_listing(json!({ "ok": false, "error": "path escapes the workspace" })).unwrap_err(),
            "path escapes the workspace"
        );
    }

    /// A folder named `.` asks the jail for its own root; repeating it as a
    /// workspace would re-send a path the server has already resolved.
    #[test]
    fn the_default_folder_asks_for_the_jails_own_root() {
        assert_eq!(workspace_param(&FmState::default().path), "");
        assert_eq!(workspace_param(""), "");
        assert_eq!(workspace_param("D:/work/site/"), "D:/work/site/");
    }

    /// The route echoes the relative path it was handed, so the absolute name the
    /// pane shows is built here — the same join the PWA's `joinPath` makes.
    #[test]
    fn a_row_path_joins_the_root_the_jail_reported() {
        assert_eq!(jail_path("D:/work/site", "src/app.rs"), "D:/work/site/src/app.rs");
        assert_eq!(jail_path("D:\\work\\site", "src/app.rs"), "D:\\work\\site\\src\\app.rs");
        assert_eq!(
            jail_path("D:\\Fennec-Suite", "docs/desktop-ab.html"),
            "D:\\Fennec-Suite\\docs\\desktop-ab.html",
            "a Windows path shows one separator, the way the PWA renders it"
        );
        assert_eq!(jail_path("", "src/app.rs"), "src/app.rs", "no root, no guess");
        assert_eq!(jail_path("D:/work", "D:/elsewhere/x.rs"), "D:/elsewhere/x.rs");
        assert_eq!(jail_path("D:/work", "/etc/hosts"), "/etc/hosts");
        assert_eq!(jail_path("D:/work", ""), "");
    }

    /// One line never reads "1 lines", and the size follows the count, which is
    /// the order the PWA prints them in.
    #[test]
    fn a_preview_says_lines_then_size() {
        assert_eq!(decode_file(json!({ "ok": true, "content": "a\n", "lines": 1, "bytes": 2 })).unwrap().meta, "1 line · 2 B");
        assert_eq!(decode_file(json!({ "ok": true, "content": "a\nb\n", "lines": 2, "bytes": 4 })).unwrap().meta, "2 lines · 4 B");
        assert_eq!(decode_file(json!({ "ok": false, "error": "binary file" })).unwrap_err(), "binary file");
    }

    /// The route answers a picture with base64 bytes, and the pane paints them
    /// rather than printing a lossy decode of them as a wall of U+FFFD.
    #[test]
    fn a_preview_answer_with_pixels_is_not_text() {
        let read = decode_file(json!({
            "ok": true, "kind": "image", "mime": "image/png",
            "data": "iVBORw0KGgo=", "bytes": 8,
        })).unwrap();
        assert_eq!(read.meta, "image/png · 8 B");
        assert!(read.body.is_empty());
        // The PNG signature, whole: what a text decode would have mangled.
        assert_eq!(read.image.unwrap().bytes, vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(
            decode_file(json!({ "ok": true, "kind": "image", "mime": "image/avif", "data": "AAA=" }))
                .unwrap_err(),
            "This pane has no decoder for image/avif."
        );
    }

    /// The address worth showing is the one the redirects landed on, which the
    /// route reports; the text is what the agent was handed, so a truncation has
    /// to stay visible instead of passing for the whole page.
    #[test]
    fn a_page_read_keeps_the_answer_it_landed_on() {
        let page = decode_page(json!({
            "ok": true, "url": "https://example.com/final", "status": 200,
            "title": "Example", "text": "_BODY_", "truncated": true,
        }))
        .unwrap();
        assert_eq!(page.url, "https://example.com/final");
        assert_eq!((page.status, page.title.as_str(), page.text.as_str()), (200, "Example", "_BODY_"));
        assert!(page.truncated);
        assert_eq!(decode_page(json!({ "ok": false, "error": "dns failed" })).unwrap_err(), "dns failed");
        // A pane that has never read anything has no history to walk back over.
        assert!(BrState::default().hist.is_empty());
    }
}
