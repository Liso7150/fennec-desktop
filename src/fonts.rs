//! Typography: an Inter-equivalent proportional face and a JetBrains
//! Mono-equivalent monospace face, resolved from whatever the host OS already
//! ships, falling back silently to egui's bundled defaults. Nothing is
//! downloaded and nothing is bundled into the binary.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

/// Candidate faces per platform, best match first. The names are the ones
/// installed with the OS (or with the developer tools most users already have);
/// a missing file is simply skipped.
const MONO: &[&str] = &[
    "JetBrainsMono-Regular.ttf", "JetBrainsMono-Regular.otf",
    "CascadiaCode.ttf", "CascadiaMono.ttf",
    "consola.ttf", "Consolas.ttf",
    "DejaVuSansMono.ttf", "UbuntuMono-R.ttf",
    "Menlo-Regular.ttf", "SFMono-Regular.ttf",
];
const PROPORTIONAL: &[&str] = &[
    "Inter-Regular.otf", "Inter-Regular.ttf",
    "segoeui.ttf", "SegoeUI.ttf",
    "DejaVuSans.ttf", "Ubuntu-R.ttf", "NotoSans-Regular.ttf",
    "Helvetica.ttc", "HelveticaNeue.ttf", "LiberationSans-Regular.ttf",
];

fn dirs() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    #[cfg(windows)]
    {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:/Windows".into());
        v.push(std::path::PathBuf::from(windir).join("Fonts"));
    }
    #[cfg(target_os = "linux")]
    {
        v.push(std::path::PathBuf::from("/usr/share/fonts"));
        v.push(std::path::PathBuf::from("/usr/local/share/fonts"));
        if let Ok(h) = std::env::var("HOME") {
            v.push(std::path::Path::new(&h).join(".local/share/fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push(std::path::PathBuf::from("/System/Library/Fonts"));
        v.push(std::path::PathBuf::from("/Library/Fonts"));
        if let Ok(h) = std::env::var("HOME") {
            v.push(std::path::Path::new(&h).join("Library/Fonts"));
        }
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = &v;
    }
    v
}

/// First file under the platform font directories, searched shallowly: some
/// Linux distributions nest faces under `truetype/…` subdirectories.
fn locate(names: &[&str]) -> Option<std::path::PathBuf> {
    for dir in dirs() {
        for name in names {
            let direct = dir.join(name);
            if direct.is_file() {
                return Some(direct);
            }
        }
        for depth2 in ["", "truetype/", "opentype/", "TTF/", "OTF/", "JetBrainsMono/", "fantasque-sans/"] {
            for name in names {
                let p = dir.join(depth2).join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Returns what was actually installed (mono, proportional) so the Environment
/// panel can report honestly instead of claiming a face that fell back.
pub fn install(ctx: &egui::Context) -> (bool, bool) {
    // A full rebuild is needed before families take effect.
    let mut defs = FontDefinitions::default();
    let mut mono = false;
    let mut prop = false;
    if let Some(p) = locate(MONO) {
        if let Ok(bytes) = std::fs::read(&p) {
            defs.font_data.insert("fennec-mono".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            defs.families.entry(FontFamily::Monospace).or_default().insert(0, "fennec-mono".into());
            mono = true;
        }
    }
    if let Some(p) = locate(PROPORTIONAL) {
        if let Ok(bytes) = std::fs::read(&p) {
            defs.font_data.insert("fennec-ui".into(), std::sync::Arc::new(FontData::from_owned(bytes)));
            defs.families.entry(FontFamily::Proportional).or_default().insert(0, "fennec-ui".into());
            prop = true;
        }
    }
    ctx.set_fonts(defs);
    (mono, prop)
}
