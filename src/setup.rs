use fennec_client::{git, suite};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub const SETUP_VERSION: &str = "1.0.0";
pub const RECOMMENDED_MODEL_GIB: f64 = 22.78;
pub const CORE_DEPENDENCIES: [&str; 4] = ["fastapi", "uvicorn", "psutil", "pydantic"];

pub fn system_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "Darwin",
        other => other,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConfigState {
    #[default]
    Absent,
    Valid,
    Invalid,
}

pub fn config_state(path: &Path) -> ConfigState {
    if !path.is_file() {
        return ConfigState::Absent;
    }
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    {
        Some(Value::Object(_)) => ConfigState::Valid,
        _ => ConfigState::Invalid,
    }
}

pub fn should_offer_first_run(
    config: ConfigState,
    had_user_data: bool,
    setup_skipped: bool,
) -> bool {
    config != ConfigState::Valid && !had_user_data && !setup_skipped
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupDraft {
    pub engine_root: String,
    pub suite_root: String,
    pub models_root: String,
    pub cache_root: String,
    pub python: String,
    pub model: String,
    pub backend: String,
    pub ctx: String,
    pub policy: String,
}

impl SetupDraft {
    pub fn from_suite(discovered: &suite::Suite, config: Option<&Value>) -> Self {
        let home = suite::home();
        let get = |key: &str| {
            config
                .and_then(Value::as_object)
                .and_then(|map| map.get(key))
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value.as_i64().map(|number| number.to_string()))
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        };
        let fallback = |key: &str, value: PathBuf| {
            let configured = get(key);
            if configured.trim().is_empty() {
                value.to_string_lossy().into_owned()
            } else {
                configured
            }
        };
        let engine_default = if discovered.engine_root.as_os_str().is_empty() {
            home.join("Fennec")
        } else {
            discovered.engine_root.clone()
        };
        let models_default = if discovered.models_root.as_os_str().is_empty() {
            home.join("fennec-models")
        } else {
            discovered.models_root.clone()
        };
        let suite_default = if discovered.root.as_os_str().is_empty() {
            home.join("Fennec-Suite")
        } else {
            discovered.root.clone()
        };
        Self {
            engine_root: fallback("engine_root", engine_default),
            suite_root: fallback("suite_root", suite_default),
            models_root: fallback("models_root", models_default),
            cache_root: fallback("cache_root", home.join(".cache").join("fennec")),
            python: get("python"),
            model: get("model"),
            backend: nonempty(get("backend"), "auto"),
            ctx: nonempty(get("ctx"), "0"),
            policy: nonempty(get("policy"), "quality"),
        }
    }

    pub fn core_script(&self) -> PathBuf {
        Path::new(&self.suite_root)
            .join("fennec-core")
            .join("setup_wizard.py")
    }

    pub fn python_command(&self) -> String {
        if self.python.trim().is_empty() {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        } else {
            self.python.clone()
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("engine checkout", self.engine_root.as_str()),
            ("suite checkout", self.suite_root.as_str()),
            ("models folder", self.models_root.as_str()),
            ("cache folder", self.cache_root.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{label} cannot be empty."));
            }
        }
        if self
            .ctx
            .trim()
            .parse::<u32>()
            .map_or(true, |value| value > 1_048_576)
        {
            return Err("Context must be a whole number from 0 to 1048576.".into());
        }
        if !matches!(
            self.backend.as_str(),
            "auto" | "cuda" | "vulkan" | "metal" | "ort" | "cpu"
        ) {
            return Err("Choose a supported engine backend.".into());
        }
        if !matches!(
            self.policy.as_str(),
            "quality" | "balanced" | "experimental-fast"
        ) {
            return Err("Choose a supported resource policy.".into());
        }
        Ok(())
    }

    pub fn finish_args(&self, download_model: bool) -> Result<Vec<String>, String> {
        self.validate()?;
        let mut args = vec![self.core_script().to_string_lossy().into_owned()];
        args.extend([
            "--engine-root".into(),
            self.engine_root.clone(),
            "--suite-root".into(),
            self.suite_root.clone(),
            "--models-root".into(),
            self.models_root.clone(),
            "--cache-root".into(),
            self.cache_root.clone(),
            "--python".into(),
            self.python_command(),
            "--backend".into(),
            self.backend.clone(),
            "--ctx".into(),
            self.ctx.trim().to_string(),
            "--policy".into(),
            self.policy.clone(),
            "--yes".into(),
            "--json".into(),
        ]);
        if !self.model.trim().is_empty() {
            args.push("--model".into());
            args.push(self.model.clone());
        }
        if download_model {
            args.push("--download-model".into());
        }
        Ok(args)
    }
}

