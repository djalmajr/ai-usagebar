//! A tray installed by Scoop updates through Scoop.
//!
//! Scoop keeps every version of an app under `<root>\apps\<name>\<version>\`
//! and points the `current` junction at the active one; swapping exes in
//! place there would leave Scoop's own bookkeeping (and its shims) behind.
//! So when the running exe sits under such a tree the tray asks Scoop for
//! the update instead: the check reads the bucket's manifest and the install
//! hands `scoop update <name>` to a detached PowerShell that waits for the
//! tray to exit, updates, and starts the new one.
//!
//! Everything that decides something is pure and compiled on every OS so
//! Linux CI tests it: detection takes the exe path and an `exists` probe,
//! the parsers take text, and the script is a string. Only the two process
//! spawns are Windows-specific.

#![cfg_attr(not(windows), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::RELAUNCH_ENV;

/// Where a Scoop-installed tray lives: `<root>/apps/<name>/...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoopApp {
    pub name: String,
    pub root: PathBuf,
}

/// The binary Scoop's `current` junction starts.
const TRAY_BIN: &str = "ai-usagebar-tray";

/// Detect a Scoop install from the running exe. `None` for a plain zip.
pub fn detect() -> Option<ScoopApp> {
    let exe = std::env::current_exe().ok()?;
    detect_with(&exe, &|path| path.exists())
}

