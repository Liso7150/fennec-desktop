//! UI-side translation of the two real backends. Nothing here is simulated:
//! agent-loop steps and engine chat deltas are plain JSON, and this module
//! turns them into timeline state — an interpretation only a client does.

use fennec_ui::attention::{args_line, clip, looks_like_call, path_of, risk_word as risk_of};
use serde_json::Value;

/// One tool-activity card, keyed by the loop's `call_id` so the `tool_start`
/// that opens it and the `tool` result that closes it are the same card.
#[derive(Clone)]
pub struct ToolEvent {
    pub id: String,
    pub category: String,
    pub title: String,
    pub status: ToolStatus,
    pub detail: String,
    pub output: String,
    /// The file the tool named in its own arguments, when it takes one. Read off
    /// the call rather than the title, so a path with a space stays one path.
    pub path: Option<String>,
    /// `tools.py:_run_process` reports the process exit status; the rest of the
    /// registry returns a value without one, which is not the same as exit 0.
    pub exit: Option<i64>,
    /// Seconds since the run started, from the step's own `t`.
    pub started_t: f64,
    pub duration: String,
    /// How many turns the timeline had already shown when this call started —
    /// the user message and the answers, not the loop's own notes. It is what
    /// puts the call in the stretch between two answers, and so in that
    /// stretch's block.
    pub after: usize,
    /// What the call has said it is doing, once it is still blocked long enough
    /// to say something. The newest report wins; a call that never reported one
    /// shows nothing beyond its Running state.
    pub progress: Option<ProgressLine>,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum ToolStatus {
    #[default]
    Running,
    Complete,
    Warning,
    Failed,
}

impl ToolStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Complete => "Complete",
            Self::Warning => "Warning",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum Role {
    #[default]
    User,
    Assistant,
    /// The loop's own reasoning and error notes — not a reply to the user.
    Progress,
}

#[derive(Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
    pub time: String,
}

#[derive(Clone)]
pub struct Artifact {
    pub name: String,
    /// The repo-relative path the write/edit call carried, when it did. The diff
    /// view needs it; `name` is only what the rail labels the row with.
    pub path: Option<String>,
    pub change: String,
    pub status: String,
    pub preview: String,
}

#[derive(Clone)]
pub struct PlanEntry {
    pub content: String,
    pub done: bool,
}

/// The risks `tools.GATES` reports, plus the `unknown` it uses for a call the
/// table does not describe. A string outside this list — a renamed tier, a
/// hostile step — reads as unknown rather than as a colour nobody chose.
pub const APPROVAL_RISKS: [&str; 4] = ["low", "medium", "high", "unknown"];

/// Where one approval question got to.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum ApprovalState {
    /// The loop is still waiting; the buttons are live.
    #[default]
    Pending,
    /// The service accepted the answer, so the run continued with it.
    Answered,
    /// The answer did not land, or the question expired. Either way the loop
    /// treated it as a rejection, so the buttons have to go.
    Expired,
}

/// One gated call the loop stopped on, as its `approval_required` step stated
/// it. `risk` and `reason` come from the backend's own gate table — the shell
/// has no idea of its own about what a tool can do, and does not pretend to.
#[derive(Clone, Debug)]
pub struct Approval {
    pub id: String,
    pub name: String,
    pub category: String,
    pub title: String,
    /// The call's arguments, compact, for the card to show what would run.
    pub detail: String,
    /// The file the call names, when it takes one — the card has to point at
    /// what it would touch, not only at a sentence about it.
    pub path: Option<String>,
    pub risk: String,
    pub reason: String,
    pub state: ApprovalState,
    /// The option the user chose, once one landed. Empty otherwise.
    pub decision: String,
    /// Why the card stopped being live, when it stopped because of an error.
    pub note: String,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum TaskStatus {
    #[default]
    Running,
    Complete,
    NeedsAttention,
    Waiting,
    /// Blocked on a question this shell has to answer, not on the model.
    AwaitingApproval,
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Complete => "Complete",
            Self::NeedsAttention => "Needs attention",
            Self::Waiting => "Waiting",
            Self::AwaitingApproval => "Waiting for approval",
        }
    }

    /// The word this status is stored and compared as. `label()` is what the
    /// rail shows; this is what the history file and the attention filter use,
    /// so the two shells can share one rule about what counts as attention.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Complete => "Complete",
            Self::NeedsAttention => "NeedsAttention",
            Self::Waiting => "Waiting",
            Self::AwaitingApproval => "AwaitingApproval",
        }
    }

    /// `loop.py` session status strings. A run stopped on an approval still
    /// reports `running` — that state only ever comes from the `pending` list
    /// on the poll, never from this string.
    pub fn from_core(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "done" => Self::Complete,
            "error" => Self::NeedsAttention,
            _ => Self::Waiting,
        }
    }
}

/// One in-flight call's live line, as the loop last reported it. `text` is the
/// backend's own sentence and reaches the user unchanged; the meter is kept
/// beside it so a surface can dim the count apart from the words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressLine {
    pub text: String,
    pub step: Option<u64>,
    pub of: Option<u64>,
}

