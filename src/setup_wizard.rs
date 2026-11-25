use crate::setup::{
    self, InstallTool, SETUP_VERSION, SetupDraft, SetupEvent, ToolStatus, WizardState, WizardStep,
};
use crate::{App, BackendResponse};
use fennec_client::suite;
use fennec_ui::widgets::brand_fox;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, Theme, ThemeMode, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputState},
    label::Label,
    scroll::ScrollableElement,
    switch::Switch,
};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::Value;
use std::cell::RefCell;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};

#[derive(Clone)]
struct WizardInputs {
    engine: Entity<InputState>,
    suite: Entity<InputState>,
    models: Entity<InputState>,
    cache: Entity<InputState>,
    python: Entity<InputState>,
    model: Entity<InputState>,
    backend: Entity<InputState>,
    ctx: Entity<InputState>,
    policy: Entity<InputState>,
}

impl WizardInputs {
    fn new(draft: &SetupDraft, window: &mut Window, cx: &mut Context<App>) -> Self {
        let mut field = |value: &str, cx: &mut Context<App>| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx);
                input.set_value(value, window, cx);
                input
            })
        };
        Self {
            engine: field(&draft.engine_root, cx),
            suite: field(&draft.suite_root, cx),
            models: field(&draft.models_root, cx),
            cache: field(&draft.cache_root, cx),
            python: field(&draft.python, cx),
            model: field(&draft.model, cx),
            backend: field(&draft.backend, cx),
            ctx: field(&draft.ctx, cx),
            policy: field(&draft.policy, cx),
        }
    }

    fn sync_draft(&self, state: &Rc<RefCell<WizardState>>, cx: &mut gpui_kit::App) {
        let read = |entity: &Entity<InputState>| entity.read(cx).value().to_string();
        let mut state = state.borrow_mut();
        state.draft.engine_root = read(&self.engine);
        state.draft.suite_root = read(&self.suite);
        state.draft.models_root = read(&self.models);
        state.draft.cache_root = read(&self.cache);
        state.draft.python = read(&self.python);
        state.sync_model_input(read(&self.model));
        state.draft.backend = read(&self.backend);
        state.draft.ctx = read(&self.ctx);
        state.draft.policy = read(&self.policy);
    }
}

