//! Fennec Desktop / Colibri Coding Agent — native Rust (egui) implementation of
//! the fennec-desktop 2.0.0 spec: three-pane shell, chat timeline, tool cards,
//! composer, environment/subagents/artifacts/sites, Colibri Night theme,
//! keyboard shortcuts, mock streaming. No web view, no Node.
use eframe::egui;
use fennec_desktop_shell::agent::{self, AgentEvent, Artifact, ChatMessage, Role, Task, TaskStatus, ToolEvent, ToolStatus};
use fennec_desktop_shell::parity;
use serde_json::json;
use std::sync::mpsc;

const BG: egui::Color32 = egui::Color32::from_rgb(21, 18, 15);
const PANEL: egui::Color32 = egui::Color32::from_rgb(27, 23, 20);
const SURFACE: egui::Color32 = egui::Color32::from_rgb(40, 33, 26);
const BORDER: egui::Color32 = egui::Color32::from_rgb(60, 50, 42);
const TEXT: egui::Color32 = egui::Color32::from_rgb(242, 238, 232);
const TEXT2: egui::Color32 = egui::Color32::from_rgb(169, 157, 145);
const BRAND: egui::Color32 = egui::Color32::from_rgb(233, 134, 93);
const OK: egui::Color32 = egui::Color32::from_rgb(94, 201, 143);
const ERR: egui::Color32 = egui::Color32::from_rgb(239, 97, 92);

fn status_color(s: TaskStatus) -> egui::Color32 {
    match s { TaskStatus::Running => BRAND, TaskStatus::Complete => OK,
              TaskStatus::NeedsAttention => ERR, TaskStatus::Reviewed => TEXT2 }
}
fn tool_color(s: ToolStatus) -> egui::Color32 {
    match s { ToolStatus::Running => BRAND, ToolStatus::Complete => OK,
              ToolStatus::Warning => egui::Color32::from_rgb(224, 164, 88), ToolStatus::Failed => ERR }
}