impl ProgressLine {
    /// The whole line as the user reads it: the sentence, then the dim `3/12`
    /// when the loop counted both halves.
    pub fn line(&self) -> String {
        match (self.step, self.of) {
            (Some(s), Some(o)) => format!("{} {}/{}", self.text, s, o),
            _ => self.text.clone(),
        }
    }
}

/// A row in the left rail: a real agent session, or a local draft chat task.
#[derive(Clone)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub status: TaskStatus,
    pub updated: String,
    pub goal: String,
    pub draft: bool,
    pub workspace_id: Option<String>,
}

/// What the timeline can receive: backend events, or a chat token stream.
#[derive(Clone)]
pub enum Event {
    Message(ChatMessage),
    Partial(String),
    Tool(ToolEvent),
    Plan(Vec<PlanEntry>),
    Summary(String),
    Status(TaskStatus),
    Cancelled,
    Approval(Approval),
    /// Follow-up prompts the loop queued for a complex run. Not timeline
    /// content: the shell owns the queue the user sees.
    Queued(Vec<String>),
    /// What one in-flight call says it is doing, as the loop put it: its own
    /// sentence and, when it counted, the position within that count. Not a
    /// timeline event — the card the `tool_start` opened carries it, and the
    /// next report of the same call replaces it.
    Progress { id: String, text: String, step: Option<u64>, of: Option<u64> },
}

/// Everything the center pane shows for one task. Built only from events the
/// backend actually emitted, so an offline backend leaves it visibly empty.
#[derive(Clone, Default)]
pub struct Timeline {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolEvent>,
    pub plan: Vec<PlanEntry>,
    pub approvals: Vec<Approval>,
    pub summary: String,
    pub status: TaskStatus,
    pub streaming: bool,
    pub streaming_text: String,
}

impl Timeline {
    /// The turns the user counts as separating one stretch of actions from the
    /// next: their own message and the answers. The loop's reasoning notes are
    /// not answers, so they do not close a block.
    pub fn boundaries(&self) -> usize {
        self.messages.iter().filter(|m| m.role != Role::Progress).count()
    }

    pub fn apply(&mut self, ev: Event) {
        match ev {
            Event::Message(m) => {
                if m.role == Role::Assistant {
                    self.streaming = false;
                    self.streaming_text.clear();
                }
                self.messages.push(m);
            }
            Event::Partial(s) => {
                self.streaming = true;
                self.streaming_text.push_str(&s);
            }
            Event::Tool(t) => {
                let finished = t.status != ToolStatus::Running;
                match self.tools.iter_mut().find(|c| c.id == t.id) {
                    Some(card) => {
                        if finished && card.status == ToolStatus::Running {
                            card.duration = format!("{:.1}s", (t.started_t - card.started_t).max(0.0));
                        }                        card.status = t.status;
                        // What the call said while it was blocked was its own
                        // report of the present tense; the result replaces it, so
                        // a settled card carries no line — and a fold that keys
                        // "still running" off one would never close.
                        if finished {
                            card.progress = None;
                        }
                        if !t.output.is_empty() {
                            card.output = t.output;
                        }
                        if !t.detail.is_empty() {
                            card.detail = t.detail;
                        }
                        if t.path.is_some() {
                            card.path = t.path;
                        }
                        if t.exit.is_some() {
                            card.exit = t.exit;
                        }
                    }
                    None => {
                        // A card keeps the position it opened at: the result step
                        // that closes it arrives later, and the answer in between
                        // has already moved the timeline on.
                        let mut card = t;
                        card.after = self.boundaries();
                        self.tools.push(card);
                    }
                }
            }
            Event::Plan(entries) => self.plan = entries,
            Event::Approval(a) => match self.approvals.iter_mut().find(|c| c.id == a.id) {
                // One question, one card: a step that repeats an id updates the
                // card it opened rather than adding a second way to answer it.
                // Only while the card is still open — a step that arrives after
                // the user answered, or after the question died, must not hand
                // the buttons back.
                Some(card) if card.state == ApprovalState::Pending => *card = a,
                Some(_) => {}
                None => self.approvals.push(a),
            },
            Event::Summary(s) => {
                self.streaming = false;
                self.streaming_text.clear();
                self.summary = s;
            }
            Event::Status(st) => self.status = st,
            // A queued follow-up is the shell's task queue, not this timeline:
            // `App::drain` moves it there before the timeline is asked to apply
            // anything, so a run's own record stays free of queue bookkeeping.
            Event::Queued(_) => {}
            // The line belongs to the card its `tool_start` opened, so a report
            // about a call this timeline never saw is dropped rather than making
            // a card of its own. A settled card keeps no line: whatever it said
            // while it ran is not what it delivered.
            Event::Progress { id, text, step, of } => {
                if let Some(card) = self.tools.iter_mut().find(|c| c.id == id) {
                    if card.status == ToolStatus::Running {
                        card.progress = Some(ProgressLine { text, step, of });
                    }
                }
            }
            Event::Cancelled => {
                self.streaming = false;
                for card in &mut self.tools {
                    if card.status == ToolStatus::Running {
                        card.status = ToolStatus::Warning;
                        card.duration = "cancelled".into();
                    }
                }
                // A cancelled run stops waiting for an answer: the loop breaks
                // out of its own wait, so those buttons are already dead.
                self.expire_approvals("the run was cancelled");
                self.status = TaskStatus::Waiting;
            }
        }
    }

