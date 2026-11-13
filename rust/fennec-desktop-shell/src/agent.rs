//! Data models + mock agent event stream for the native desktop app.
//! Mirrors the fennec-desktop 2.0.0 spec (Colibri Coding Agent). Mock mode runs
//! without API keys / shell / model; the same commands can later bind to the
//! real local agent backend.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TaskStatus { Running, Complete, NeedsAttention, Reviewed }
impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self { Self::Running => "Running", Self::Complete => "Complete",
                     Self::NeedsAttention => "Needs attention", Self::Reviewed => "Reviewed" }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task { pub id: u32, pub title: String, pub status: TaskStatus, pub updated: String, pub goal: String }

#[derive(Clone, Copy, PartialEq)]
pub enum Role { User, Assistant }

#[derive(Clone)]
pub struct ChatMessage { pub role: Role, pub text: String, pub time: String }

#[derive(Clone, Copy, PartialEq)]
pub enum ToolStatus { Running, Complete, Warning, Failed }
impl ToolStatus {
    pub fn label(&self) -> &'static str {
        match self { Self::Running => "Running", Self::Complete => "Complete",
                     Self::Warning => "Warning", Self::Failed => "Failed" }
    }
}

#[derive(Clone)]
pub struct ToolEvent {
    pub category: String, pub title: String, pub status: ToolStatus,
    pub detail: String, pub output: String, pub duration: String, pub expanded: bool,
}

#[derive(Clone)]
pub struct Artifact { pub name: String, pub change: String, pub diff: String, pub content: String }

#[derive(Clone)]
pub struct Subagent { pub name: String, pub status: String, pub desc: String, pub progress: f32 }

#[derive(Clone)]
pub struct Environment {
    pub runtime: String, pub env: String, pub repo: String, pub branch: String,
    pub changed: String, pub tests: String, pub model: String, pub ctx: String,
}

pub enum AgentEvent {
    Message(ChatMessage),
    Tool(ToolEvent),
    Summary(String),
    Status(String),
}

pub fn mock_tasks() -> Vec<Task> {
    vec![
        Task { id: 1, title: "Optimize local Colibri model".into(), status: TaskStatus::Running, updated: "2m ago".into(), goal: "Reduce local inference latency while preserving tool-call reliability.".into() },
        Task { id: 2, title: "Package desktop app".into(), status: TaskStatus::Reviewed, updated: "Yesterday".into(), goal: "Define the native application shell and release packaging path.".into() },
        Task { id: 3, title: "Fix streaming response state".into(), status: TaskStatus::NeedsAttention, updated: "Yesterday".into(), goal: "Prevent duplicate token events during reconnect.".into() },
        Task { id: 4, title: "Add repository indexer".into(), status: TaskStatus::Complete, updated: "Nov 7".into(), goal: "Build a local source map for code-aware retrieval.".into() },
        Task { id: 5, title: "Improve terminal output parsing".into(), status: TaskStatus::Complete, updated: "Nov 6".into(), goal: "Normalize command output for compact rendering.".into() },
        Task { id: 6, title: "Review local inference startup path".into(), status: TaskStatus::Complete, updated: "Nov 4".into(), goal: "Verify cold-start and device-tier residency.".into() },
    ]
}

pub fn mock_environment() -> Environment {
    Environment {
        runtime: "Rust 1.99 / native".into(), env: "local / active".into(),
        repo: "colibri-desktop".into(), branch: "feat/agent-shell".into(),
        changed: "4 files changed".into(), tests: "Passing".into(),
        model: "Colibri Qwen3-Coder 30B".into(), ctx: "32k tokens".into(),
    }
}

pub fn mock_subagents() -> Vec<Subagent> {
    vec![
        Subagent { name: "Planner".into(), status: "Complete".into(), desc: "Mapped packaging and runtime boundaries".into(), progress: 1.0 },
        Subagent { name: "Implementation".into(), status: "Working".into(), desc: "Updating task event pipeline".into(), progress: 0.68 },
        Subagent { name: "Verifier".into(), status: "Waiting".into(), desc: "Ready to run startup checks".into(), progress: 0.0 },
    ]
}

pub fn mock_artifacts() -> Vec<Artifact> {
    vec![
        Artifact { name: "app.py".into(), change: "Modified".into(), diff: "+46 −8".into(), content: "# entry point\nfn main() { run_agent(); }\n".into() },
        Artifact { name: "agent_runner.py".into(), change: "Modified".into(), diff: "+31 −4".into(), content: "def run_agent():\n    stream_events()\n".into() },
        Artifact { name: "components/task_timeline.py".into(), change: "Added".into(), diff: "+120".into(), content: "class TaskTimeline:\n    ...\n".into() },
        Artifact { name: "requirements.txt".into(), change: "Modified".into(), diff: "+2 −1".into(), content: "tauri==2\nserde\n".into() },
        Artifact { name: "README.md".into(), change: "Added".into(), diff: "+64".into(), content: "# Colibri Coding Agent\n".into() },
    ]
}

pub fn mock_sites() -> Vec<(String, String, String)> {
    vec![
        ("Python Packaging User Guide".into(), "packaging.python.org".into(), "Build and distribution reference".into()),
        ("Tauri 2 Documentation".into(), "tauri.app".into(), "Native shell + event streaming".into()),
        ("PyInstaller Documentation".into(), "pyinstaller.org".into(), "Legacy packaging path".into()),
        ("Local runtime notes".into(), "internal".into(), "Device-tier residency notes".into()),
    ]
}

/// Simulated agent run: emits an ack, a sequence of tool events, then a summary.
/// Runs on a background thread; the UI drains the channel each frame.
pub fn spawn_mock_run(prompt: String, tx: std::sync::mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let _ = tx.send(AgentEvent::Message(ChatMessage { role: Role::Assistant,
            text: "Got it. I'll inspect the current repository and runtime boundaries first, then map the smallest packaging path that preserves local inference.".into(),
            time: "now".into() }));
        let tools = vec![
            ("Inspect repository", "Inspect application entry points", "find . -maxdepth 2 -type f", "app.py\nagent_runner.py\ncomponents/\ntests/\nrequirements.txt\nREADME.md", ToolStatus::Complete, "1.3s"),
            ("Read file", "Read pyproject.toml", "pyproject.toml", "Detected Python 3.12, Streamlit runtime, and local packaging configuration.", ToolStatus::Complete, "0.4s"),
            ("Edit file", "Update agent_runner.py", "agent_runner.py", "Added streaming event normalization and structured tool event emission.", ToolStatus::Complete, "0.9s"),
            ("Run tests", "Run test suite", "pytest tests/ -q", "================ 18 passed in 4.21s ================", ToolStatus::Complete, "4.2s"),
            ("Run command", "Build desktop bundle", "python -m build", "Build completed with one optional dependency warning.", ToolStatus::Warning, "6.1s"),
            ("Search documentation", "Review Tauri application lifecycle", "Tauri 2 Rust event streaming", "Found recommended event channel patterns for background command progress.", ToolStatus::Complete, "0.7s"),
        ];
        for (cat, title, detail, output, status, dur) in tools {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = tx.send(AgentEvent::Tool(ToolEvent {
                category: cat.into(), title: title.into(), status,
                detail: detail.into(), output: output.into(), duration: dur.into(), expanded: false }));
        }
        let _ = tx.send(AgentEvent::Summary(
            "Repository inspection is complete. Added task timeline event model, artifact tracking, normalized command output, and verified the local startup path. Test result: 18 passed.".into()));
        let _ = tx.send(AgentEvent::Status("Complete".into()));
        let _ = prompt; // prompt reserved for real backend binding
    });
}