impl App {
    pub fn open_setup_wizard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        first_run: bool,
    ) {
        let environ = suite::real_environ();
        let home = suite::home();
        let config_path = suite::config_file_in(&environ, &home, suite::Relative::Home);
        let config = setup::config_object(&config_path).map(Value::Object);
        let draft = SetupDraft::from_suite(&suite::discover(), config.as_ref());
        let state = Rc::new(RefCell::new(WizardState::new(draft, first_run)));
        self.setup = Some(state.clone());
        self.setup_system_theme = true;
        Theme::change(ThemeMode::from(cx.window_appearance()), None, cx);
        self.setup_appearance_subscription =
            Some(cx.observe_window_appearance(window, |app, _, cx| {
                if app.setup_system_theme {
                    Theme::change(ThemeMode::from(cx.window_appearance()), None, cx);
                }
            }));
        let inputs = WizardInputs::new(&state.borrow().draft, window, cx);
        let weak = cx.entity().downgrade();
        let dialog_state = state.clone();
        let dialog_inputs = inputs.clone();
        let close_weak = weak.clone();

        window.open_dialog(cx, move |dialog, _, _| {
            let state = dialog_state.clone();
            let inputs = dialog_inputs.clone();
            let weak = weak.clone();
            dialog
                .w(px(820.0))
                .title("Fennec Setup")
                .entrance(gpui_kit::component::dialog::DialogEntrance::None)
                .content(move |content, window, cx| {
                    let snapshot = state.borrow().clone();
                    let t = cx.theme().clone();
                    content
                        .w(px(788.0))
                        .min_h(px(470.0))
                        .flex()
                        .flex_row()
                        .child(
                            div()
                                .w(px(164.0))
                                .flex_shrink_0()
                                .p_3()
                                .border_r_1()
                                .border_color(t.border)
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(div().mb_2().child(brand_fox(&t, 34.0)))
                                .children(WizardStep::ALL.into_iter().enumerate().map(
                                    |(index, step)| {
                                        let state = state.clone();
                                        let active = snapshot.step == step;
                                        let reached = index <= snapshot.step.index();
                                        let theme = t.clone();
                                        div()
                                            .id(ElementId::Name(
                                                format!("setup-step-{index}").into(),
                                            ))
                                            .w_full()
                                            .h(px(38.0))
                                            .px_3()
                                            .rounded_lg()
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .hover(|d| d.bg(fennec_ui::theme::hover()))
                                            .when(active, |d| {
                                                d.bg(theme.muted)
                                                    .border_l_2()
                                                    .border_color(theme.accent)
                                            })
                                            .when(!reached, |d| d.opacity(0.45))
                                            .on_click(move |_, window, _| {
                                                if reached {
                                                    state.borrow_mut().step = step;
                                                    window.refresh();
                                                }
                                            })
                                            .child(
                                                Label::new(step.label())
                                                    .text_size(px(13.0))
                                                    .text_color(theme.foreground),
                                            )
                                    },
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .p_4()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(render_step(&snapshot, &inputs, &state, &weak, window, cx))
                                .child(render_logs(&snapshot, &t))
                                .child(render_navigation(
                                    &snapshot, &inputs, &state, &weak, window, cx,
                                )),
                        )
                })
                .footer(div().flex().justify_end().p_3().child(
                    Button::new("setup-close").label("Close").ghost().on_click({
                        let weak = close_weak.clone();
                        move |_, window, cx| {
                            let _ = weak.update(cx, |app, cx| app.close_setup_wizard(true, cx));
                            window.close_dialog(cx);
                        }
                    }),
                ))
                .on_close({
                    let weak = close_weak.clone();
                    move |_, window, cx| {
                        let _ = weak.update(cx, |app, cx| app.close_setup_wizard(true, cx));
                        window.close_dialog(cx);
                    }
                })
        });
        self.spawn_setup_probe(state);
    }

    fn spawn_setup_probe(&self, state: Rc<RefCell<WizardState>>) {
        let draft = state.borrow().draft.clone();
        let tx = self.response_tx.clone();
        tx.send(BackendResponse::Setup(SetupEvent::Busy(true))).ok();
        std::thread::spawn(move || {
            let result = setup::probe(&draft);
            tx.send(BackendResponse::Setup(SetupEvent::Probe(result)))
                .ok();
            tx.send(BackendResponse::Setup(SetupEvent::Busy(false)))
                .ok();
        });
    }

    fn spawn_setup_models(&self, state: Rc<RefCell<WizardState>>) {
        let draft = state.borrow().draft.clone();
        let tx = self.response_tx.clone();
        tx.send(BackendResponse::Setup(SetupEvent::Busy(true))).ok();
        std::thread::spawn(move || {
            let result = setup::scan_models(&draft);
            let event = match result {
                Ok(models) => SetupEvent::Models(models),
                Err(error) => SetupEvent::Log(format!("Model scan failed: {error}")),
            };
            tx.send(BackendResponse::Setup(event)).ok();
            tx.send(BackendResponse::Setup(SetupEvent::Busy(false)))
                .ok();
        });
    }

    fn spawn_setup_action(
        &self,
        _state: Rc<RefCell<WizardState>>,
        program: &str,
        args: Vec<String>,
        cwd: Option<PathBuf>,
    ) {
        let tx = self.response_tx.clone();
        let program = program.to_string();
        tx.send(BackendResponse::Setup(SetupEvent::Busy(true))).ok();
        std::thread::spawn(move || {
            let result = stream_command(&program, &args, cwd.as_deref(), &tx);
            match result {
                Ok(_) => tx
                    .send(BackendResponse::Setup(SetupEvent::Log(
                        "Action finished.".into(),
                    )))
                    .ok(),
                Err(error) => tx
                    .send(BackendResponse::Setup(SetupEvent::Log(format!(
                        "Action failed: {error}"
                    ))))
                    .ok(),
            };
            tx.send(BackendResponse::Setup(SetupEvent::Busy(false)))
                .ok();
        });
    }

    fn spawn_setup_finish(&self, state: Rc<RefCell<WizardState>>) {
        let draft = state.borrow().draft.clone();
        let download = state.borrow().download_model;
        let args = match draft.finish_args(download) {
            Ok(args) => args,
            Err(error) => {
                state.borrow_mut().message = error;
                return;
            }
        };
        let tx = self.response_tx.clone();
        let program = draft.python_command();
        let cwd = Path::new(&draft.suite_root).join("fennec-core");
        state.borrow_mut().busy = true;
        state.borrow_mut().logs.clear();
        state.borrow_mut().message.clear();
        std::thread::spawn(move || {
            let result = stream_command(&program, &args, Some(&cwd), &tx).and_then(|text| {
                serde_json::from_str::<Value>(text.trim())
                    .map_err(|error| format!("setup returned no valid JSON: {error}"))
            });
            let event = match result {
                Ok(value) if value.get("config").is_some() => SetupEvent::Finished(Ok(value)),
                Ok(_) => SetupEvent::Finished(Err("setup did not return a configuration.".into())),
                Err(error) => SetupEvent::Finished(Err(error)),
            };
            tx.send(BackendResponse::Setup(event)).ok();
        });
    }

    pub fn apply_setup_event(&mut self, event: SetupEvent, cx: &mut Context<Self>) {
        let Some(state) = self.setup.clone() else {
            return;
        };
        match event {
            SetupEvent::Probe(probe) => {
                state.borrow_mut().probe = Some(probe);
                state.borrow_mut().message.clear();
            }
            SetupEvent::Models(models) => {
                let selected = state.borrow().selected_model.clone();
                state.borrow_mut().models = models;
                if selected.is_empty() {
                    let first = state
                        .borrow()
                        .models
                        .iter()
                        .find(|model| model.ready)
                        .map(|model| model.path.clone());
                    if let Some(first) = first {
                        state.borrow_mut().selected_model = first.clone();
                        state.borrow_mut().draft.model = first;
                    }
                }
            }
            SetupEvent::Log(line) => {
                let mut state = state.borrow_mut();
                state.logs.push(line);
                if state.logs.len() > 200 {
                    let excess = state.logs.len() - 200;
                    state.logs.drain(0..excess);
                }
            }
            SetupEvent::Busy(busy) => state.borrow_mut().busy = busy,
            SetupEvent::Finished(Ok(value)) => {
                state.borrow_mut().busy = false;
                state.borrow_mut().finished = Some(value.clone());
                state.borrow_mut().message =
                    "Configuration written. You can close setup now.".into();
                if let Some(config) = value.get("config").and_then(Value::as_object) {
                    if let Some(model) = config.get("model").and_then(Value::as_str) {
                        if !model.trim().is_empty() {
                            self.model = model.to_string();
                        }
                    }
                    if let Some(root) = config.get("suite_root").and_then(Value::as_str) {
                        self.suite_root = PathBuf::from(root);
                    }
                }
                self.setup_completed_version = Some(SETUP_VERSION.into());
                self.setup_skipped = false;
                self.save_settings();
                self.toast("Fennec setup complete.", true);
            }
            SetupEvent::Finished(Err(error)) => {
                state.borrow_mut().busy = false;
                state.borrow_mut().message = format!("Setup failed: {error}");
            }
        }
        cx.notify();
    }

    fn close_setup_wizard(&mut self, skip: bool, cx: &mut Context<Self>) {
        let first_run = self
            .setup
            .as_ref()
            .map_or(false, |state| state.borrow().first_run);
        let finished = self
            .setup
            .as_ref()
            .map_or(false, |state| state.borrow().finished.is_some());
        if skip && first_run && !finished {
            self.setup_skipped = true;
        }
        self.setup = None;
        self.setup_appearance_subscription = None;
        if self.setup_system_theme {
            if let Some(preset) = fennec_ui::theme::theme_presets()
                .get(fennec_ui::theme::find_preset_index(&self.theme))
            {
                preset.apply(cx);
            }
            self.setup_system_theme = false;
        }
        self.save_settings();
    }
}

fn stream_command(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    tx: &Sender<BackendResponse>,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    fennec_client::git::quiet(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    let stdout = child.stdout.take().ok_or("could not read setup stdout")?;
    let stderr = child.stderr.take().ok_or("could not read setup stderr")?;
    let combined = Arc::new(Mutex::new(Vec::<String>::new()));
    let stdout_lines = combined.clone();
    let stdout_tx = tx.clone();
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            stdout_lines.lock().unwrap().push(line.clone());
            stdout_tx
                .send(BackendResponse::Setup(SetupEvent::Log(line)))
                .ok();
        }
    });
    let stderr_lines = combined.clone();
    let stderr_tx = tx.clone();
    let stderr_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            stderr_lines.lock().unwrap().push(line.clone());
            stderr_tx
                .send(BackendResponse::Setup(SetupEvent::Log(line)))
                .ok();
        }
    });
    let status = child
        .wait()
        .map_err(|error| format!("setup process failed: {error}"))?;
    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    let text = combined.lock().unwrap().join("\n");
    if status.success() {
        Ok(text)
    } else {
        let tail: String = text
            .chars()
            .rev()
            .take(1200)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        Err(format!("{program} exited with {status}: {tail}"))
    }
}