    /// Artifacts are the files this run actually wrote or edited, read off the
    /// tool cards — the backend keeps no separate artifact registry.
    pub fn artifacts(&self) -> Vec<Artifact> {
        let mut out = Vec::new();
        for t in &self.tools {
            if t.category != "EDIT" {
                continue;
            }
            out.push(Artifact {
                name: t.path.clone().unwrap_or_else(|| t.title.clone()),
                path: t.path.clone(),
                change: if t.title.starts_with("Write") { "Added" } else { "Modified" }.to_string(),
                status: t.status.label().to_string(),
                preview: t.output.clone(),
            });
        }
        out
    }

    /// Record what the service said about one answer. A rejected `POST` is not
    /// swallowed: the card keeps the decision the user chose and says it did not
    /// land, because the run went on without it.
    pub fn answer_approval(&mut self, approval_id: &str, decision: &str, error: Option<String>) {
        let Some(card) = self.approvals.iter_mut().find(|c| c.id == approval_id) else {
            return;
        };
        card.decision = decision.to_string();
        match error {
            None => card.state = ApprovalState::Answered,
            Some(e) => {
                card.state = ApprovalState::Expired;
                card.note = e;
            }
        }
    }

    /// `pending` from `/api/agent/poll` — the approval ids the loop still holds.
    /// A card whose id is missing can no longer be answered: it timed out, was
    /// answered elsewhere, or the run ended. Every way the loop reaches one of
    /// those is a rejection of the call, so the buttons have to go and the card
    /// has to say the question is no longer live.
    pub fn close_approvals(&mut self, live: &[String]) {
        for card in &mut self.approvals {
            if card.state == ApprovalState::Pending && !live.contains(&card.id) {
                card.state = ApprovalState::Expired;
                card.note = "no longer pending — the loop waited, got no answer, and refused the call".to_string();
            }
        }
    }

    fn expire_approvals(&mut self, note: &str) {
        for card in &mut self.approvals {
            if card.state == ApprovalState::Pending {
                card.state = ApprovalState::Expired;
                card.note = note.to_string();
            }
        }
    }

    pub fn waiting_on_approval(&self) -> bool {
        self.approvals.iter().any(|c| c.state == ApprovalState::Pending)
    }

    pub fn has_activity(&self) -> bool {
        !self.messages.is_empty()
            || !self.tools.is_empty()
            || !self.approvals.is_empty()
            || !self.summary.is_empty()
    }

    /// What a call the run is still inside last said about itself, as one line:
    /// the loop's own sentence with its meter. `None` when the card settled or
    /// never reported — a surface draws nothing then, which is what it drew
    /// before a long call learned to talk.
    pub fn progress_of(&self, id: &str) -> Option<String> {
        self.tools.iter().find(|c| c.id == id)
            .filter(|c| c.status == ToolStatus::Running)
            .and_then(|c| c.progress.as_ref())
            .map(ProgressLine::line)
    }
}

/// The loop's tool names → the card's badge and human title. The table lives in
/// `fennec-ui` so both shells name the same call the same way; an unknown tool
/// shows its own name rather than a lie.
pub fn describe(name: &str, args: &Value) -> (String, String) {
    fennec_ui::attention::describe_call(name, args)
}

fn stamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() % 86400;
    format!("{:02}:{:02}", secs / 3600, (secs / 60) % 60)
}

pub fn now_stamp() -> String {
    stamp()
}

/// The most useful string a tool result carries; `loop.py` tools return
/// different keys, so the first non-empty one wins — in the order that depends
/// on the outcome, because a failed command's story is on stderr, not stdout.
fn result_preview(result: &Value, ok: bool) -> String {
    let keys: &[&str] = if ok {
        &["stdout", "content", "text", "diff", "status", "error", "stderr"]
    } else {
        &["error", "stderr", "stdout", "content", "text", "diff", "status"]
    };
    for key in keys {
        if let Some(s) = result.get(*key).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                let t = s.trim();
                return if t.chars().count() > 600 { format!("{} …", t.chars().take(600).collect::<String>()) } else { t.to_string() };
            }
        }
    }
    result.to_string().chars().take(300).collect()
}

