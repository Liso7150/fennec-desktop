//! UI-side translation of the two real backends. Nothing here is simulated:
//! agent-loop steps and engine chat deltas are plain JSON, and this module
//! turns them into timeline state — an interpretation only a client does.

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
    /// Seconds since the run started, from the step's own `t`.
    pub started_t: f64,
    pub duration: String,
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
    pub change: String,
    pub status: String,
    pub preview: String,
}

#[derive(Clone)]
pub struct PlanEntry {
    pub content: String,
    pub done: bool,
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum TaskStatus {
    #[default]
    Running,
    Complete,
    NeedsAttention,
    Waiting,
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Complete => "Complete",
            Self::NeedsAttention => "Needs attention",
            Self::Waiting => "Waiting",
        }
    }

    /// `loop.py` session status strings.
    pub fn from_core(s: &str) -> Self {
        match s {
            "running" => Self::Running,
            "done" => Self::Complete,
            "error" => Self::NeedsAttention,
            _ => Self::Waiting,
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
}

/// Everything the center pane shows for one task. Built only from events the
/// backend actually emitted, so an offline backend leaves it visibly empty.
#[derive(Clone, Default)]
pub struct Timeline {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolEvent>,
    pub plan: Vec<PlanEntry>,
    pub summary: String,
    pub status: TaskStatus,
    pub streaming: bool,
    pub streaming_text: String,
}

impl Timeline {
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
                        }
                        card.status = t.status;
                        if !t.output.is_empty() {
                            card.output = t.output;
                        }
                        if !t.detail.is_empty() {
                            card.detail = t.detail;
                        }
                    }
                    None => self.tools.push(t),
                }
            }
            Event::Plan(entries) => self.plan = entries,
            Event::Summary(s) => {
                self.streaming = false;
                self.streaming_text.clear();
                self.summary = s;
            }
            Event::Status(st) => self.status = st,
            Event::Cancelled => {
                self.streaming = false;
                for card in &mut self.tools {
                    if card.status == ToolStatus::Running {
                        card.status = ToolStatus::Warning;
                        card.duration = "cancelled".into();
                    }
                }
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
                name: t.title.split(' ').nth(2).unwrap_or("?").to_string(),
                change: if t.title.starts_with("Write") { "Added" } else { "Modified" }.to_string(),
                status: t.status.label().to_string(),
                preview: t.output.clone(),
            });
        }
        out
    }

    pub fn has_activity(&self) -> bool {
        !self.messages.is_empty() || !self.tools.is_empty() || !self.summary.is_empty()
    }
}

/// The loop's tool names → the card's badge and human title. The names are
/// `loop.py`'s registry; an unknown tool shows its own name rather than a lie.
pub fn describe(name: &str, args: &Value) -> (String, String) {
    let focus = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
                return s.to_string();
            }
        }
        String::new()
    };
    match name {
        "list_dir" => ("LS".into(), format!("Inspect repository {}", focus(&["path", "dir"]))),
        "read_file" => ("READ".into(), format!("Read file {}", focus(&["path", "file"]))),
        "write_file" => ("EDIT".into(), format!("Write file {}", focus(&["path"]))),
        "edit_file" => ("EDIT".into(), format!("Edit file {}", focus(&["path"]))),
        "grep" => ("FIND".into(), format!("Search code {}", focus(&["pattern", "query"]))),
        "semantic_search" => ("FIND".into(), format!("Semantic search {}", focus(&["query"]))),
        "run_python" => ("TEST".into(), format!("Run python {}", focus(&["code"]).chars().take(48).collect::<String>())),
        "run_shell" => ("CMD".into(), format!("Run command {}", focus(&["cmd", "command"]))),
        "git_status" => ("GIT".into(), "Repository status".into()),
        "git_diff" => ("DIFF".into(), "Review diff".into()),
        "web_task" => ("WEB".into(), "Web research task".into()),
        "coworker_task" => ("PEER".into(), "Delegate to OpenBot coworker".into()),
        "summarise" => ("SUM".into(), "Summarise the run".into()),
        other => (other.to_uppercase(), other.into()),
    }
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
/// different keys, so the first non-empty one wins.
fn result_preview(result: &Value) -> String {
    for key in ["stdout", "content", "text", "diff", "status", "error", "stderr"] {
        if let Some(s) = result.get(key).and_then(|v| v.as_str()) {
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
            if !content.is_empty() {
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
                started_t: t,
                duration: String::new(),
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
                output: result_preview(&step["result"]),
                started_t: t,
                duration: String::new(),
            })]
        }
        "final" => vec![
            Event::Summary(step["content"].as_str().unwrap_or("Task finished.").to_string()),
            Event::Status(TaskStatus::Complete),
        ],
        "cancelled" => vec![Event::Cancelled],
        "error" => vec![
            Event::Message(ChatMessage { role: Role::Progress, text: format!("Agent loop error: {}", step["content"].as_str().unwrap_or("")), time: stamp() }),
            Event::Status(TaskStatus::NeedsAttention),
        ],
        _ => Vec::new(),
    }
}