fn render_step(
    snapshot: &WizardState,
    inputs: &WizardInputs,
    state: &Rc<RefCell<WizardState>>,
    weak: &gpui::WeakEntity<App>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> Div {
    let t = cx.theme().clone();
    match snapshot.step {
        WizardStep::Welcome => div().flex().flex_col().gap_4()
            .child(Label::new("Welcome to Fennec").font_weight(FontWeight::SEMIBOLD).text_size(px(24.0)).text_color(t.foreground))
            .child(Label::new("Set up the local engine, coding service, and model folder in one pass. Fennec stays on this machine; nothing is uploaded.")
                .w(px(560.0)).text_size(px(14.0)).text_color(t.muted_foreground))
            .child(info_card("setup-welcome-changes", "What setup changes", "Setup checks Python, Git, Rust, the built Fennec engine, core dependencies, and your model folder. Finish writes ~/.fennec/config.json and backs up an existing file.", &t))
            .child(info_card("setup-welcome-installs", "Safe installs", "Package-manager actions run only after you click them. Linux commands are shown for you to run because they may need administrator rights.", &t)),
        WizardStep::Prerequisites => render_prerequisites(snapshot, inputs, state, weak, window, &t),
        WizardStep::Paths => render_paths(snapshot, inputs, state, weak, window, &t),
        WizardStep::Model => render_models(snapshot, inputs, state, weak, window, &t),
        WizardStep::Review => render_review(snapshot, &t),
    }
}

fn info_card(id: &'static str, title: &str, body: &str, theme: &Theme) -> Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .w(px(560.0))
        .p_3()
        .rounded_lg()
        .bg(theme.muted)
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            Label::new(title)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(13.0))
                .text_color(theme.foreground),
        )
        .child(
            Label::new(body)
                .text_size(px(12.0))
                .text_color(theme.muted_foreground),
        )
}

