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
//! The GNU toolchain this repo builds with has no `rc.exe`, so this uses the
//! MinGW `windres` that ships beside it, and hands the resulting COFF object
//! straight to the linker the way `embed-resource` does.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build/fennec-desktop.rc");
    println!("cargo:rerun-if-changed=build/icon.ico");

    let target = env::var("TARGET").unwrap_or_default();
    if !cfg!(windows) || !target.contains("windows") || !target.contains("gnu") {
        // macOS and Linux take their icon from the bundle/desktop entry, and an
        // MSVC link would need rc.exe; neither needs (nor can take) this object.
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
        Ok(s) => println!(
            "cargo:warning=windres exited {s}: the binaries keep the default Windows icon"
        ),
        Err(e) => println!(
            "cargo:warning=windres did not run ({e}): the binaries keep the default Windows icon"
        ),
    }
}