/// Translate one `/api/agent/poll` step into timeline events. Unknown step
/// types return nothing: the timeline never invents an activity.
pub fn map_step(step: &Value) -> Vec<Event> {
    let kind = step["type"].as_str().unwrap_or("");
    let t = step["t"].as_f64().unwrap_or(0.0);
    match kind {
        "assistant" => {
            let mut out = Vec::new();
            let reasoning = step["reasoning"].as_str().unwrap_or("").trim();
            if !reasoning.is_empty() {
                out.push(Event::Message(ChatMessage { role: Role::Progress, text: reasoning.to_string(), time: stamp() }));
            }
            let content = step["content"].as_str().unwrap_or("").trim();
            // The loop publishes each tool spelling the model wrote as text as
            // its own assistant step, and a turn that does not land a call
            // spells the same blob again. As an answer that is the same JSON
            // object over and over; the call's own row says what ran, so the
            // blob never becomes a message here.
            if !content.is_empty() && !looks_like_call(content,
                    step["tool_calls"].as_array().is_some_and(|c| !c.is_empty())) {
                out.push(Event::Message(ChatMessage { role: Role::Assistant, text: content.to_string(), time: stamp() }));
            }
            out
        }
        "partial" => vec![Event::Partial(step["content"].as_str().unwrap_or("").to_string())],
        "tool_start" => {
            let name = step["name"].as_str().unwrap_or("tool");
            let args = step["args"].clone();
            let (category, title) = describe(name, &args);
            vec![Event::Tool(ToolEvent {
                id: step["call_id"].as_str().unwrap_or(name).to_string(),
                category,
                title,
                status: ToolStatus::Running,
                detail: args_line(&args),
                output: String::new(),
                path: path_of(&args),
                exit: None,
                started_t: t,
                duration: String::new(),
                after: 0,
            progress: None,
})]
        }
        "tool" => {
            let name = step["name"].as_str().unwrap_or("tool");
            let args = step["args"].clone();
            let (category, title) = describe(name, &args);
            let ok = step["ok"].as_bool().unwrap_or(false);
            vec![Event::Tool(ToolEvent {
                id: step["call_id"].as_str().unwrap_or(name).to_string(),
                category,
                title,
                status: if ok { ToolStatus::Complete } else { ToolStatus::Failed },
                detail: args_line(&args),
                output: result_preview(&step["result"], ok),
                path: path_of(&args),
                exit: step["result"]["exit"].as_i64(),
                started_t: t,
                duration: String::new(),
                after: 0,
            progress: None,
})]
        }
        "approval_required" => {
            let id = step["approval_id"].as_str().unwrap_or("");
            let name = step["name"].as_str().unwrap_or("").trim();
            // The same rule the PWA applies: a question with no id cannot be
            // answered and a question with no name cannot be acted on, so
            // neither one becomes a card.
            if id.is_empty() || name.is_empty() {
                return Vec::new();
            }
            let args = step["args"].clone();
            let (category, title) = describe(name, &args);
            vec![Event::Approval(Approval {
                id: clip(id, 64),
                name: clip(name, 120),
                category,
                title,
                detail: args_line(&args),
                path: path_of(&args),
                risk: risk_of(step["risk"].as_str()),
                reason: clip(step["reason"].as_str().unwrap_or(""), 400),
                state: ApprovalState::Pending,
                decision: String::new(),
                note: String::new(),
            })]
        }
        "plan" => vec![Event::Plan(plan_entries(&step["entries"]))],
        // What a call still blocked says about itself. The words are the loop's
        // and travel unchanged; the two counters come along so the surface can
        // put a meter beside them. A report with no call to belong to, or with
        // nothing to say, is not an event at all — see `progress_without_a_call_
        // or_a_sentence_is_dropped`.
        "progress" => {
            let id = step["call_id"].as_str().unwrap_or("");
            let text = match step["text"].as_str() {
                Some(t) => t.to_string(),
                None => return Vec::new(),
            };
            if id.is_empty() || text.is_empty() {
                return Vec::new();
            }
            vec![Event::Progress {
                id: id.to_string(),
                text,
                step: positive_field(step, "step"),
                of: positive_field(step, "of"),
            }]
        }
        "final" => vec![
            Event::Summary(step["content"].as_str().unwrap_or("Task finished.").to_string()),
            Event::Status(TaskStatus::Complete),
        ],
        "cancelled" => vec![Event::Cancelled],
        "queue" => vec![Event::Queued(step["items"].as_array().map(|list| {
            list.iter().filter_map(Value::as_str).map(str::to_string).collect()
        }).unwrap_or_default())],
        "error" => vec![
            Event::Message(ChatMessage { role: Role::Progress, text: format!("Agent loop error: {}", step["content"].as_str().unwrap_or("")), time: stamp() }),
            Event::Status(TaskStatus::NeedsAttention),
        ],
        _ => Vec::new(),
    }
}

/// A position the loop counted: a positive integer under this key, and nothing
/// else. A float, a numeral spelled as text or a zero means the step did not
/// count, so no meter is drawn for it.
fn positive_field(step: &Value, key: &str) -> Option<u64> {
    step[key].as_u64().filter(|n| *n > 0)
}

