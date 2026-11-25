//! Embeds the Windows application icon (and, later, any other resource) into
//! the desktop binaries, so Explorer, the taskbar button and gpui's window
//! class all show the fennec mark from the moment the process starts.
//!
//! gpui has no window-icon option: its Windows backend calls
//! `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)` once and registers
//! the handle as the window class icon. The only way to change what the app
//! shows at start is therefore to put an icon group at resource ordinal 1 in
//! the executable, which is what `build/fennec-desktop.rc` declares.
//!
//! The GNU toolchain uses MinGW `windres`; MSVC uses `embed-resource` to run the
//! Windows SDK resource compiler. Both hand the resulting COFF object to the linker.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build/fennec-desktop.rc");
    println!("cargo:rerun-if-changed=build/icon.ico");

    let target = env::var("TARGET").unwrap_or_default();
    if !cfg!(windows) || !target.contains("windows") {
        // macOS and Linux take their icon from the bundle/desktop entry.
        return;
    }
    if target.contains("msvc") {
        let _ = embed_resource::compile("build/fennec-desktop.rc", embed_resource::NONE);
        return;
    }
    if !target.contains("gnu") {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let object = out_dir.join("fennec-desktop-res.o");
    let windres_target = if target.starts_with("aarch64") {
        "pe-aarch64-little"
    } else if target.starts_with("i686") {
        "pe-i386"
    } else {
        "pe-x86-64"
    };

    let status = Command::new("windres")
        .arg("--input")
        .arg("build/fennec-desktop.rc")
        .arg("--output")
        .arg(&object)
        .arg("--include-dir")
        .arg("build")
        .arg("--output-format=coff")
        .arg("--target")
        .arg(windres_target)
        // The .rc is UTF-8 with comments; without a codepage windres guesses.
        .args(["-c", "65001"])
        .status();

    match status {
        Ok(s) if s.success() => {
            // rustc 1.50+ forwards this to the linker for every bin target.
            println!("cargo:rustc-link-arg-bins={}", object.display());
        }
        Ok(s) => {
            println!("cargo:warning=windres exited {s}: the binaries keep the default Windows icon")
        }
        Err(e) => println!(
            "cargo:warning=windres did not run ({e}): the binaries keep the default Windows icon"
        ),
    }
}