fn nonempty(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.into()
    } else {
        value
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolStatus {
    pub ok: bool,
    pub detail: String,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ProbeResult {
    pub python: ToolStatus,
    pub git: ToolStatus,
    pub rust: ToolStatus,
    pub dependencies: ToolStatus,
    pub engine: ToolStatus,
    pub core_script: ToolStatus,
    pub python_venv: bool,
}

pub fn engine_binary(root: &Path) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "fennec.exe"
    } else {
        "fennec"
    };
    ["target/release", "target/debug", ""]
        .into_iter()
        .map(|part| root.join(part).join(name))
        .find(|path| path.is_file())
}

pub fn run_capture(program: &str, args: &[String], timeout: Duration) -> Result<Output, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git::quiet(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    let start = Instant::now();
    loop {
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            break;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} did not finish within {} seconds.",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    child
        .wait_with_output()
        .map_err(|error| format!("could not read {program} output: {error}"))
}

fn version_status(program: &str, args: &[String]) -> ToolStatus {
    match run_capture(program, args, Duration::from_secs(20)) {
        Ok(output) if output.status.success() => {
            let detail = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("ready")
                .to_string();
            ToolStatus {
                ok: true,
                detail,
                path: Some(program.into()),
            }
        }
        Ok(output) => ToolStatus {
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(400)
                .collect(),
            path: None,
        },
        Err(error) => ToolStatus {
            ok: false,
            detail: error,
            path: None,
        },
    }
}

fn python_status(command: &str) -> (ToolStatus, bool) {
    let code = "import json,sys; print(json.dumps({'version':'.'.join(map(str,sys.version_info[:3])),'venv':sys.prefix!=sys.base_prefix}))";
    match run_capture(
        command,
        &["-c".into(), code.into()],
        Duration::from_secs(20),
    ) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let data: Value = serde_json::from_str(text.trim()).unwrap_or(Value::Null);
            let version = data["version"].as_str().unwrap_or("unknown");
            let mut parts = version
                .split('.')
                .filter_map(|part| part.parse::<u32>().ok());
            let major = parts.next().unwrap_or(0);
            let minor = parts.next().unwrap_or(0);
            let ok = (major, minor) >= (3, 10);
            (
                ToolStatus {
                    ok,
                    detail: if ok {
                        format!("Python {version}")
                    } else {
                        format!("Python {version} is older than 3.10")
                    },
                    path: Some(command.into()),
                },
                data["venv"].as_bool().unwrap_or(false),
            )
        }
        Ok(output) => (
            ToolStatus {
                ok: false,
                detail: String::from_utf8_lossy(&output.stderr)
                    .trim()
                    .chars()
                    .take(400)
                    .collect(),
                path: None,
            },
            false,
        ),
        Err(error) => (
            ToolStatus {
                ok: false,
                detail: error,
                path: None,
            },
            false,
        ),
    }
}

