//! Fennec Desktop — native Rust GUI (egui/eframe). Replaces the Electron main
//! process and the React renderer. Every feature maps to the in-process backend
//! (parity/engine/config), no web view, no Node.
use eframe::egui;
use fennec_desktop_shell::parity;
use serde_json::{json, Value};

#[derive(PartialEq, Clone, Copy)]
enum Tab { Chat, Engine, Models, Files, Outlets, Setup }

struct App {
    tab: Tab,
    prompt: String,
    thread: Vec<(String, String)>,
    out: String,
    models: Vec<Value>,
    files: Vec<Value>,
    outlets: Vec<Value>,
    busy: bool,
}

impl Default for App {
    fn default() -> Self {
        Self { tab: Tab::Chat, prompt: String::new(), thread: Vec::new(), out: String::new(),
               models: Vec::new(), files: Vec::new(), outlets: Vec::new(), busy: false }
    }
}

fn call(cmd: &str, args: Value) -> Value {
    parity::dispatch(cmd, &args).unwrap_or(Ok(json!({}))).unwrap_or_else(|e| json!({ "error": e }))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Fennec Desktop");
                ui.label("native Rust · egui");
                for (t, label) in [(Tab::Chat, "Chat"), (Tab::Engine, "Engine"), (Tab::Models, "Models"),
                                   (Tab::Files, "Files"), (Tab::Outlets, "Outlets"), (Tab::Setup, "Setup")] {
                    if ui.selectable_label(self.tab == t, label).clicked() {
                        self.tab = t;
                        match t {
                            Tab::Models => self.models = call("models_list", json!({})).as_array().cloned().unwrap_or_default(),
                            Tab::Files => self.files = call("file_tree", json!({})).as_array().cloned().unwrap_or_default(),
                            Tab::Outlets => self.outlets = call("outlets_status", json!({"origins":["http://127.0.0.1:8184"]})).as_array().cloned().unwrap_or_default(),
                            Tab::Setup => self.out = serde_json::to_string_pretty(&call("setup_detect", json!({}))).unwrap_or_default(),
                            _ => {}
                        }
                    }
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.tab {
                Tab::Chat => {
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        for (role, text) in &self.thread {
                            ui.group(|ui| { ui.label(role); ui.label(text); });
                        }
                    });
                    ui.horizontal(|ui| {
                        let send = ui.button("Send").clicked();
                        ui.text_edit_singleline(&mut self.prompt);
                        if send && !self.prompt.trim().is_empty() {
                            let text = std::mem::take(&mut self.prompt);
                            self.thread.push(("you".into(), text.clone()));
                            let r = call("agent_send", json!({ "messages": [{ "role": "user", "content": text }] }));
                            let reply = r["reply"].as_str().map(|s| s.to_string())
                                .unwrap_or_else(|| r["error"].as_str().unwrap_or("(no reply)").to_string());
                            self.thread.push(("fennec".into(), reply));
                        }
                    });
                }
                Tab::Engine => {
                    ui.horizontal(|ui| {
                        if ui.button("Start").clicked() { self.out = serde_json::to_string_pretty(&call("engine_start", json!({"port":8184}))).unwrap_or_default(); }
                        if ui.button("Stop").clicked() { self.out = serde_json::to_string_pretty(&call("engine_stop", json!({}))).unwrap_or_default(); }
                        if ui.button("Stats").clicked() { self.out = serde_json::to_string_pretty(&call("engine_stats", json!({}))).unwrap_or_default(); }
                        if ui.button("Log").clicked() { self.out = call("engine_log", json!({"lines":100}))["log"].as_str().unwrap_or("").to_string(); }
                    });
                    ui.monospace(&self.out);
                }
                Tab::Models => {
                    for m in &self.models { ui.label(format!("{}  {}", m["name"].as_str().unwrap_or("?"), m["dir"].as_str().unwrap_or(""))); }
                    if self.models.is_empty() { ui.label("no models found"); }
                }
                Tab::Files => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for f in self.files.clone() {
                            let name = f["name"].as_str().unwrap_or("").to_string();
                            let path = f["path"].as_str().unwrap_or("").to_string();
                            if ui.link(&name).clicked() && !f["dir"].as_bool().unwrap_or(false) {
                                self.out = call("file_read", json!({ "rel": path }))["content"].as_str().unwrap_or("").to_string();
                            }
                        }
                    });
                    ui.monospace(&self.out);
                }
                Tab::Outlets => {
                    for o in &self.outlets { ui.label(format!("{}  loopback:{}  blocked:{}", o["origin"].as_str().unwrap_or("?"), o["loopback"], o["blocked"].as_str().unwrap_or("no"))); }
                }
                Tab::Setup => {
                    if ui.button("Detect").clicked() { self.out = serde_json::to_string_pretty(&call("setup_detect", json!({}))).unwrap_or_default(); }
                    ui.monospace(&self.out);
                }
            }
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 720.0]).with_title("Fennec Desktop"),
        ..Default::default()
    };
    eframe::run_native("Fennec Desktop", opts, Box::new(|_cc| Box::new(App::default())))
}