/// The exe must sit in `<root>/apps/<name>/<current-or-version>/` and the
/// root must have Scoop's `shims` directory. Both the junction path and the
/// resolved version path are accepted: `current_exe` reports either.
pub fn detect_with(exe: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<ScoopApp> {
    let version_dir = exe.parent()?;
    let app_dir = version_dir.parent()?;
    let apps_dir = app_dir.parent()?;
    if !apps_dir.file_name()?.eq_ignore_ascii_case("apps") {
        return None;
    }
    let root = apps_dir.parent()?;
    let name = app_dir.file_name()?.to_str()?;
    if name.is_empty() || !exists(&root.join("shims")) {
        return None;
    }
    Some(ScoopApp {
        name: name.to_owned(),
        root: root.to_path_buf(),
    })
}

/// A bucket or app name is joined onto a path, so it must be one plain
/// component.
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `"bucket"` of `install.json`, the file Scoop writes next to the app.
pub fn bucket_from_install_json(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let bucket = value.get("bucket")?.as_str()?.trim();
    is_plain_name(bucket).then(|| bucket.to_owned())
}

/// `"version"` of a bucket manifest. Anything but a strict `X.Y.Z` is not
/// ours: `is_newer` would refuse it anyway, so refusing it here keeps the
/// "no version" and "odd version" cases on the same path.
pub fn version_from_manifest(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let version = value.get("version")?.as_str()?.trim();
    let strict = version.split('.').count() == 3
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    strict.then(|| version.to_owned())
}

/// `"homepage"` of a bucket manifest when it is a GitHub page; the popover
/// keeps only those, so anything else yields `None` and the caller falls
/// back to the releases page.
pub fn homepage_from_manifest(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let homepage = value.get("homepage")?.as_str()?.trim();
    (homepage.starts_with("https://github.com/") && homepage.len() <= 400)
        .then(|| homepage.to_owned())
}

impl ScoopApp {
    fn app_dir(&self) -> PathBuf {
        self.root.join("apps").join(&self.name)
    }

    /// The bucket this app was installed from, per `current/install.json`.
    pub fn bucket(&self) -> Option<String> {
        let text =
            std::fs::read_to_string(self.app_dir().join("current").join("install.json")).ok()?;
        bucket_from_install_json(&text)
    }

    fn bucket_manifest_text(&self) -> Option<String> {
        let bucket = self.bucket()?;
        let path = self
            .root
            .join("buckets")
            .join(bucket)
            .join("bucket")
            .join(format!("{}.json", self.name));
        std::fs::read_to_string(path).ok()
    }

    /// The version the bucket currently offers.
    pub fn bucket_manifest_version(&self) -> Option<String> {
        version_from_manifest(&self.bucket_manifest_text()?)
    }

    /// The manifest's GitHub homepage, if it names one.
    pub fn bucket_manifest_homepage(&self) -> Option<String> {
        homepage_from_manifest(&self.bucket_manifest_text()?)
    }

    /// `<root>/apps/<name>/current/<bin>.exe`: the junction path, so the
    /// relaunch after an update starts whatever version is current then.
    pub fn current_exe_path(&self, bin: &str) -> PathBuf {
        self.app_dir().join("current").join(format!("{bin}.exe"))
    }

    /// One PowerShell program that waits for this tray to exit, runs
    /// `scoop update <name>` (which refuses while the exe is running), and
    /// starts the updated tray with the relaunch marker set so it waits for
    /// the single-instance mutex instead of quitting. Everything is
    /// transcribed to `log` for the user to read when it goes wrong, with the
    /// PowerShell and git in use named first.
    ///
    /// `scoop update <app>` also updates Scoop itself when it thinks it is
    /// stale, and a `git pull` that fails there aborts the whole update; a
    /// network blip at the wrong second must not strand the user on the old
    /// version, so the update is tried up to three times, five seconds apart.
    /// The relaunch happens either way: `current` still points at whatever
    /// version Scoop left in place.
    pub fn update_script(&self, tray_pid: u32, log: &Path) -> String {
        let exe = self.current_exe_path(TRAY_BIN);
        let dir = self.app_dir().join("current");
        let name = quote(&self.name);
        [
            format!(
                "Start-Transcript -Path '{}' -Force",
                quote(&log.display().to_string())
            ),
            "Write-Host (\"PowerShell $($PSVersionTable.PSVersion); git: \" + (Get-Command git -ErrorAction SilentlyContinue).Source)".to_owned(),
            format!("Wait-Process -Id {tray_pid} -ErrorAction SilentlyContinue"),
            "$updated = $false".to_owned(),
            format!(
                "for ($attempt = 1; $attempt -le 3 -and -not $updated; $attempt++) {{ Write-Host \"scoop update '{name}' (attempt $attempt)\"; scoop update '{name}' 2>&1 | Out-Host; if ($LASTEXITCODE -eq 0) {{ $updated = $true }} else {{ Start-Sleep -Seconds 5 }} }}"
            ),
            "Write-Host \"updated: $updated\"".to_owned(),
            format!("$env:{RELAUNCH_ENV} = '1'"),
            format!(
                "Start-Process -FilePath '{}' -WorkingDirectory '{}'",
                quote(&exe.display().to_string()),
                quote(&dir.display().to_string())
            ),
            "Stop-Transcript".to_owned(),
        ]
        .join("; ")
    }
}

/// Inside a single-quoted PowerShell string only the quote itself is
/// special, and it is doubled.
fn quote(text: &str) -> String {
    text.replace('\'', "''")
}

/// `scoop` is a PowerShell shim, so every Scoop call is a `powershell.exe`
/// invocation; the flags keep it quiet, non-blocking and policy-proof.
pub fn powershell_command(script: &str) -> Vec<String> {
    vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-Command".into(),
        script.to_owned(),
    ]
}

/// `scoop update` with no app: refresh Scoop and every bucket (a `git pull`
/// each), so the manifest the check reads is the newest one.
pub fn refresh_buckets_command() -> Vec<String> {
    powershell_command("scoop update")
}

/// Longest a bucket refresh may take before the check gives up on it.
pub const BUCKET_REFRESH_TIMEOUT: Duration = Duration::from_secs(90);

/// Run `argv` with no console window and no inherited handles, killing it
/// past `timeout`. Its output is not read: nothing in it is ours to show,
/// and a pipe nobody drains would block the child.
pub fn run_quietly(argv: &[String], timeout: Duration) -> Result<(), String> {
    let program = argv.first().cloned().unwrap_or_default();
    let mut command = quiet_command(argv)?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} exited with {status}")),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took longer than {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(format!("could not wait for {program}: {e}")),
        }
    }
}