fn dependency_status(python: &str) -> ToolStatus {
    let names = CORE_DEPENDENCIES.map(str::to_string);
    let code = format!(
        "import importlib,json; missing=[]\nfor name in {names:?}:\n try: importlib.import_module(name)\n except Exception: missing.append(name)\nprint(json.dumps(missing))"
    );
    match run_capture(python, &["-c".into(), code], Duration::from_secs(60)) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let missing: Vec<String> = serde_json::from_str(text.trim()).unwrap_or_default();
            if missing.is_empty() {
                ToolStatus {
                    ok: true,
                    detail: "FastAPI, Uvicorn, psutil, and Pydantic are ready.".into(),
                    path: None,
                }
            } else {
                ToolStatus {
                    ok: false,
                    detail: format!("Missing: {}", missing.join(", ")),
                    path: None,
                }
            }
        }
        Ok(output) => ToolStatus {
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(400)
                .collect(),
            path: None,
        },
        Err(error) => ToolStatus {
            ok: false,
            detail: error,
            path: None,
        },
    }
}

pub fn probe(draft: &SetupDraft) -> ProbeResult {
    let python_command = draft.python_command();
    let (python, python_venv) = python_status(&python_command);
    let dependencies = if python.ok {
        dependency_status(&python_command)
    } else {
        ToolStatus::default()
    };
    let git = git::which("git")
        .map(|path| version_status(path.to_string_lossy().as_ref(), &["--version".into()]))
        .unwrap_or_else(|| ToolStatus {
            ok: false,
            detail: "Git was not found on PATH.".into(),
            path: None,
        });
    let rust = git::which("cargo")
        .map(|path| version_status(path.to_string_lossy().as_ref(), &["--version".into()]))
        .unwrap_or_else(|| ToolStatus {
            ok: false,
            detail: "Rust/Cargo was not found on PATH.".into(),
            path: None,
        });
    let engine = engine_binary(Path::new(&draft.engine_root))
        .map(|path| ToolStatus {
            ok: true,
            detail: "Built Fennec binary found.".into(),
            path: Some(path.to_string_lossy().into_owned()),
        })
        .unwrap_or_else(|| ToolStatus {
            ok: false,
            detail: "No fennec binary under target/release, target/debug, or the checkout root."
                .into(),
            path: None,
        });
    let core_script = if draft.core_script().is_file() {
        ToolStatus {
            ok: true,
            detail: "Core setup helper found.".into(),
            path: Some(draft.core_script().to_string_lossy().into_owned()),
        }
    } else {
        ToolStatus {
            ok: false,
            detail: "fennec-core/setup_wizard.py is missing from the suite checkout.".into(),
            path: None,
        }
    };
    ProbeResult {
        python,
        git,
        rust,
        dependencies,
        engine,
        core_script,
        python_venv,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallTool {
    Python,
    Git,
    Rust,
}

pub fn install_plan(system: &str, tool: InstallTool) -> Option<Vec<String>> {
    let package = match tool {
        InstallTool::Python => "Python.Python.3.12",
        InstallTool::Git => "Git.Git",
        InstallTool::Rust => "Rustlang.Rustup",
    };
    match system {
        "Windows" => Some(vec![
            "winget".into(),
            "install".into(),
            "--id".into(),
            package.into(),
            "-e".into(),
            "--accept-package-agreements".into(),
            "--accept-source-agreements".into(),
        ]),
        "Darwin" => {
            let formula = match tool {
                InstallTool::Python => "python@3.12",
                InstallTool::Git => "git",
                InstallTool::Rust => "rustup",
            };
            Some(vec!["brew".into(), "install".into(), formula.into()])
        }
        _ => None,
    }
}

pub fn manual_install_hint(system: &str, tool: InstallTool) -> String {
    let package = match tool {
        InstallTool::Python => "python3",
        InstallTool::Git => "git",
        InstallTool::Rust => "rustup",
    };
    match system {
        "Windows" => format!("Install {package} with winget, then run Check again."),
        "Darwin" => format!("Install {package} with Homebrew, then run Check again."),
        "Ubuntu" | "Debian" => {
            format!("Run: sudo apt-get update && sudo apt-get install -y {package}")
        }
        "Fedora" => format!("Run: sudo dnf install -y {package}"),
        "Arch" => format!("Run: sudo pacman -S --needed {package}"),
        "linux" => format!("Install {package} with apt, dnf, or pacman, then run Check again."),
        _ => format!("Install {package} with your system package manager, then run Check again."),
    }
}

pub fn dependency_install_args(python: &str, venv: bool) -> Vec<String> {
    let mut args = vec![
        python.to_string(),
        "-m".into(),
        "pip".into(),
        "install".into(),
        "--disable-pip-version-check".into(),
        "--no-input".into(),
    ];
    if !venv {
        args.push("--user".into());
    }
    args.extend(CORE_DEPENDENCIES.map(str::to_string));
    args
}

#[derive(Clone, Debug)]
pub struct ModelEntry {
    pub name: String,
    pub path: String,
    pub size_gb: f64,
    pub ready: bool,
    pub detail: String,
}

pub fn scan_models(draft: &SetupDraft) -> Result<Vec<ModelEntry>, String> {
    let code = "import json,sys; from pathlib import Path; sys.path.insert(0, sys.argv[3]); from setup_wizard import scan_models; print(json.dumps(scan_models(sys.argv[1], cache_root=sys.argv[2])))";
    let core_dir = draft
        .core_script()
        .parent()
        .ok_or("core setup helper has no parent directory")?
        .to_string_lossy()
        .into_owned();
    let output = run_capture(
        &draft.python_command(),
        &[
            "-c".into(),
            code.into(),
            draft.models_root.clone(),
            draft.cache_root.clone(),
            core_dir,
        ],
        Duration::from_secs(120),
    )?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(800)
            .collect());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(text.trim()).map_err(|error| error.to_string())?;
    let rows = value.as_array().ok_or("model scan returned no list")?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            Some(ModelEntry {
                name: row["name"].as_str()?.into(),
                path: row["path"].as_str()?.into(),
                size_gb: row["size_gb"].as_f64().unwrap_or(0.0),
                ready: row["ready"].as_bool().unwrap_or(false),
                detail: row["detail"].as_str().unwrap_or("").into(),
            })
        })
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    Prerequisites,
    Paths,
    Model,
    Review,
}

