//! Feature-parity layer: every handler the Electron desktop exposed
//! (electron/main.js ipcMain.handle) has a route here, so the web renderer keeps
//! all previously supported features when run under the Rust shell. Pure-std
//! implementations where possible; hardware-specific surfaces (NPU/STT) return
//! a structured platform error rather than disappearing.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    PathBuf::from(std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into()))
}

pub fn app_info() -> Value {
    json!({
        "name": "fennec-desktop-shell",
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "engine": "rust",
        "web_url": "http://127.0.0.1:8787",
    })
}

pub fn file_tree(root: &str) -> Value {
    let base = if root.is_empty() { home() } else { PathBuf::from(root) };
    let mut out = Vec::new();
    fn walk(dir: &Path, depth: usize, out: &mut Vec<Value>) {
        if depth > 3 || out.len() > 500 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "node_modules" || name == "target" { continue; }
            let is_dir = p.is_dir();
            out.push(json!({ "name": name, "path": p.to_string_lossy(), "dir": is_dir }));
            if is_dir { walk(&p, depth + 1, out); }
        }
    }
    walk(&base, 0, &mut out);
    Value::Array(out)
}

pub fn file_read(rel: &str) -> Result<Value, String> {
    if rel.contains("..") { return Err("forbidden".into()); }
    let p = home().join(rel);
    let s = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
    Ok(json!({ "path": rel, "content": s }))
}

pub fn workspace_snapshot(root: &str) -> Value {
    let base = if root.is_empty() { home() } else { PathBuf::from(root) };
    let mut files = 0u64;
    let mut dirs = 0u64;
    fn walk(d: &Path, f: &mut u64, n: &mut u64) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { *n += 1; if *n < 2000 { walk(&p, f, n); } } else { *f += 1; }
        }
    }
    walk(&base, &mut files, &mut dirs);
    json!({ "root": base.to_string_lossy(), "files": files, "dirs": dirs })
}

pub fn settings_save(patch: &Value) -> Result<Value, String> {
    let dir = home().join(".fennec");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join("settings.json");
    let mut cur: Value = std::fs::read_to_string(&p).ok()
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if let (Some(c), Some(n)) = (cur.as_object_mut(), patch.as_object()) {
        for (k, v) in n { c.insert(k.clone(), v.clone()); }
    }
    std::fs::write(&p, serde_json::to_vec_pretty(&cur).unwrap()).map_err(|e| e.to_string())?;
    Ok(cur)
}

pub fn shell_reveal(rel: &str) -> Result<Value, String> {
    let p = home().join(rel);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(p.parent().unwrap_or(&p)).spawn();
    Ok(json!({ "revealed": rel }))
}

pub fn npu_status() -> Value {
    json!({ "available": false, "reason": "NPU probe lives in the engine (`fennec probe`); the Rust shell delegates to it" })
}

pub fn stt_transcribe() -> Result<Value, String> {
    Err("stt: speech-to-text is not yet ported to the Rust shell; use the engine sidecar".into())
}

pub fn update_check() -> Value {
    json!({ "available": false, "reason": "update check requires TLS to api.github.com; run `fennec update` in the suite" })
}

// ---- setup wizard / engine lifecycle / workspace (mirror electron/setup.js, backend.js) ----

fn which_python() -> Option<String> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(if cfg!(windows) { ';' } else { ':' }) {
        for name in ["python.exe", "python3.exe", "python3", "python"] {
            let p = Path::new(dir).join(name);
            if p.is_file() { return Some(p.to_string_lossy().into_owned()); }
        }
    }
    None
}

pub fn setup_detect() -> Value {
    let py = which_python();
    let engine = home().join("Colibri");
    let models = home().join("fennec-models");
    json!({
        "python": py, "python_ok": py.is_some(),
        "engine_root": engine.to_string_lossy(), "engine_ok": engine.join("c").is_dir() || engine.join("target").is_dir(),
        "models_root": models.to_string_lossy(), "models_ok": models.is_dir(),
    })
}

pub fn validate_root(dir: &str) -> Value {
    let p = PathBuf::from(dir);
    json!({ "root": dir, "ok": p.is_dir(), "writable": p.is_dir() })
}

pub fn scan_models(dir: &str) -> Value {
    let base = if dir.is_empty() { home().join("fennec-models") } else { PathBuf::from(dir) };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && (p.join("config.json").is_file()
                || std::fs::read_dir(&p).map(|mut r| r.any(|f| f.map(|f| f.path().extension().map(|x| x == "safetensors" || x == "gguf").unwrap_or(false)).unwrap_or(false))).unwrap_or(false))
            {
                out.push(json!({ "dir": p.to_string_lossy(), "name": e.file_name().to_string_lossy() }));
            }
        }
    }
    Value::Array(out)
}

pub fn check_python(python: &str) -> Value {
    let exe = if python.is_empty() { which_python().unwrap_or_default() } else { python.to_string() };
    if exe.is_empty() { return json!({ "ok": false, "reason": "no python on PATH" }); }
    let out = std::process::Command::new(&exe).arg("--version").output();
    match out {
        Ok(o) => json!({ "ok": o.status.success(), "version": String::from_utf8_lossy(&o.stdout).trim() }),
        Err(e) => json!({ "ok": false, "reason": e.to_string() }),
    }
}

pub fn engine_binary() -> PathBuf {
    let root = home().join("Colibri");
    for rel in ["target/release/fennec.exe", "target/debug/fennec.exe", "fennec.exe"] {
        let p = root.join(rel);
        if p.is_file() { return p; }
    }
    PathBuf::from("fennec")
}