/// The loop's own plan list, read straight off its `plan` step. Nothing here
/// re-derives phases from tool names: `loop.py` owns that rule, and a polling
/// shell renders exactly what it sent — including an empty list at turn start.
fn plan_entries(rows: &Value) -> Vec<PlanEntry> {
    let Some(list) = rows.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|r| {
            Some(PlanEntry {
                content: r["content"].as_str()?.to_string(),
                done: r["status"].as_str() == Some("completed"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_card_closes_on_its_result() {
        let mut tl = Timeline::default();
        for e in map_step(&json!({"type":"tool_start","t":1.0,"name":"read_file","call_id":"c1","args":{"path":"a.py"}})) {
            tl.apply(e);
        }
        for e in map_step(&json!({"type":"tool","t":2.5,"name":"read_file","call_id":"c1","ok":true,"args":{"path":"a.py"},"result":{"content":"x=1"}})) {
            tl.apply(e);
        }
        assert_eq!(tl.tools.len(), 1);
        assert_eq!(tl.tools[0].status, ToolStatus::Complete);
        assert_eq!(tl.tools[0].duration, "1.5s");
        assert_eq!(tl.tools[0].output, "x=1");
        assert_eq!(tl.tools[0].path.as_deref(), Some("a.py"));
        // Only `_run_process` reports an exit status; a read has none to show.
        assert_eq!(tl.tools[0].exit, None);
    }

    /// `tools.py:_run_process` returns {ok, exit, stdout, stderr}, so a failing
    /// command has to surface both its status and what it wrote to stderr.
    #[test]
    fn a_failed_command_shows_its_exit_and_its_stderr() {
        let mut tl = Timeline::default();
        let start = json!({"type":"tool_start","t":1.0,"name":"run_shell","call_id":"c9","args":{"cmd":"pytest -q"}});
        let end = json!({"type":"tool","t":4.0,"name":"run_shell","call_id":"c9","ok":false,
                         "args":{"cmd":"pytest -q"},
                         "result":{"exit":1,"stdout":"....\n","stderr":"AssertionError: 1 != 0"}});
        for e in map_step(&start) {
            tl.apply(e);
        }
        for e in map_step(&end) {
            tl.apply(e);
        }
        assert_eq!(tl.tools[0].status, ToolStatus::Failed);
        assert_eq!(tl.tools[0].exit, Some(1));
        assert_eq!(tl.tools[0].output, "AssertionError: 1 != 0");
        assert_eq!(tl.tools[0].duration, "3.0s");
    }

    #[test]
    fn a_successful_command_shows_its_stdout() {
        let ev = map_step(&json!({"type":"tool","t":1.0,"name":"run_shell","call_id":"c1","ok":true,
                                  "args":{"cmd":"true"},"result":{"exit":0,"stdout":"ok\n","stderr":""}}));
        let mut tl = Timeline::default();
        for e in ev {
            tl.apply(e);
        }
        assert_eq!(tl.tools[0].output, "ok");
        assert_eq!(tl.tools[0].exit, Some(0));
    }

    #[test]
    fn unknown_step_types_produce_nothing() {
        assert!(map_step(&json!({"type":"nonsense"})).is_empty());
        assert!(map_step(&json!({})).is_empty());
    }

    /// The follow-ups a complex run leaves behind reach this shell as prompts,
    /// in order, which is what its task queue holds.
    #[test]
    fn a_queue_step_carries_its_items() {
        let step = json!({"type": "queue", "t": 4.0,
                          "items": ["Add the drain test", "Refresh the docs"]});
        match map_step(&step).as_slice() {
            [Event::Queued(items)] => assert_eq!(items,
                &["Add the drain test".to_string(), "Refresh the docs".to_string()]),
            other => panic!("one queue event expected, got {} events", other.len()),
        }
        match map_step(&json!({"type": "queue", "items": []})).as_slice() {
            [Event::Queued(items)] => assert!(items.is_empty()),
            other => panic!("one queue event expected, got {} events", other.len()),
        }
    }

    /// `loop.py:STEP_TYPES`, copied here as the contract this shell has to
    /// cover. `loop.py` owns the list; a type added there without an arm here
    /// would make this shell go silent on part of a live run.
    const LOOP_STEP_TYPES: [&str; 11] = ["plan", "assistant", "partial", "tool_start", "tool",
                                         "queue", "final", "error", "cancelled",
                                         "approval_required", "progress"];

    #[test]
    fn every_step_type_the_loop_declares_has_an_arm_here() {
        for kind in LOOP_STEP_TYPES {
            let rest = match kind {
                "plan" => json!({"entries": []}),
                "tool_start" => json!({"name": "read_file", "args": {}, "call_id": "c1"}),
                "tool" => json!({"name": "read_file", "args": {}, "call_id": "c1",
                                 "ok": true, "result": {}}),
                "progress" => json!({"name": "web_task", "call_id": "c1",
                                     "text": "Deciding step 3 of 12", "step": 3, "of": 12}),
                "approval_required" => json!({"approval_id": "a1", "name": "write_file",
                                              "args": {"path": "n.md"}, "risk": "medium",
                                              "reason": "replaces a file in the workspace"}),
                _ => json!({"content": "text"}),
            };
            let mut step = rest;
            step["type"] = json!(kind);
            assert!(!map_step(&step).is_empty(), "{kind} has no arm in map_step");
        }
    }

    /// The contract a long-running call reports against: its sentence, verbatim,
    /// with the counters beside it, and nothing else about it.
    fn progress(step: Value) -> ProgressLine {
        match map_step(&step).as_slice() {
            [Event::Progress { id, text, step, of }] => {
                assert_eq!(id, "call_7");
                ProgressLine { text: text.clone(), step: *step, of: *of }
            }
            other => panic!("one progress event expected, got {} events", other.len()),
        }
    }

    #[test]
    fn a_progress_step_carries_the_loops_sentence_and_its_counts() {
        let line = progress(json!({"type": "progress", "name": "web_task", "call_id": "call_7",
                                   "text": "Deciding step 3 of 12", "step": 3, "of": 12,
                                   "i": 7, "t": 41.2}));
        assert_eq!(line.text, "Deciding step 3 of 12");
        assert_eq!((line.step, line.of), (Some(3), Some(12)));
        assert_eq!(line.line(), "Deciding step 3 of 12 3/12");
        // A sentence is not cut or re-cased on the way through, however long it is.
        let long = "x".repeat(900);
        assert_eq!(progress(json!({"type": "progress", "call_id": "call_7", "text": &long})).text, long);
        // Either counter may be absent, and then there is no meter to draw.
        let alone = progress(json!({"type": "progress", "call_id": "call_7", "text": "Typing", "step": 4}));
        assert_eq!((alone.step, alone.of, alone.line().as_str()), (Some(4), None, "Typing"));
        // A counter that is not a positive integer said nothing about a position.
        for junk in [json!(0), json!(-2), json!(1.5), json!("3"), json!(null), json!(true)] {
            let l = progress(json!({"type": "progress", "call_id": "call_7", "text": "Waiting",
                                    "step": junk.clone(), "of": 12}));
            assert_eq!(l.step, None, "for {junk}");
            assert_eq!(l.line(), "Waiting");
        }
    }

    /// A `progress` step belongs to a card. One that names no call, or brings no
    /// sentence, is ignored rather than turned into an error row.
    #[test]
    fn a_progress_step_with_nothing_to_say_or_nobody_to_say_it_about_is_dropped() {
        for orphan in [
            json!({"type": "progress", "text": "no call id"}),
            json!({"type": "progress", "call_id": "", "text": "empty call id"}),
            json!({"type": "progress", "call_id": "c1"}),
            json!({"type": "progress", "call_id": "c1", "text": null}),
            json!({"type": "progress", "call_id": "c1", "text": 7}),
            json!({"type": "progress", "call_id": "c1", "text": ["a list"]}),
            json!({"type": "progress", "call_id": "c1", "text": ""}),
        ] {
            assert!(map_step(&orphan).is_empty(), "{orphan} should be ignored");
        }
    }

    /// The line lives on the card its `tool_start` opened: one visible report per
    /// call, replaced by the next, and gone from the timeline's counted rows.
    #[test]
    fn a_newer_report_replaces_the_line_on_its_own_card() {
        let mut tl = Timeline::default();
        for e in map_step(&json!({"type": "tool_start", "t": 1.0, "name": "web_task",
                                  "call_id": "call_7", "args": {"goal": "book it"}})) {
            tl.apply(e);
        }
        for e in map_step(&json!({"type": "progress", "name": "web_task", "call_id": "call_7",
                                  "text": "Deciding step 3 of 12", "step": 3, "of": 12})) {
            tl.apply(e);
        }
        assert_eq!(tl.tools.len(), 1, "a report does not open a second card");
        assert_eq!(tl.tools[0].progress.as_ref().unwrap().line(), "Deciding step 3 of 12 3/12");
        for e in map_step(&json!({"type": "progress", "name": "web_task", "call_id": "call_7",
                                  "text": "Typing into the search box"})) {
            tl.apply(e);
        }
        assert_eq!(tl.tools.len(), 1);
        assert_eq!(tl.tools[0].progress.as_ref().unwrap().line(), "Typing into the search box");
        // The card stays as live as the call it stands for: a report neither
        // settles nor folds it.
        assert_eq!(tl.tools[0].status, ToolStatus::Running);
        // And a report about a call this timeline never saw opens nothing.
        for e in map_step(&json!({"type": "progress", "call_id": "elsewhere", "text": "Stranded"})) {
            tl.apply(e);
        }
        assert_eq!(tl.tools.len(), 1);
        // The result closes the card and its line goes with the running state.
        for e in map_step(&json!({"type": "tool", "t": 41.0, "name": "web_task", "call_id": "call_7",
                                  "ok": true, "args": {}, "result": {"done": true}})) {
            tl.apply(e);
        }
        assert_eq!(tl.tools[0].status, ToolStatus::Complete);
        assert!(tl.tools[0].progress.is_none());
    }

    /// What `loop.request_approval` publishes for the gated call in the PWA's
    /// own fixture: the risk and the reason are the backend's words, and the
    /// file is the argument's value, so a card can name what it would touch.
    fn ask(step: Value) -> Approval {
        let events = map_step(&step);
        match events.as_slice() {
            [Event::Approval(a)] => a.clone(),
            other => panic!("one approval event expected, got {} events", other.len()),
        }
    }

    fn approval_step() -> Value {
        json!({"type": "approval_required", "t": 3.0, "approval_id": "a1b2c3d4",
               "name": "write_file", "args": {"path": "notes/todo list.md", "content": "hi"},
               "risk": "medium", "reason": "replaces a file in the workspace"})
    }

    #[test]
    fn an_approval_card_carries_what_the_backend_said_about_the_call() {
        let card = ask(approval_step());
        assert_eq!(card.id, "a1b2c3d4");
        assert_eq!(card.name, "write_file");
        assert_eq!(card.risk, "medium");
        assert_eq!(card.reason, "replaces a file in the workspace");
        assert_eq!(card.title, "Write file notes/todo list.md");
        assert_eq!(card.path.as_deref(), Some("notes/todo list.md"));
        assert_eq!(card.state, ApprovalState::Pending);
        assert!(card.decision.is_empty() && card.note.is_empty());
    }

    #[test]
    fn a_question_that_cannot_be_answered_or_acted_on_is_not_a_card() {
        let mut no_id = approval_step();
        no_id["approval_id"] = json!("");
        assert!(map_step(&no_id).is_empty());
        let mut no_name = approval_step();
        no_name["name"] = json!("");
        assert!(map_step(&no_name).is_empty());
        // A step that is not a question stays dropped, as every unknown type is.
        assert!(map_step(&json!({"type": "approval_required"})).is_empty());
    }

    #[test]
    fn a_risk_the_gate_table_never_mentioned_reads_as_unknown() {
        let mut step = approval_step();
        for spelling in ["", "HIGH", "critical", "constructor"] {
            step["risk"] = json!(spelling);
            let want = match spelling {
                "HIGH" => "high",
                "" | "critical" | "constructor" => "unknown",
                _ => unreachable!(),
            };
            assert_eq!(ask(step.clone()).risk, want, "for {spelling:?}");
        }
        // Oversized payloads are clipped, not rejected: the card stays one card.
        step["reason"] = json!("a".repeat(1200));
        assert_eq!(ask(step.clone()).reason.chars().count(), 400);
        step["approval_id"] = json!("x".repeat(200));
        assert_eq!(ask(step).id.chars().count(), 64);
    }

    #[test]
    fn one_question_is_one_card_even_when_the_run_repeats_it() {
        let mut tl = Timeline::default();
        for e in map_step(&approval_step()) {
            tl.apply(e);
        }
        for e in map_step(&approval_step()) {
            tl.apply(e);
        }
        assert_eq!(tl.approvals.len(), 1);
        assert!(tl.waiting_on_approval());
        assert!(tl.has_activity());
    }

    #[test]
    fn a_poll_that_stops_listing_a_question_retires_its_buttons() {
        let mut tl = Timeline::default();
        for e in map_step(&approval_step()) {
            tl.apply(e.clone());
        }
        // Still held: nothing changes, the buttons stay live.
        tl.close_approvals(&["a1b2c3d4".to_string()]);
        assert_eq!(tl.approvals[0].state, ApprovalState::Pending);
        // Gone from `pending`: the loop already refused the call, so an answer
        // sent now would be a button the service is going to reject.
        tl.close_approvals(&[]);
        assert_eq!(tl.approvals[0].state, ApprovalState::Expired);
        assert!(tl.approvals[0].note.contains("refused"));
        assert!(!tl.waiting_on_approval());
    }

    #[test]
    fn an_answer_that_did_not_land_says_so_on_the_card() {
        let mut tl = Timeline::default();
        for e in map_step(&approval_step()) {
            tl.apply(e);
        }
        tl.answer_approval("a1b2c3d4", "allow_once", Some("http 404: no such pending approval".into()));
        let card = &tl.approvals[0];
        assert_eq!(card.decision, "allow_once");
        assert_eq!(card.state, ApprovalState::Expired);
        assert_eq!(card.note, "http 404: no such pending approval");
        // A card that never existed is not an answer either; nothing is invented.
        tl.answer_approval("nope", "allow_once", None);
        assert_eq!(tl.approvals.len(), 1);
    }

    #[test]
    fn a_cancelled_run_stops_waiting_on_its_questions() {
        let mut tl = Timeline::default();
        for e in map_step(&approval_step()) {
            tl.apply(e);
        }
        tl.apply(Event::Cancelled);
        assert_eq!(tl.approvals[0].state, ApprovalState::Expired);
        assert_eq!(tl.approvals[0].note, "the run was cancelled");
        assert_eq!(tl.status, TaskStatus::Waiting);
    }

    #[test]
    fn failed_tool_keeps_its_error_text() {
        let ev = map_step(&json!({"type":"tool","t":3.0,"name":"run_shell","call_id":"c9","ok":false,"args":{"cmd":"false"},"result":{"error":"exit 1"}}));
        let mut tl = Timeline::default();
        for e in ev {
            tl.apply(e);
        }
        assert_eq!(tl.tools[0].status, ToolStatus::Failed);
        assert_eq!(tl.tools[0].output, "exit 1");
    }

    #[test]
    fn cancelled_run_warns_on_open_cards() {
        let mut tl = Timeline::default();
        tl.apply(Event::Tool(ToolEvent { id: "c1".into(), category: "CMD".into(), title: "t".into(), status: ToolStatus::Running, detail: String::new(), output: String::new(), path: None, exit: None, started_t: 0.0, after: 0, duration: String::new(), progress: None, }));
        tl.apply(Event::Cancelled);
        assert_eq!(tl.tools[0].status, ToolStatus::Warning);
        assert_eq!(tl.status, TaskStatus::Waiting);
    }

    #[test]
    fn core_statuses_map_without_invention() {
        assert_eq!(TaskStatus::from_core("running"), TaskStatus::Running);
        assert_eq!(TaskStatus::from_core("done"), TaskStatus::Complete);
        assert_eq!(TaskStatus::from_core("error"), TaskStatus::NeedsAttention);
        assert_eq!(TaskStatus::from_core("cancelled"), TaskStatus::Waiting);
    }

    #[test]
    fn artifacts_come_from_edit_cards_only() {
        let mut tl = Timeline::default();
        tl.apply(Event::Tool(ToolEvent { id: "a".into(), category: "EDIT".into(), title: "Write file src/x.py".into(), status: ToolStatus::Complete, detail: String::new(), output: "+1".into(), path: Some("src/x.py".into()), exit: None, started_t: 0.0, after: 0, duration: "0.1s".into(), progress: None, }));
        tl.apply(Event::Tool(ToolEvent { id: "b".into(), category: "READ".into(), title: "Read file src/y.py".into(), status: ToolStatus::Complete, detail: String::new(), output: String::new(), path: Some("src/y.py".into()), exit: None, started_t: 0.0, after: 0, duration: "0.1s".into(), progress: None, }));
        tl.apply(Event::Tool(ToolEvent { id: "c".into(), category: "EDIT".into(), title: "Edit file notes/todo list.md".into(), status: ToolStatus::Complete, detail: String::new(), output: "~1".into(), path: Some("notes/todo list.md".into()), exit: None, started_t: 0.0, after: 0, duration: "0.1s".into(), progress: None, }));
        let arts = tl.artifacts();
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].name, "src/x.py");
        assert_eq!(arts[0].change, "Added");
        // The name is the argument's value, not a word of the title: a path with
        // a space in it stays one path.
        assert_eq!(arts[1].name, "notes/todo list.md");
        assert_eq!(arts[1].change, "Modified");
    }

    #[test]
    fn the_plan_comes_from_the_loop_not_from_tool_names() {
        let step = json!({"type": "plan", "t": 2.0, "entries": [
            {"content": "Inspect the workspace", "status": "completed", "priority": "medium"},
            {"content": "Implement the change", "status": "in_progress", "priority": "high"},
        ]});
        let mut tl = Timeline::default();
        for e in map_step(&step) {
            tl.apply(e);
        }
        assert_eq!(tl.plan.len(), 2);
        assert!(tl.plan[0].done);
        assert!(!tl.plan[1].done);
        assert_eq!(tl.plan[1].content, "Implement the change");
        // An empty plan is what a new turn reports, not a missing one.
        let mut fresh = Timeline::default();
        fresh.plan = vec![PlanEntry { content: "stale".into(), done: true }];
        for e in map_step(&json!({"type": "plan", "entries": []})) {
            fresh.apply(e);
        }
        assert!(fresh.plan.is_empty());
    }

    /// Action blocks are cut at message boundaries, so a card has to record how
    /// many answers already sat above it when it opened — and a reasoning note
    /// is not an answer, or every thought would split a block in two.
    #[test]
    fn a_tool_card_records_the_answer_stretch_it_opened_in() {
        let mut tl = Timeline::default();
        tl.messages.push(ChatMessage { role: Role::User, text: "do it".into(), time: String::new() });
        for step in [
            json!({"type":"tool_start","t":1.0,"name":"write_file","call_id":"c1","args":{"path":"a.py"}}),
            json!({"type":"assistant","t":2.0,"reasoning":"let me think","content":"done"}),
            json!({"type":"tool_start","t":3.0,"name":"run_shell","call_id":"c2","args":{"cmd":"pytest"}}),
        ] {
            for e in map_step(&step) {
                tl.apply(e);
            }
        }
        // The user prompt is boundary 1; the reasoning note is skipped, so the
        // answer is boundary 2 and the command opened after it.
        assert_eq!(tl.boundaries(), 2);
        assert_eq!(
            tl.tools.iter().map(|c| c.after).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }
}