impl WizardStep {
    pub const ALL: [WizardStep; 5] = [
        WizardStep::Welcome,
        WizardStep::Prerequisites,
        WizardStep::Paths,
        WizardStep::Model,
        WizardStep::Review,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::Prerequisites => "Prerequisites",
            Self::Paths => "Paths",
            Self::Model => "Model",
            Self::Review => "Review",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
pub struct WizardState {
    pub step: WizardStep,
    pub draft: SetupDraft,
    pub probe: Option<ProbeResult>,
    pub models: Vec<ModelEntry>,
    pub selected_model: String,
    pub download_model: bool,
    pub busy: bool,
    pub logs: Vec<String>,
    pub message: String,
    pub finished: Option<Value>,
    pub first_run: bool,
}

impl WizardState {
    pub fn new(draft: SetupDraft, first_run: bool) -> Self {
        let selected_model = draft.model.clone();
        Self {
            step: WizardStep::Welcome,
            draft,
            probe: None,
            models: Vec::new(),
            selected_model,
            download_model: false,
            busy: false,
            logs: Vec::new(),
            message: String::new(),
            finished: None,
            first_run,
        }
    }

    pub fn ready_to_finish(&self) -> bool {
        self.probe.as_ref().is_some_and(|probe| {
            probe.python.ok && probe.dependencies.ok && probe.engine.ok && probe.core_script.ok
        }) && self.draft.validate().is_ok()
    }

    pub fn sync_model_input(&mut self, entered: String) {
        self.draft.model = entered;
        if !self.draft.model.trim().is_empty() {
            self.selected_model = self.draft.model.clone();
        } else if !self.selected_model.trim().is_empty() {
            self.draft.model = self.selected_model.clone();
        }
    }
}

#[derive(Clone, Debug)]
pub enum SetupEvent {
    Probe(ProbeResult),
    Models(Vec<ModelEntry>),
    Log(String),
    Busy(bool),
    Finished(Result<Value, String>),
}

pub fn config_object(path: &Path) -> Option<Map<String, Value>> {
    match suite::read_config(path) {
        suite::ConfigRead::Object(map) => Some(map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("fennec-setup-{label}-{stamp}"))
    }

    #[test]
    fn first_run_never_traps_an_existing_user() {
        assert!(should_offer_first_run(ConfigState::Absent, false, false));
        assert!(should_offer_first_run(ConfigState::Invalid, false, false));
        assert!(!should_offer_first_run(ConfigState::Valid, false, false));
        assert!(!should_offer_first_run(ConfigState::Absent, true, false));
        assert!(!should_offer_first_run(ConfigState::Absent, false, true));
    }

    #[test]
    fn config_state_distinguishes_absent_invalid_and_valid() {
        let dir = temp_dir("config-state");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        assert_eq!(config_state(&path), ConfigState::Absent);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(config_state(&path), ConfigState::Invalid);
        std::fs::write(&path, "[]").unwrap();
        assert_eq!(config_state(&path), ConfigState::Invalid);
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(config_state(&path), ConfigState::Valid);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn draft_reads_shared_config_and_keeps_defaults() {
        let discovered = suite::Suite {
            root: PathBuf::from("/suite"),
            engine_root: PathBuf::from("/engine"),
            models_root: PathBuf::from("/models"),
            ..Default::default()
        };
        let config = json!({
            "engine_root": "/custom-engine",
            "model": "/custom-model",
            "backend": "vulkan",
            "ctx": 8192,
            "policy": "balanced"
        });
        let draft = SetupDraft::from_suite(&discovered, Some(&config));
        assert_eq!(draft.engine_root, "/custom-engine");
        assert_eq!(draft.suite_root, "/suite");
        assert_eq!(draft.model, "/custom-model");
        assert_eq!(draft.backend, "vulkan");
        assert_eq!(draft.ctx, "8192");
        assert_eq!(draft.policy, "balanced");
    }

    #[test]
    fn finish_args_are_explicit_and_never_shell_strings() {
        let mut draft = SetupDraft::from_suite(&suite::Suite::default(), None);
        draft.python = "/path with spaces/python".into();
        draft.model = "/models/my model".into();
        let args = draft.finish_args(true).unwrap();
        assert!(args[0].ends_with("setup_wizard.py"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--python", "/path with spaces/python"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "/models/my model"])
        );
        assert!(args.iter().any(|arg| arg == "--download-model"));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains(';') || arg.contains('`'))
        );
    }

    #[test]
    fn platform_install_plans_stay_explicit() {
        assert_eq!(
            install_plan("Windows", InstallTool::Python).unwrap()[0],
            "winget"
        );
        assert_eq!(install_plan("Darwin", InstallTool::Git).unwrap()[0], "brew");
        assert!(install_plan("Linux", InstallTool::Rust).is_none());
        assert!(manual_install_hint("Ubuntu", InstallTool::Git).contains("apt-get"));
    }

    #[test]
    fn dependency_install_respects_virtual_environments() {
        assert!(
            dependency_install_args("python", false)
                .iter()
                .any(|arg| arg == "--user")
        );
        assert!(
            !dependency_install_args("python", true)
                .iter()
                .any(|arg| arg == "--user")
        );
    }

    #[test]
    fn empty_model_input_keeps_the_scanned_selection() {
        let draft = SetupDraft::from_suite(&suite::Suite::default(), None);
        let mut state = WizardState::new(draft, true);
        state.selected_model = "/models/scanned".into();
        state.sync_model_input(String::new());
        assert_eq!(state.draft.model, "/models/scanned");
        state.sync_model_input("/models/typed".into());
        assert_eq!(state.selected_model, "/models/typed");
    }
}