struct App {
    tasks: Vec<Task>, selected: usize,
    messages: Vec<ChatMessage>, tools: Vec<ToolEvent>, summary: String,
    composer: String, model: String, web: bool, local_tools: bool,
    env: agent::Environment, subagents: Vec<agent::Subagent>, artifacts: Vec<Artifact>,
    sites: Vec<(String, String, String)>,
    left_collapsed: bool, right_open: bool, search_open: bool, search: String,
    preview: Option<usize>, status: String,
    rx: mpsc::Receiver<AgentEvent>, tx: mpsc::Sender<AgentEvent>,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tasks: agent::mock_tasks(), selected: 0,
            messages: vec![ChatMessage { role: Role::User, text: "Turn Colibri into a packaged desktop coding-agent application. Keep local-model support, add a project-aware workspace, and make terminal execution visible without overwhelming the chat.".into(), time: "09:12".into() }],
            tools: Vec::new(), summary: String::new(),
            composer: String::new(), model: "Colibri Qwen3-Coder 30B".into(), web: false, local_tools: true,
            env: agent::mock_environment(), subagents: agent::mock_subagents(), artifacts: agent::mock_artifacts(),
            sites: agent::mock_sites(),
            left_collapsed: false, right_open: true, search_open: false, search: String::new(),
            preview: None, status: "Running".into(),
            rx, tx,
        }
    }
    fn send(&mut self) {
        let text = std::mem::take(&mut self.composer);
        if text.trim().is_empty() { return; }
        self.messages.push(ChatMessage { role: Role::User, text: text.clone(), time: "now".into() });
        self.status = "Running".into();
        agent::spawn_mock_run(text, self.tx.clone());
    }
    fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                AgentEvent::Message(m) => self.messages.push(m),
                AgentEvent::Tool(t) => self.tools.push(t),
                AgentEvent::Summary(s) => self.summary = s,
                AgentEvent::Status(s) => self.status = s,
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        // keyboard shortcuts
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::K) { self.search_open = !self.search_open; }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::N) {
                let id = self.tasks.len() as u32 + 1;
                self.tasks.push(Task { id, title: format!("New task {id}"), status: TaskStatus::Running, updated: "now".into(), goal: String::new() });
                self.selected = self.tasks.len() - 1;
            }
            if i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter) { self.send(); }
            if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) { self.search_open = false; self.preview = None; }
        });

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(BRAND, "◆ Colibri");
                ui.label(egui::RichText::new("Coding Agent").color(TEXT2).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(&self.status).color(BRAND).size(12.0));
                    ui.label(egui::RichText::new("local mode").color(TEXT2).size(12.0));
                });
            });
        });

        // left sidebar
        egui::SidePanel::left("left").resizable(true).default_width(252.0).min_width(64.0).show(ctx, |ui| {
            if self.left_collapsed {
                if ui.button("»").on_hover_text("Expand").clicked() { self.left_collapsed = false; }
                for (i, t) in self.tasks.iter().enumerate() {
                    if ui.selectable_label(self.selected == i, "•").on_hover_text(&t.title).clicked() { self.selected = i; }
                }
                return;
            }
            ui.horizontal(|ui| {
                ui.colored_label(BRAND, "◆"); ui.strong("Colibri");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("«").clicked() { self.left_collapsed = true; }
                });
            });
            ui.label(egui::RichText::new("Local workspace · colibri-desktop").color(TEXT2).size(12.0));
            ui.separator();
            for (label, key) in [("Coding", ""), ("New Task", "Ctrl+N"), ("My Attention", ""), ("Search", "Ctrl+K")] {
                if ui.selectable_label(label == "Coding", label).on_hover_text(key).clicked() {
                    if label == "Search" { self.search_open = true; }
                    if label == "New Task" { let id = self.tasks.len() as u32 + 1; self.tasks.push(Task { id, title: format!("New task {id}"), status: TaskStatus::Running, updated: "now".into(), goal: String::new() }); self.selected = self.tasks.len() - 1; }
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("RECENT TASKS").color(TEXT2).size(11.0));
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, t) in self.tasks.iter().enumerate() {
                    let sel = self.selected == i;
                    let r = ui.selectable_label(sel, egui::RichText::new(&t.title).color(if sel { BRAND } else { TEXT }).size(13.0));
                    if r.clicked() { self.selected = i; }
                    ui.horizontal(|ui| {
                        ui.colored_label(status_color(t.status), "●");
                        ui.label(egui::RichText::new(&t.updated).color(TEXT2).size(11.0).monospace());
                    });
                }
            });
            ui.separator();
            ui.label(egui::RichText::new(format!("{}  {}", self.env.repo, self.env.branch)).monospace().size(12.0).color(TEXT2));
            ui.label(egui::RichText::new(&self.env.changed).color(TEXT2).size(11.0));
            ui.horizontal(|ui| { ui.colored_label(BRAND, "AM"); ui.label(egui::RichText::new("Alex Morgan · Local mode").size(12.0).color(TEXT2)); });
        });

        // right context rail
        if self.right_open {
            egui::SidePanel::right("right").resizable(true).default_width(320.0).show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading(egui::RichText::new("Environment").size(13.0).color(TEXT2));
                    egui::Grid::new("env").num_columns(2).show(ui, |ui| {
                        for (k, v) in [("Runtime", &self.env.runtime), ("Env", &self.env.env), ("Repo", &self.env.repo), ("Branch", &self.env.branch), ("Tree", &self.env.changed), ("Tests", &self.env.tests), ("Model", &self.env.model), ("Ctx", &self.env.ctx)] {
                            ui.label(egui::RichText::new(k).color(TEXT2).size(12.0));
                            ui.label(egui::RichText::new(v).monospace().size(12.0).color(if k == "Tests" { OK } else { TEXT }));
                            ui.end_row();
                        }
                    });
                    ui.separator();
                    ui.heading(egui::RichText::new("Subagents").size(13.0).color(TEXT2));
                    for s in &self.subagents {
                        ui.horizontal(|ui| { ui.colored_label(if s.progress >= 1.0 { OK } else { BRAND }, "●"); ui.strong(&s.name); ui.label(egui::RichText::new(&s.status).color(TEXT2).size(11.0)); });
                        ui.label(egui::RichText::new(&s.desc).color(TEXT2).size(12.0));
                        ui.add(egui::ProgressBar::new(s.progress).desired_width(200.0));
                    }
                    ui.separator();
                    ui.heading(egui::RichText::new("Artifacts").size(13.0).color(TEXT2));
                    for (i, a) in self.artifacts.iter().enumerate() {
                        if ui.link(egui::RichText::new(format!("{}  {}", a.name, a.change)).size(12.0)).clicked() { self.preview = Some(i); }
                        ui.label(egui::RichText::new(&a.diff).monospace().size(11.0).color(TEXT2));
                    }
                    ui.separator();
                    ui.heading(egui::RichText::new("Sites").size(13.0).color(TEXT2));
                    for (t, d, desc) in &self.sites {
                        ui.hyperlink_to(egui::RichText::new(t).size(12.0), format!("https://{d}"));
                        ui.label(egui::RichText::new(format!("{d} · {desc}")).color(TEXT2).size(11.0));
                    }
                });
            });
        }

        // center workspace
        egui::CentralPanel::default().show(ctx, |ui| {
            let t = &self.tasks[self.selected];
            ui.label(egui::RichText::new(format!("{} / {}", self.env.repo, self.env.branch)).monospace().size(11.0).color(TEXT2));
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new(&t.title).size(20.0).color(TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Context").clicked() { self.right_open = !self.right_open; }
                    ui.colored_label(status_color(t.status), format!("● {}", t.status.label()));
                });
            });
            ui.label(egui::RichText::new(&t.goal).color(TEXT2).size(13.0));
            ui.separator();
            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                for m in &self.messages {
                    let (who, col) = match m.role { Role::User => ("You", TEXT), Role::Assistant => ("Colibri", BRAND) };
                    ui.horizontal(|ui| { ui.colored_label(col, who); ui.label(egui::RichText::new(&m.time).monospace().size(11.0).color(TEXT2)); });
                    ui.label(egui::RichText::new(&m.text).color(TEXT).size(14.0));
                    ui.add_space(14.0);
                }
                if !self.messages.is_empty() {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Plan").strong().size(13.0));
                        for (i, s) in ["Inspect project layout and entry points", "Review local model runtime configuration", "Add the desktop application shell", "Surface terminal actions in the task timeline", "Run unit tests and verify startup flow"].iter().enumerate() {
                            ui.label(egui::RichText::new(format!("{}. {}", i + 1, s)).size(13.0).color(TEXT2));
                        }
                    });
                }
                for (i, tool) in self.tools.iter_mut().enumerate() {
                    let head = format!("{}  ·  {}  ·  {}  ·  {}", tool.category, tool.title, tool.status.label(), tool.duration);
                    let r = ui.collapsing(egui::RichText::new(head).size(13.0).color(tool_color(tool.status)), |ui| {
                        ui.label(egui::RichText::new(&tool.detail).monospace().size(12.0).color(TEXT2));
                        ui.label(egui::RichText::new(&tool.output).monospace().size(12.0).color(TEXT));
                    });
                    let _ = (i, r);
                }
                if !self.summary.is_empty() {
                    ui.group(|ui| { ui.label(egui::RichText::new(&self.summary).color(OK).size(13.0)); });
                }
            });
            ui.separator();
            // composer
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("").size(14.0));
                ui.label(egui::RichText::new("⌕").size(14.0));
            });
            let resp = ui.add(egui::TextEdit::multiline(&mut self.composer).desired_rows(2).hint_text("Ask Colibri to inspect, edit, test, or explain your code.  (Ctrl+Enter to send)"));
            let _ = resp;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.model).monospace().size(12.0).color(TEXT2));
                ui.checkbox(&mut self.web, "web");
                ui.checkbox(&mut self.local_tools, "local tools");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled(!self.composer.trim().is_empty(), egui::Button::new(egui::RichText::new("Send ↑").color(egui::Color32::from_rgb(34, 18, 13))).fill(BRAND)).clicked() { self.send(); }
                });
            });
        });

        // artifact preview overlay
        if let Some(i) = self.preview {
            let a = self.artifacts[i].clone();
            egui::Window::new(format!("{}  ({})", a.name, a.change)).collapsible(false).show(ctx, |ui| {
                ui.label(egui::RichText::new(&a.diff).monospace().size(12.0).color(TEXT2));
                ui.label(egui::RichText::new(&a.content).monospace().size(12.0));
                if ui.button("Close").clicked() { self.preview = None; }
            });
        }
        // search overlay
        if self.search_open {
            egui::Window::new("Search").collapsible(false).show(ctx, |ui| {
                ui.text_edit_singleline(&mut self.search);
                for t in self.tasks.iter().filter(|t| t.title.to_lowercase().contains(&self.search.to_lowercase())) {
                    ui.label(egui::RichText::new(&t.title).size(13.0));
                }
                if ui.button("Close").clicked() { self.search_open = false; }
            });
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

fn main() -> Result<(), eframe::Error> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 960.0]).with_min_inner_size([1180.0, 760.0]).with_title("Colibri Coding Agent"),
        ..Default::default()
    };
    eframe::run_native("Colibri Coding Agent", opts, Box::new(|cc| {
        cc.egui_ctx.style_mut(|s| {
            s.visuals.dark_mode = true;
            s.visuals.panel_fill = BG;
            s.visuals.window_fill = PANEL;
            s.visuals.extreme_bg_color = SURFACE;
            s.visuals.widgets.noninteractive.bg_fill = PANEL;
            s.visuals.widgets.inactive.bg_fill = SURFACE;
            s.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(53, 34, 24);
            s.visuals.selection.bg_fill = BRAND;
            s.visuals.hyperlink_color = BRAND;
            s.visuals.window_stroke = egui::Stroke::new(1.0, BORDER);
            s.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
        });
        Box::new(App::new())
    }))
}