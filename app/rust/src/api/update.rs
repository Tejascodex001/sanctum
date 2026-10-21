//! Updates over the air: the newest release on GitHub, checked and
//! downloaded through Tor (GitHub sees a Tor exit, never this phone).
//! Android installs it only if it's signed with the same key as the app,
//! so a tampered download can't be installed.

use super::session::runtime;
use sanctum_net::https::{self, Connect};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

const REPO: &str = "https://github.com/Tejascodex001/sanctum";
/// The APK each release carries (see .github/workflows/android.yml).
const ASSET: &str = "app-release.apk";
const MAX_APK: usize = 200 << 20;
/// A check that takes longer has hung (a stuck Tor circuit): give up, so
/// the next check can run.
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Set once Tor is up.
pub(crate) static CONNECT: LazyLock<Mutex<Option<Connect>>> = LazyLock::new(Default::default);
static RECEIVED: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);

pub struct UpdateInfo {
    pub build: i64,
    pub url: String,
    /// What changed since `current`, newest first.
    pub notes: Vec<ReleaseNotes>,
}

pub struct ReleaseNotes {
    pub build: i64,
    /// Markdown: "- **Change**" lines with "  - detail" under them.
    pub text: String,
}

/// The "## Build N" sections of a changelog newer than `current`.
fn notes_since(changelog: &str, current: i64) -> Vec<ReleaseNotes> {
    changelog
        .split("\n## Build ")
        .skip(1)
        .filter_map(|section| {
            let (number, text) = section.split_once('\n').unwrap_or((section, ""));
            let build = number.trim().parse::<i64>().ok()?;
            (build > current).then(|| ReleaseNotes {
                build,
                text: text.trim().to_string(),
            })
        })
        .collect()
}

fn connect() -> anyhow::Result<Connect> {
    CONNECT.lock().unwrap().clone().ok_or_else(|| anyhow::anyhow!("Tor isn't connected yet"))
}

/// The newest release, if it's newer than `current` (this app's build).
/// Uses `releases/latest`, which redirects to the newest release's page:
/// no API, so no rate limits on shared Tor exits.
pub fn update_check(current: i64) -> anyhow::Result<Option<UpdateInfo>> {
    let connect = connect()?;
    let latest = runtime().block_on(async {
        tokio::time::timeout(CHECK_TIMEOUT, https::redirect_of(&connect, &format!("{REPO}/releases/latest"))).await
    })??;
    let tag = latest.rsplit('/').next().unwrap_or_default();
    let Some(build) = tag.strip_prefix("build-").and_then(|n| n.parse::<i64>().ok()) else {
        anyhow::bail!("unexpected release: {latest}");
    };
    if build <= current {
        return Ok(None);
    }
    // Older releases have no changelog; the update is still offered.
    let url = format!("{REPO}/releases/download/{tag}/changelog.md");
    let changelog = runtime()
        .block_on(async { tokio::time::timeout(CHECK_TIMEOUT, https::get(&connect, &url, 1 << 20, &|_, _| {})).await })
        .ok()
        .and_then(Result::ok)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    Ok(Some(UpdateInfo {
        build,
        url: format!("{REPO}/releases/download/{tag}/{ASSET}"),
        notes: notes_since(&changelog, current),
    }))
}

/// Downloads the update to `path` (progress: [`update_progress`]).
pub fn update_download(url: String, path: String) -> anyhow::Result<()> {
    anyhow::ensure!(url.starts_with(&format!("{REPO}/releases/download/")), "not a Sanctum release");
    let connect = connect()?;
    RECEIVED.store(0, Ordering::Relaxed);
    TOTAL.store(0, Ordering::Relaxed);
    let apk = runtime().block_on(https::get(&connect, &url, MAX_APK, &|got, total| {
        RECEIVED.store(got, Ordering::Relaxed);
        TOTAL.store(total.unwrap_or(0), Ordering::Relaxed);
    }))?;
    anyhow::ensure!(apk.starts_with(b"PK"), "not an APK");
    if let Some(dir) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = format!("{path}.part");
    std::fs::write(&partial, apk)?;
    std::fs::rename(partial, path)?;
    Ok(())
}

/// 0–1 while downloading (0 until the size is known).
pub fn update_progress() -> f64 {
    let total = TOTAL.load(Ordering::Relaxed);
    if total == 0 {
        return 0.0;
    }
    RECEIVED.load(Ordering::Relaxed) as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_newer_than_installed() {
        let log = "# Sanctum changelog\n\n## Build 27\n\n- **Updates**\n  - over Tor\n\n## Build 26\n\n- **Nearby**\n\n## Build 25\n\n- **Groups**\n";
        let notes = notes_since(log, 25);
        assert_eq!(notes.iter().map(|n| n.build).collect::<Vec<_>>(), [27, 26]);
        assert_eq!(notes[0].text, "- **Updates**\n  - over Tor");
        assert!(notes_since(log, 27).is_empty());
        assert!(notes_since("", 1).is_empty());
    }
}