fn status_row(id: &'static str, label: &str, status: &ToolStatus, theme: &Theme) -> Stateful<Div> {
    let detail = match &status.path {
        Some(path) if !status.detail.is_empty() => format!("{} · {}", status.detail, path),
        Some(path) => path.clone(),
        None if status.detail.is_empty() => "Not checked yet.".to_string(),
        None => status.detail.clone(),
    };
    div()
        .id(ElementId::Name(id.into()))
        .w(px(560.0))
        .p_3()
        .rounded_lg()
        .bg(theme.muted)
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(10.0))
                .h(px(10.0))
                .mt_1()
                .rounded_full()
                .bg(if status.ok {
                    theme.success
                } else {
                    theme.yellow
                }),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    Label::new(label)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(13.0))
                        .text_color(theme.foreground),
                )
                .child(
                    Label::new(detail)
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground),
                ),
        )
}

fn render_prerequisites(
    snapshot: &WizardState,
    inputs: &WizardInputs,
    state: &Rc<RefCell<WizardState>>,
    weak: &gpui::WeakEntity<App>,
    _window: &mut Window,
    theme: &Theme,
) -> Div {
    let inputs = inputs.clone();
    let probe = snapshot.probe.clone().unwrap_or_default();
    let system = setup::system_name();
    let mut column = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            Label::new("Prerequisites")
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(20.0))
                .text_color(theme.foreground),
        )
        .child(path_field(
            "Python interpreter",
            &inputs.python,
            "setup-python",
            None,
            theme,
        ))
        .child(status_row(
            "setup-status-python",
            "Python 3.10+",
            &probe.python,
            theme,
        ))
        .child(status_row("setup-status-git", "Git", &probe.git, theme))
        .child(status_row(
            "setup-status-rust",
            "Rust / Cargo",
            &probe.rust,
            theme,
        ))
        .child(status_row(
            "setup-status-dependencies",
            "Core Python dependencies",
            &probe.dependencies,
            theme,
        ));
    for (tool, label, status) in [
        (InstallTool::Python, "Install Python", &probe.python),
        (InstallTool::Git, "Install Git", &probe.git),
        (InstallTool::Rust, "Install Rust", &probe.rust),
    ] {
        if !status.ok {
            if let Some(args) = setup::install_plan(system, tool) {
                let available = match system {
                    "Windows" => fennec_client::git::which("winget").is_some(),
                    "Darwin" => fennec_client::git::which("brew").is_some(),
                    _ => false,
                };
                if available {
                    let state = state.clone();
                    let weak = weak.clone();
                    column = column.child(
                        Button::new(format!("setup-install-{label}"))
                            .label(label)
                            .ghost()
                            .small()
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |app, _| {
                                    app.spawn_setup_action(
                                        state.clone(),
                                        &args[0],
                                        args[1..].to_vec(),
                                        None,
                                    )
                                });
                            }),
                    );
                }
            }
            column = column.child(
                div()
                    .id(ElementId::Name(
                        format!("setup-install-hint-{label}").into(),
                    ))
                    .child(
                        Label::new(setup::manual_install_hint(system, tool))
                            .w(px(560.0))
                            .text_size(px(11.0))
                            .text_color(theme.muted_foreground),
                    ),
            );
        }
    }
    if probe.python.ok {
        let state = state.clone();
        let weak = weak.clone();
        let python = snapshot.draft.python_command();
        let venv = probe.python_venv;
        column = column.child(
            Button::new("setup-install-deps")
                .label("Install core Python dependencies")
                .ghost()
                .small()
                .disabled(probe.dependencies.ok)
                .on_click(move |_, _, cx| {
                    let args = setup::dependency_install_args(&python, venv);
                    let _ = weak.update(cx, |app, _| {
                        app.spawn_setup_action(state.clone(), &args[0], args[1..].to_vec(), None)
                    });
                }),
        );
    }
    let state = state.clone();
    let weak = weak.clone();
    column.child(
        Button::new("setup-check-again")
            .label("Check again")
            .ghost()
            .small()
            .on_click(move |_, window, cx| {
                inputs.sync_draft(&state, cx);
                let _ = weak.update(cx, |app, _| app.spawn_setup_probe(state.clone()));
                window.refresh();
            }),
    )
}