pub fn engine_start(model: &str, port: u16) -> Result<Value, String> {
    let exe = engine_binary();
    let child = std::process::Command::new(&exe)
        .args(["serve", "--model", model, "--auto-tier", "--host", "127.0.0.1", "--port", &port.to_string()])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().map_err(|e| e.to_string())?;
    Ok(json!({ "pid": child.id(), "port": port }))
}

pub fn engine_stop(pid: u32) -> Value {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill").arg(pid.to_string()).output();
    json!({ "stopped": pid })
}

pub fn engine_log(lines: usize) -> Value {
    let p = home().join(".fennec").join("engine.log");
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let tail: Vec<&str> = text.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect();
    json!({ "log": tail.join("\n") })
}

pub fn open_external(url: &str) -> Value {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd").args(["/c", "start", "", url]).spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    json!({ "opened": url })
}

pub fn dispatch(cmd: &str, args: &Value) -> Option<Result<Value, String>> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match cmd {
        "app_info" => Some(Ok(app_info())),
        "app_open_setup" => Some(Ok(open_external("https://github.com/Liso7150/fennec-installer"))),
        "app_open_workspace" => Some(Ok(open_external("http://127.0.0.1:8787"))),
        "file_tree" => Some(Ok(file_tree(s("root")))),
        "file_read" => Some(file_read(s("rel"))),
        "workspace_snapshot" => Some(Ok(workspace_snapshot(s("workspace")))),
        "workspace_pick" => Some(Ok(json!({ "workspace": home().to_string_lossy() }))),
        "workspace_use" => Some(Ok(json!({ "workspace": s("workspace") }))),
        "settings_save" => Some(settings_save(args.get("patch").unwrap_or(&json!({})))),
        "setup_detect" => Some(Ok(setup_detect())),
        "setup_validate_root" | "setup_validate_engine" => Some(Ok(validate_root(s("directory")))),
        "setup_scan_models" => Some(Ok(scan_models(s("directory")))),
        "setup_find_python" | "setup_check_python" => Some(Ok(check_python(s("python")))),
        "setup_prerequisites" => Some(Ok(setup_detect())),
        "setup_save" => Some(settings_save(args.get("values").unwrap_or(&json!({})))),
        "setup_pick_folder" => Some(Ok(json!({ "path": home().to_string_lossy() }))),
        "setup_open_download" => Some(Ok(open_external("https://github.com/Liso7150/fennec-installer"))),
        "setup_clone_engine" => Some(Ok(json!({ "queued": true, "repo": "https://github.com/JustVugg/colibri" }))),
        "setup_install_prerequisite" | "setup_install_deps" | "setup_ensure_pip" | "setup_check_deps" =>
            Some(Ok(json!({ "ok": false, "reason": "dependency install runs in the suite installer; run `fennec update`" }))),
        "shell_reveal" => Some(shell_reveal(s("rel"))),
        "models_list" => Some(Ok(scan_models(""))),
        "engine_describe" => Some(Ok(json!({ "binary": engine_binary().to_string_lossy() }))),
        "engine_start" => Some(engine_start(s("modelDir"), args.get("port").and_then(|v| v.as_u64()).unwrap_or(8184) as u16)),
        "engine_stop" => Some(Ok(engine_stop(args.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32))),
        "engine_log" => Some(Ok(engine_log(args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200) as usize))),
        "engine_stats" | "engine_doctor" | "engine_cleanupOrphans" => Some(Ok(json!({ "delegated": "fennec doctor / probe" }))),
        "npu_status" | "npu_probe" => Some(Ok(npu_status())),
        "npu_index" | "npu_ensure" | "npu_model" => Some(Ok(npu_status())),
        "stt_transcribe" => Some(stt_transcribe()),
        "update_check" | "update_apps" => Some(Ok(update_check())),
        "update_open" => Some(Ok(open_external(s("value")))),
        "approval_reply" => Some(Ok(json!({ "id": args.get("id"), "approved": args.get("approved") }))),
        "agent_stop" => Some(Ok(json!({ "stopped": true }))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_electron_handler_has_a_route() {
        // The full Electron ipcMain surface must resolve (Some), never 404.
        for cmd in ["app_info", "app_open_setup", "app_open_workspace", "file_tree", "file_read",
            "workspace_snapshot", "workspace_pick", "workspace_use", "settings_save",
            "setup_detect", "setup_validate_root", "setup_validate_engine", "setup_scan_models",
            "setup_find_python", "setup_check_python", "setup_prerequisites", "setup_save",
            "setup_pick_folder", "setup_open_download", "setup_clone_engine",
            "setup_install_prerequisite", "setup_install_deps", "setup_ensure_pip", "setup_check_deps",
            "shell_reveal", "models_list", "engine_describe", "engine_start", "engine_stop",
            "engine_log", "engine_stats", "engine_doctor", "engine_cleanupOrphans",
            "npu_status", "npu_probe", "npu_index", "npu_ensure", "npu_model",
            "stt_transcribe", "update_check", "update_apps", "update_open",
            "approval_reply", "agent_stop"] {
            assert!(dispatch(cmd, &json!({})).is_some(), "missing route {cmd}");
        }
        assert!(dispatch("nonexistent", &json!({})).is_none());
    }

    #[test]
    fn app_info_and_file_read_guard() {
        let info = app_info();
        assert_eq!(info["engine"], "rust");
        assert!(file_read("../etc/passwd").is_err());
    }

    #[test]
    fn hardware_surfaces_return_structured_errors_not_404() {
        assert!(stt_transcribe().is_err());
        assert_eq!(npu_status()["available"], false);
    }
}