/// Start `argv` detached (no window, no handles) and return at once; the
/// child outlives this process.
pub fn spawn_detached(argv: &[String]) -> Result<(), String> {
    let program = argv.first().cloned().unwrap_or_default();
    quiet_command(argv)?
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {program}: {e}"))
}

/// Environment variables a parent shell may leave behind that break the
/// Windows PowerShell Scoop runs in. `PSModulePath` is the one seen in the
/// wild: a tray started from PowerShell 7 inherits its module path, and
/// Windows PowerShell 5.1 then loads 7's `Microsoft.PowerShell.Utility`
/// and loses `Get-FileHash`, which Scoop needs to verify a download.
const SCRUBBED_ENV: &[&str] = &["PSModulePath"];

/// `argv` as a child with no console window, no inherited handles and a
/// clean PowerShell environment.
fn quiet_command(argv: &[String]) -> Result<Command, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "empty command".to_owned())?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in SCRUBBED_ENV {
        command.env_remove(name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::process::CREATE_NO_WINDOW);
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shims_exist(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "shims")
    }

    #[test]
    fn detects_the_junction_path() {
        let exe = Path::new("C:/Users/me/scoop/apps/ai-usagebar/current/ai-usagebar-tray.exe");
        let app = detect_with(exe, &shims_exist).expect("scoop install");
        assert_eq!(app.name, "ai-usagebar");
        assert_eq!(app.root, Path::new("C:/Users/me/scoop"));
    }

    #[test]
    fn detects_the_resolved_version_path() {
        let exe = Path::new("C:/Users/me/scoop/apps/ai-usagebar/1.14.0/ai-usagebar-tray.exe");
        let app = detect_with(exe, &shims_exist).expect("scoop install");
        assert_eq!(app.name, "ai-usagebar");
        assert_eq!(app.root, Path::new("C:/Users/me/scoop"));
    }

    #[test]
    fn a_tree_without_apps_is_not_scoop() {
        let exe = Path::new("C:/Tools/ai-usagebar/current/ai-usagebar-tray.exe");
        assert_eq!(detect_with(exe, &shims_exist), None);
        assert_eq!(
            detect_with(Path::new("ai-usagebar-tray.exe"), &shims_exist),
            None
        );
    }

    #[test]
    fn a_root_without_shims_is_not_scoop() {
        let exe = Path::new("C:/Users/me/scoop/apps/ai-usagebar/current/ai-usagebar-tray.exe");
        assert_eq!(detect_with(exe, &|_| false), None);
    }

    #[test]
    fn bucket_comes_from_install_json() {
        assert_eq!(
            bucket_from_install_json(
                r#"{"bucket":"djalmajr","architecture":"64bit","url":"https://x/y.zip"}"#
            ),
            Some("djalmajr".into())
        );
        assert_eq!(
            bucket_from_install_json(r#"{"architecture":"64bit"}"#),
            None
        );
        assert_eq!(bucket_from_install_json(r#"{"bucket":"../etc"}"#), None);
        assert_eq!(bucket_from_install_json(r#"{"bucket":""}"#), None);
        assert_eq!(bucket_from_install_json("not json"), None);
    }

    #[test]
    fn version_comes_from_the_manifest() {
        assert_eq!(
            version_from_manifest(r#"{"version":"1.14.0","homepage":"https://github.com/x/y"}"#),
            Some("1.14.0".into())
        );
        assert_eq!(version_from_manifest(r#"{"homepage":"x"}"#), None);
        assert_eq!(version_from_manifest(r#"{"version":"v1.14.0"}"#), None);
        assert_eq!(version_from_manifest(r#"{"version":"1.14"}"#), None);
        assert_eq!(version_from_manifest(r#"{"version":1}"#), None);
        assert_eq!(version_from_manifest("{"), None);
    }

    #[test]
    fn homepage_is_kept_only_when_it_is_github() {
        assert_eq!(
            homepage_from_manifest(r#"{"homepage":"https://github.com/akitaonrails/ai-usagebar"}"#),
            Some("https://github.com/akitaonrails/ai-usagebar".into())
        );
        assert_eq!(
            homepage_from_manifest(r#"{"homepage":"https://evil.example/"}"#),
            None
        );
        assert_eq!(homepage_from_manifest(r#"{"version":"1.0.0"}"#), None);
        assert_eq!(homepage_from_manifest("[]"), None);
    }

    #[test]
    fn current_exe_path_goes_through_the_junction() {
        let app = ScoopApp {
            name: "ai-usagebar".into(),
            root: PathBuf::from("C:/scoop"),
        };
        assert_eq!(
            app.current_exe_path("ai-usagebar-tray"),
            Path::new("C:/scoop")
                .join("apps")
                .join("ai-usagebar")
                .join("current")
                .join("ai-usagebar-tray.exe")
        );
    }

    #[test]
    fn update_script_waits_updates_and_relaunches() {
        let app = ScoopApp {
            name: "ai-usagebar".into(),
            root: PathBuf::from("C:/scoop"),
        };
        let log = Path::new("C:/cache/ai-usagebar/updates/scoop.log");
        let script = app.update_script(4242, log);
        let exe = app
            .current_exe_path("ai-usagebar-tray")
            .display()
            .to_string();
        assert!(script.contains("Wait-Process -Id 4242"), "{script}");
        assert!(
            script.contains("scoop update 'ai-usagebar' 2>&1"),
            "{script}"
        );
        assert!(script.contains("$attempt -le 3"), "{script}");
        assert!(script.contains("Start-Sleep -Seconds 5"), "{script}");
        assert!(script.contains("Get-Command git"), "{script}");
        assert!(script.contains(&format!("-FilePath '{exe}'")), "{script}");
        assert!(script.contains("$env:AIUB_TRAY_RELAUNCH = '1'"), "{script}");
        assert!(
            script.starts_with(
                "Start-Transcript -Path 'C:/cache/ai-usagebar/updates/scoop.log' -Force"
            ),
            "{script}"
        );
        assert!(script.ends_with("Stop-Transcript"), "{script}");
        // The relaunch marker is the one the host's single-instance wait reads.
        assert!(script.contains(RELAUNCH_ENV), "{script}");
    }

    #[test]
    fn update_script_doubles_single_quotes() {
        let app = ScoopApp {
            name: "ai-usagebar".into(),
            root: PathBuf::from("C:/Users/O'Brien/scoop"),
        };
        let script = app.update_script(1, Path::new("C:/Users/O'Brien/scoop.log"));
        assert!(script.contains("O''Brien/scoop.log'"), "{script}");
        let current = app.app_dir().join("current").display().to_string();
        assert!(
            script.contains(&format!("'{}'", quote(&current))),
            "{script}"
        );
        assert!(!script.contains("O'Brien"), "{script}");
    }

    #[test]
    fn quiet_command_scrubs_the_parent_shells_module_path() {
        let command = quiet_command(&[
            "powershell.exe".to_owned(),
            "-Command".to_owned(),
            "scoop update".to_owned(),
        ])
        .unwrap();
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(removed, vec!["PSModulePath"]);
        assert_eq!(command.get_program(), "powershell.exe");
        assert!(quiet_command(&[]).is_err());
    }

    #[test]
    fn refresh_buckets_is_a_quiet_powershell_call() {
        assert_eq!(
            refresh_buckets_command(),
            vec![
                "powershell.exe",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "scoop update",
            ]
        );
        assert_eq!(BUCKET_REFRESH_TIMEOUT, Duration::from_secs(90));
    }
}