fn args_line(args: &Value) -> String {
    let compact = serde_json::to_string(args).unwrap_or_default();
    if compact.chars().count() > 240 { compact.chars().take(240).collect() } else { compact }
}

/// The plan the loop shows as it works: derived from the phases its own tool
/// cards prove it used — inspect, implement, verify, then summarise.
pub fn plan_from_tools(tools: &[ToolEvent]) -> Vec<PlanEntry> {
    let seen = |cats: &[&str]| tools.iter().any(|t| cats.contains(&t.category.as_str()));
    let settled = |cats: &[&str]| !tools.iter().any(|t| cats.contains(&t.category.as_str()) && t.status == ToolStatus::Running);
    vec![
        PlanEntry { content: "Inspect the project".into(), done: seen(&["LS", "READ", "FIND", "GIT", "DIFF"]) && settled(&["LS", "READ", "FIND", "GIT", "DIFF"]) },
        PlanEntry { content: "Implement the change".into(), done: seen(&["EDIT"]) && settled(&["EDIT"]) },
        PlanEntry { content: "Verify it runs".into(), done: seen(&["TEST", "CMD"]) && settled(&["TEST", "CMD"]) },
        PlanEntry { content: "Summarise the result".into(), done: seen(&["SUM"]) },
    ]
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
    }

    #[test]
    fn unknown_step_types_produce_nothing() {
        assert!(map_step(&json!({"type":"nonsense"})).is_empty());
        assert!(map_step(&json!({})).is_empty());
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
        tl.apply(Event::Tool(ToolEvent { id: "c1".into(), category: "CMD".into(), title: "t".into(), status: ToolStatus::Running, detail: String::new(), output: String::new(), started_t: 0.0, duration: String::new() }));
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
        tl.apply(Event::Tool(ToolEvent { id: "a".into(), category: "EDIT".into(), title: "Write file src/x.py".into(), status: ToolStatus::Complete, detail: String::new(), output: "+1".into(), started_t: 0.0, duration: "0.1s".into() }));
        tl.apply(Event::Tool(ToolEvent { id: "b".into(), category: "READ".into(), title: "Read file src/y.py".into(), status: ToolStatus::Complete, detail: String::new(), output: String::new(), started_t: 0.0, duration: "0.1s".into() }));
        let arts = tl.artifacts();
        assert_eq!(arts.len(), 1);
        assert_eq!(arts[0].name, "src/x.py");
        assert_eq!(arts[0].change, "Added");
    }

    #[test]
    fn plan_tracks_the_phases_used() {
        let tools = vec![ToolEvent { id: "a".into(), category: "LS".into(), title: "t".into(), status: ToolStatus::Complete, detail: String::new(), output: String::new(), started_t: 0.0, duration: "0.1s".into() }];
        let plan = plan_from_tools(&tools);
        assert!(plan[0].done);
        assert!(!plan[1].done);
    }
}