fn render_paths(
    snapshot: &WizardState,
    inputs: &WizardInputs,
    state: &Rc<RefCell<WizardState>>,
    weak: &gpui::WeakEntity<App>,
    window: &mut Window,
    theme: &Theme,
) -> Div {
    let probe = snapshot.probe.clone().unwrap_or_default();
    let mut column = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            Label::new("Engine and folders")
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(20.0))
                .text_color(theme.foreground),
        )
        .child(path_field(
            "Fennec engine checkout",
            &inputs.engine,
            "setup-engine",
            Some(inputs.engine.clone()),
            theme,
        ))
        .child(path_field(
            "Fennec suite checkout",
            &inputs.suite,
            "setup-suite",
            Some(inputs.suite.clone()),
            theme,
        ))
        .child(path_field(
            "Models folder",
            &inputs.models,
            "setup-models",
            Some(inputs.models.clone()),
            theme,
        ))
        .child(path_field(
            "Cache folder",
            &inputs.cache,
            "setup-cache",
            Some(inputs.cache.clone()),
            theme,
        ))
        .child(status_row(
            "setup-status-engine",
            "Built engine",
            &probe.engine,
            theme,
        ))
        .child(status_row(
            "setup-status-core",
            "Core setup helper",
            &probe.core_script,
            theme,
        ));
    if !probe.engine.ok && probe.rust.ok {
        let state = state.clone();
        let weak = weak.clone();
        column = column.child(
            Button::new("setup-build-engine")
                .label("Build Fennec engine")
                .ghost()
                .small()
                .on_click(move |_, _, cx| {
                    let draft = state.borrow().draft.clone();
                    let _ = weak.update(cx, |app, _| {
                        app.spawn_setup_action(
                            state.clone(),
                            "cargo",
                            vec!["build".into(), "--release".into()],
                            Some(PathBuf::from(draft.engine_root)),
                        )
                    });
                }),
        );
    }
    let _ = window;
    column
}

