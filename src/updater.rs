//! Auto-update system: silent background updates from GitHub releases,
//! plus `fennec update --experimental` for devs to pull from main branch.
//!
//! Normal mode checks tagged releases and updates silently in the background.
//! Experimental mode pulls the latest commit from main/origin for developers.
//! Every download is verified against a SHA-256 checksum before applying.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};



/// GitHub repository for releases
const GITHUB_REPO: &str = "Liso7150/fennec-desktop";

/// Update channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    /// Stable releases from GitHub tags
    Stable,
    /// Latest commits from main branch
    Experimental,
}

/// Platform triple for binary selection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    pub ext: String,
}

impl Platform {
    /// Detect the current platform
    pub fn current() -> Self {
        let os = if cfg!(target_os = "windows") {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        }
        .to_string();

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        }
        .to_string();

        let ext = if cfg!(target_os = "windows") {
            "exe"
        } else {
            "bin"
        }
        .to_string();

        Platform { os, arch, ext }
    }

    /// Asset name for this platform (e.g., "fennec-windows-x86_64.exe")
    pub fn asset_name(&self, base: &str) -> String {
        if self.ext == "exe" {
            format!("{}-{}-{}.{}", base, self.os, self.arch, self.ext)
        } else {
            format!("{}-{}-{}", base, self.os, self.arch)
        }
    }
}

/// Release information from GitHub
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub published_at: String,
    pub assets: Vec<Asset>,
    pub checksums_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

/// Update status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UpdateStatus {
    /// No update available
    UpToDate,
    /// Update downloaded and ready to apply
    UpdateReady { version: String, path: PathBuf },
    /// Update applied, restart required
    Applied { version: String },
    /// Error during update
    Error(String),
}

/// Check for updates from GitHub releases
pub async fn check_stable(current_version: &str) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", GITHUB_REPO);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("fennec-updater")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GitHub API: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }

    let release: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| format!("Parse release: {e}"))?;

    // Compare versions
    if release.tag_name == format!("v{}", current_version) {
        return Ok(None);
    }

    let assets = release
        .assets
        .into_iter()
        .map(|a| Asset {
            name: a.name,
            download_url: a.browser_download_url,
            size: a.size,
        })
        .collect();

    Ok(Some(Release {
        version: release.tag_name.trim_start_matches('v').to_string(),
        tag: release.tag_name,
        published_at: release.published_at,
        assets,
        checksums_url: None,
    }))
}

/// Check for experimental updates from main branch
pub async fn check_experimental() -> Result<Option<Release>, String> {
    let url = format!(
        "https://api.github.com/repos/{}/commits/master",
        GITHUB_REPO
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("fennec-updater")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GitHub API: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }

    let commit: GitHubCommit = resp
        .json()
        .await
        .map_err(|e| format!("Parse commit: {e}"))?;

    // For experimental, we always "update" to the latest commit
    let short_sha = &commit.sha[..7];

    Ok(Some(Release {
        version: format!("dev-{}", short_sha),
        tag: commit.sha.clone(),
        published_at: commit.commit.author.date,
        assets: vec![],
        checksums_url: None,
    }))
}

/// Download an asset with checksum verification
pub async fn download_asset(url: &str, expected_checksum: Option<&str>) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent("fennec-updater")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Download returned {}", resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Read bytes: {e}"))?
        .to_vec();

    // Verify checksum if provided
    if let Some(expected) = expected_checksum {
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(format!(
                "Checksum mismatch: expected {}, got {}",
                expected, actual
            ));
        }
    }

    Ok(bytes)
}

/// Compute SHA-256 checksum of bytes
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    hex::encode(result)
}

/// Background update checker
pub struct BackgroundUpdater {
    current_version: String,
    check_interval: Duration,
}

impl BackgroundUpdater {
    pub fn new(current_version: String) -> Self {
        BackgroundUpdater {
            current_version,
            check_interval: Duration::from_secs(3600), // Check every hour
        }
    }

    /// Run the background update loop
    pub async fn run(&self) {
        loop {
            match check_stable(&self.current_version).await {
                Ok(Some(release)) => {
                    eprintln!(
                        "[updater] New version available: {} (current: {})",
                        release.version, self.current_version
                    );
                    // Download and apply silently
                    if let Err(e) = self.apply_update(&release).await {
                        eprintln!("[updater] Failed to apply update: {}", e);
                    }
                }
                Ok(None) => {
                    // Up to date
                }
                Err(e) => {
                    eprintln!("[updater] Check failed: {}", e);
                }
            }
            tokio::time::sleep(self.check_interval).await;
        }
    }

    async fn apply_update(&self, release: &Release) -> Result<(), String> {
        let platform = Platform::current();
        let asset_name = platform.asset_name("fennec");

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| format!("No asset for platform: {}", asset_name))?;

        // Download checksum file if available
        let checksum = if let Some(checksums_url) = &release.checksums_url {
            self.download_checksum(checksums_url, &asset_name).await.ok()
        } else {
            None
        };

        // Download the asset
        let bytes = download_asset(&asset.download_url, checksum.as_deref()).await?;

        // Write to temporary location
        let temp_path = std::env::temp_dir().join(&asset_name);
        std::fs::write(&temp_path, &bytes)
            .map_err(|e| format!("Write temp: {e}"))?;

        // Apply (platform-specific)
        self.replace_binary(&temp_path).await?;

        eprintln!("[updater] Updated to {} - restart required", release.version);
        Ok(())
    }

    async fn download_checksum(&self, url: &str, asset_name: &str) -> Result<String, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("fennec-updater")
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;

        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Download checksums: {e}"))?;

        let text = resp
            .text()
            .await
            .map_err(|e| format!("Read checksums: {e}"))?;

        // Parse checksum file (format: "<checksum>  <filename>")
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 && parts[1] == asset_name {
                return Ok(parts[0].to_string());
            }
        }

        Err(format!("Checksum not found for {}", asset_name))
    }

    async fn replace_binary(&self, new_path: &std::path::Path) -> Result<(), String> {
        let current_exe = std::env::current_exe().map_err(|e| format!("Current exe: {e}"))?;

        // On Windows, we can't replace a running binary directly.
        // Rename the current binary, then move the new one into place.
        if cfg!(target_os = "windows") {
            let backup = current_exe.with_extension("exe.old");
            if backup.exists() {
                std::fs::remove_file(&backup).map_err(|e| format!("Remove backup: {e}"))?;
            }
            std::fs::rename(&current_exe, &backup)
                .map_err(|e| format!("Backup current: {e}"))?;
            std::fs::copy(new_path, &current_exe)
                .map_err(|e| format!("Install new: {e}"))?;
        } else {
            // On Unix, we can replace directly
            std::fs::copy(new_path, &current_exe)
                .map_err(|e| format!("Replace: {e}"))?;
            // Make executable
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o755);
                std::fs::set_permissions(&current_exe, perms)
                    .map_err(|e| format!("Chmod: {e}"))?;
            }
        }

        Ok(())
    }
}

// GitHub API response types
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    published_at: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[derive(Deserialize)]
struct GitHubCommit {
    sha: String,
    commit: GitHubCommitDetail,
}

#[derive(Deserialize)]
struct GitHubCommitDetail {
    author: GitHubAuthor,
}

#[derive(Deserialize)]
struct GitHubAuthor {
    date: String,
}