fn render_models(
    snapshot: &WizardState,
    inputs: &WizardInputs,
    state: &Rc<RefCell<WizardState>>,
    weak: &gpui::WeakEntity<App>,
    _window: &mut Window,
    theme: &Theme,
) -> Div {
    let inputs = inputs.clone();
    let mut column = div().flex().flex_col().gap_3()
        .child(Label::new("Choose a model").font_weight(FontWeight::SEMIBOLD).text_size(px(20.0)).text_color(theme.foreground))
        .child(Label::new("Select a ready local model, enter a path, or download the recommended Fennec-ready int4 checkpoint.")
            .w(px(560.0)).text_size(px(12.0)).text_color(theme.muted_foreground));
    if snapshot.models.is_empty() {
        column = column.child(info_card("setup-no-models", "No local models found", "The scan uses fennec-core's model classifier. You can still enter a model path or download the recommended checkpoint.", theme));
    }
    for (index, model) in snapshot.models.iter().enumerate() {
        let selected = snapshot.selected_model == model.path;
        let state = state.clone();
        let inputs = inputs.clone();
        let path = model.path.clone();
        let name = model.name.clone();
        let theme = theme.clone();
        column = column.child(
            Button::new(format!("setup-model-{index}"))
                .w(px(560.0))
                .justify_start()
                .ghost()
                .when(selected, |button| {
                    button
                        .bg(theme.muted)
                        .border_l_2()
                        .border_color(theme.accent)
                })
                .on_click(move |_, window, cx| {
                    state.borrow_mut().selected_model = path.clone();
                    state.borrow_mut().draft.model = path.clone();
                    inputs
                        .model
                        .update(cx, |input, cx| input.set_value(path.clone(), window, cx));
                    window.refresh();
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w_full()
                        .child(
                            Label::new(name)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(13.0))
                                .text_color(theme.foreground),
                        )
                        .child(
                            Label::new(format!(
                                "{:.2} GB · {}",
                                model.size_gb,
                                if model.ready {
                                    "ready"
                                } else {
                                    model.detail.as_str()
                                }
                            ))
                            .text_size(px(11.0))
                            .text_color(if model.ready {
                                theme.success
                            } else {
                                theme.yellow
                            }),
                        ),
                ),
        );
    }
    column = column.child(path_field(
        "Model path",
        &inputs.model,
        "setup-model-path",
        None,
        theme,
    ));
    let download_state = state.clone();
    column = column.child(div().w(px(560.0)).p_3().rounded_lg().bg(theme.muted).border_1().border_color(theme.border)
        .flex().items_center().justify_between().gap_3()
        .child(div().flex_1().flex().flex_col().gap_1()
            .child(Label::new("Download the recommended model").font_weight(FontWeight::SEMIBOLD).text_size(px(13.0)).text_color(theme.foreground))
            .child(Label::new(format!("Liso/qwen36-35b-colibri · Fennec-ready int4 · up to {:.2} GiB. Existing files are kept and the download can resume.", setup::RECOMMENDED_MODEL_GIB))
                .text_size(px(11.0)).text_color(theme.muted_foreground)))
        .child(Switch::new("setup-download-model").checked(snapshot.download_model)
            .on_click(move |checked, window, _| {
                download_state.borrow_mut().download_model = *checked;
                window.refresh();
            })));
    let state = state.clone();
    let weak = weak.clone();
    column.child(
        Button::new("setup-scan-models")
            .label("Scan again")
            .ghost()
            .small()
            .on_click(move |_, window, cx| {
                inputs.sync_draft(&state, cx);
                let _ = weak.update(cx, |app, _| app.spawn_setup_models(state.clone()));
                window.refresh();
            }),
    )
}

fn render_review(snapshot: &WizardState, theme: &Theme) -> Div {
    let mut column = div().flex().flex_col().gap_3()
        .child(Label::new("Review and finish").font_weight(FontWeight::SEMIBOLD).text_size(px(20.0)).text_color(theme.foreground))
        .child(info_card("setup-review-config", "Configuration", "Finish writes ~/.fennec/config.json atomically. An existing file is backed up first; secrets stay in environment variables and are never written by setup.", theme));
    let python = snapshot.draft.python_command();
    for (label, value) in [
        ("Engine", snapshot.draft.engine_root.as_str()),
        ("Suite", snapshot.draft.suite_root.as_str()),
        ("Models", snapshot.draft.models_root.as_str()),
        ("Cache", snapshot.draft.cache_root.as_str()),
        ("Python", python.as_str()),
        (
            "Model",
            if snapshot.download_model {
                "recommended download"
            } else {
                snapshot.draft.model.as_str()
            },
        ),
        ("Backend", snapshot.draft.backend.as_str()),
        ("Context", snapshot.draft.ctx.as_str()),
        ("Policy", snapshot.draft.policy.as_str()),
    ] {
        column = column.child(
            div()
                .id(ElementId::Name(format!("setup-review-{label}").into()))
                .w(px(560.0))
                .flex()
                .justify_between()
                .gap_3()
                .py_1()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    Label::new(label)
                        .text_size(px(12.0))
                        .text_color(theme.muted_foreground),
                )
                .child(
                    Label::new(if value.is_empty() {
                        "not selected"
                    } else {
                        value
                    })
                    .font_family("monospace")
                    .text_size(px(11.0))
                    .text_right()
                    .text_color(theme.foreground)
                    .flex_1()
                    .truncate(),
                ),
        );
    }
    if !snapshot.ready_to_finish() {
        column = column.child(info_card("setup-review-blocked", "Finish is blocked", "Python 3.10+, core dependencies, a built Fennec binary, and fennec-core/setup_wizard.py must be ready. Go back to Prerequisites or Paths to fix the missing item.", theme));
    }
    column
}

fn render_logs(snapshot: &WizardState, theme: &Theme) -> AnyElement {
    if snapshot.logs.is_empty() && snapshot.message.is_empty() {
        return div().into_any_element();
    }
    let mut lines: Vec<String> = snapshot.logs.iter().rev().take(8).rev().cloned().collect();
    if !snapshot.message.is_empty() {
        lines.push(snapshot.message.clone());
    }
    div()
        .w(px(560.0))
        .max_h(px(150.0))
        .overflow_y_scrollbar()
        .p_2()
        .rounded_md()
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .children(lines.into_iter().enumerate().map(|(index, line)| {
            div()
                .id(ElementId::NamedInteger("setup-log".into(), index as u64))
                .child(
                    Label::new(line)
                        .font_family("monospace")
                        .text_size(px(10.0))
                        .text_color(theme.muted_foreground),
                )
        }))
        .into_any_element()
}

fn render_navigation(
    snapshot: &WizardState,
    inputs: &WizardInputs,
    state: &Rc<RefCell<WizardState>>,
    weak: &gpui::WeakEntity<App>,
    _window: &mut Window,
    _cx: &mut gpui_kit::App,
) -> Div {
    let inputs = inputs.clone();
    let mut left = div().flex().gap_2();
    if snapshot.step != WizardStep::Welcome {
        let state = state.clone();
        left = left.child(Button::new("setup-back").label("Back").ghost().on_click(
            move |_, window, _| {
                let mut state = state.borrow_mut();
                let index = state.step.index().saturating_sub(1);
                state.step = WizardStep::ALL[index];
                window.refresh();
            },
        ));
    }
    if snapshot.first_run && snapshot.finished.is_none() {
        let weak = weak.clone();
        left = left.child(
            Button::new("setup-not-now")
                .label("Not now")
                .ghost()
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |app, cx| app.close_setup_wizard(true, cx));
                    window.close_dialog(cx);
                }),
        );
    }
    let mut right = div().flex().gap_2();
    if snapshot.finished.is_some() {
        let weak = weak.clone();
        right = right.child(Button::new("setup-done").label("Done").primary().on_click(
            move |_, window, cx| {
                let _ = weak.update(cx, |app, cx| app.close_setup_wizard(false, cx));
                window.close_dialog(cx);
            },
        ));
    } else if snapshot.step == WizardStep::Review {
        let state = state.clone();
        let weak = weak.clone();
        right = right.child(
            Button::new("setup-finish")
                .label("Finish setup")
                .primary()
                .disabled(!snapshot.ready_to_finish() || snapshot.busy)
                .on_click(move |_, window, cx| {
                    inputs.sync_draft(&state, cx);
                    let _ = weak.update(cx, |app, _| app.spawn_setup_finish(state.clone()));
                    window.refresh();
                }),
        );
    } else {
        let state = state.clone();
        let weak = weak.clone();
        right = right.child(
            Button::new("setup-next")
                .label(if snapshot.step == WizardStep::Welcome {
                    "Get started"
                } else {
                    "Next"
                })
                .primary()
                .disabled(snapshot.busy)
                .on_click(move |_, window, cx| {
                    inputs.sync_draft(&state, cx);
                    if state.borrow().step == WizardStep::Paths {
                        let _ = weak.update(cx, |app, _| app.spawn_setup_models(state.clone()));
                    }
                    let mut state = state.borrow_mut();
                    let index = (state.step.index() + 1).min(WizardStep::ALL.len() - 1);
                    state.step = WizardStep::ALL[index];
                    window.refresh();
                }),
        );
    }
    div()
        .w(px(560.0))
        .flex()
        .justify_between()
        .items_center()
        .child(left)
        .child(right)
}

fn path_field(
    label: &str,
    input: &Entity<InputState>,
    id: &'static str,
    browse: Option<Entity<InputState>>,
    theme: &Theme,
) -> Stateful<Div> {
    let mut row = div()
        .id(ElementId::Name(id.into()))
        .w(px(560.0))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            Label::new(label)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(px(12.0))
                .text_color(theme.foreground),
        )
        .child(Input::new(input).id(id).h(px(36.0)));
    if let Some(input) = browse {
        row = row.child(
            Button::new(format!("{id}-browse"))
                .label("Browse…")
                .ghost()
                .xsmall()
                .on_click(move |_, window, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: None,
                    });
                    let input = input.clone();
                    window
                        .spawn(cx, async move |cx| {
                            if let Ok(Ok(Some(paths))) = rx.await {
                                if let Some(path) = paths.first() {
                                    let value = path.to_string_lossy().to_string();
                                    let _ = cx.update(|window, cx| {
                                        input.update(cx, |state, cx| {
                                            state.set_value(value, window, cx)
                                        });
                                        window.refresh();
                                    });
                                }
                            }
                        })
                        .detach();
                }),
        );
    }
    row
}
