//! Resolves an absolute path to a native CLI binary (`claude`, `codex`)
//! before spawning it, rather than trusting `Command::new("claude")`'s
//! bare-name PATH search to already work (`agent-core-and-task-router`
//! Group 10).
//!
//! A bare-name spawn only finds the binary if it happens to sit on
//! whatever `PATH` this process inherited -- true when Aemeath is
//! launched from a terminal, false in the cases that motivated this
//! module:
//! - **macOS**: launched from Finder/Dock/Spotlight, the process is spawned by
//!   `launchd`, whose own `PATH` is just `/usr/bin:/bin:/usr/sbin:/sbin` (plus
//!   whatever `/etc/paths.d/*` adds, which is little by default) -- Homebrew
//!   (`/opt/homebrew/bin` or `/usr/local/bin`), nvm/volta/fnm, and a
//!   `~/.local/bin`-style installer script are all added by
//!   `.zshrc`/`.zprofile`, which `launchd` never sources.
//! - **Linux**: launched from a `.desktop` file, whether the desktop
//!   environment's session sourced `~/.profile` (and so picked up
//!   `~/.local/bin`, nvm, Nix's profile scripts, ...) before spawning it varies
//!   by DE and isn't something to rely on.
//! - **Windows**: normally fine -- `PATH` is a persistent `HKCU\Environment`
//!   registry value, and Explorer rebuilds each new process's environment block
//!   from it. The one real failure mode is a *stale* one: if Explorer was
//!   already running when an installer added itself to `PATH`, Explorer won't
//!   pick that up until it restarts (the well-known "log off/on to see a new
//!   PATH entry" issue).
//!
//! Search order, first hit wins, memoized per binary name for the
//! process lifetime (a login-shell spawn below is not cheap enough to
//! repeat on every `check_logged_in()` call, which both CLI providers
//! make before every chat turn):
//! 1. This process's own `PATH` (already correct for a terminal launch) -- on
//!    Windows, merged with a fresh read of `HKCU\Environment\Path`/the
//!    machine-wide `...\Environment\Path` straight from the registry,
//!    sidestepping the stale-Explorer- session problem rather than trusting the
//!    inherited value.
//! 2. A small curated list of common per-OS install locations (cheap,
//!    synchronous, no subprocess). Deliberately does **not** try to guess a
//!    NixOS `/nix/store/<hash>-.../bin` path -- there isn't a stable one to
//!    guess. `~/.nix-profile/bin` and `/run/current-system/sw/bin` are included
//!    instead, since those are the profile-symlink locations Nix itself keeps
//!    stable regardless of the package's store hash.
//! 3. Unix only: the user's own login shell's resolved `PATH`, captured once by
//!    spawning `$SHELL -ilc` and reading it back. This is the one mechanism
//!    that generalizes across Homebrew, nvm/volta/fnm, *and* Nix without
//!    hardcoding an install-manager-specific path table -- it just asks the
//!    shell that already knows the right answer. Not attempted on Windows,
//!    where step 1's registry re-read is the correct fix for the equivalent
//!    problem.
//!
//! If nothing is found, `resolve()` returns `name` unchanged so the
//! caller's own `Command::new(...)` still attempts (and fails in the
//! same familiar way as before this module existed --
//! `cli_process::map_spawn_error`'s "not installed or not on PATH"
//! message stays accurate) its own PATH search.

#[cfg(unix)]
use std::time::Duration;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

fn cache() -> &'static Mutex<HashMap<String, PathBuf>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Test seam: makes `resolve(name)` return `path`, so a provider can be
/// driven against a fake CLI script. It edits the process-wide cache, so
/// it is for the fake-CLI tests only; the returned guard restores normal
/// resolution when dropped (including on a panic). Those tests are unit
/// tests, and the live tests that need the real CLI are `#[ignore]`d --
/// running both kinds in one process (`--include-ignored`) is the one
/// combination this seam does not support.
#[cfg(all(test, unix))]
pub(crate) fn override_for_test(name: &'static str, path: PathBuf) -> OverrideGuard {
    cache().lock().unwrap().insert(name.to_string(), path);
    OverrideGuard(name)
}

#[cfg(all(test, unix))]
pub(crate) struct OverrideGuard(&'static str);

#[cfg(all(test, unix))]
impl Drop for OverrideGuard {
    fn drop(&mut self) { cache().lock().unwrap().remove(self.0); }
}

/// Resolves `name` (e.g. `"claude"`, `"codex"`) to an absolute
/// executable path if one can be found by any step in the module doc's
/// search order; otherwise returns `name` itself unchanged.
pub(crate) async fn resolve(name: &str) -> PathBuf {
    if let Some(cached) = cache().lock().unwrap().get(name) {
        return cached.clone();
    }
    let resolved = resolve_uncached(name).await.unwrap_or_else(|| PathBuf::from(name));
    cache().lock().unwrap().insert(name.to_string(), resolved.clone());
    resolved
}

async fn resolve_uncached(name: &str) -> Option<PathBuf> {
    let extensions = executable_extensions();

    #[allow(unused_mut)]
    let mut search_dirs = process_path_dirs();
    #[cfg(target_os = "windows")]
    search_dirs.extend(windows_registry_path_dirs());
    if let Some(found) = find_in_dirs(&search_dirs, name, extensions) {
        return Some(found);
    }

    if let Some(found) = find_in_dirs(&fallback_candidate_dirs(), name, extensions) {
        return Some(found);
    }

    #[cfg(unix)]
    {
        if let Some(path_value) = login_shell_path().await {
            let dirs: Vec<PathBuf> = std::env::split_paths(&path_value).collect();
            if let Some(found) = find_in_dirs(&dirs, name, extensions) {
                return Some(found);
            }
        }
    }

    None
}

fn process_path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

#[cfg(target_os = "windows")]
fn executable_extensions() -> &'static [&'static str] { &["exe", "cmd", "bat", "ps1"] }
#[cfg(not(target_os = "windows"))]
fn executable_extensions() -> &'static [&'static str] { &[] }

fn find_in_dirs(dirs: &[PathBuf], name: &str, extensions: &[&str]) -> Option<PathBuf> {
    for dir in dirs {
        let bare = dir.join(name);
        if is_executable_file(&bare) {
            return Some(bare);
        }
        for ext in extensions {
            let candidate = dir.join(format!("{name}.{ext}"));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool { std::fs::metadata(path).is_ok_and(|m| m.is_file()) }

// `std::env::home_dir()` was deprecated for years over a Windows-only
// bug (silently ignoring `%USERPROFILE%` in some cases) fixed and
// un-deprecated in Rust 1.85 -- safe to use directly on this project's
// toolchain (`rustc 1.97.1`).
#[cfg(target_os = "macos")]
fn fallback_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".local/bin"));
    }
    dirs
}

#[cfg(all(unix, not(target_os = "macos")))]
fn fallback_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs =
        vec![PathBuf::from("/usr/local/bin"), PathBuf::from("/run/current-system/sw/bin")];
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join(".local/bin"));
        // NixOS/home-manager profile symlinks -- stable regardless of
        // the package's content-hashed `/nix/store/...` path, which is
        // why this list can support Nix at all despite not being able
        // to guess a store path (see module doc, step 2).
        dirs.push(home.join(".nix-profile/bin"));
    }
    if let Ok(user) = std::env::var("USER") {
        dirs.push(PathBuf::from(format!("/etc/profiles/per-user/{user}/bin")));
    }
    dirs
}

#[cfg(target_os = "windows")]
fn fallback_candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(home) = std::env::home_dir() {
        dirs.push(home.join("scoop").join("shims"));
    }
    dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    dirs
}

/// Reads `PATH` fresh from the registry rather than trusting this
/// process's (possibly stale, see module doc) inherited value.
/// `HKLM`'s machine-wide value can be `REG_EXPAND_SZ` (containing
/// `%SystemRoot%`-style references); those aren't expanded here, so an
/// unexpanded segment simply won't match any real directory and is
/// harmlessly skipped by `find_in_dirs` -- full expansion isn't worth
/// the complexity when the only consequence of skipping it is missing
/// a system-installed (not third-party) tool location.
#[cfg(target_os = "windows")]
fn windows_registry_path_dirs() -> Vec<PathBuf> {
    use winreg::{
        enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE},
        RegKey,
    };

    let read = |hive, subkey: &str| -> Option<String> {
        RegKey::predef(hive).open_subkey(subkey).ok()?.get_value::<String, _>("Path").ok()
    };

    let mut dirs = Vec::new();
    if let Some(user_path) = read(HKEY_CURRENT_USER, "Environment") {
        dirs.extend(std::env::split_paths(&user_path));
    }
    if let Some(machine_path) =
        read(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment")
    {
        dirs.extend(std::env::split_paths(&machine_path));
    }
    dirs
}

#[cfg(unix)]
const PATH_SENTINEL_START: &str = "__fleet_snowfluff_path_start__";
#[cfg(unix)]
const PATH_SENTINEL_END: &str = "__fleet_snowfluff_path_end__";

/// Extracts the text between the *last* occurrence of `start`/`end`,
/// pulled out as its own pure function so the sentinel-parsing logic is
/// directly testable without spawning a real shell. `rfind` (not
/// `find`) matters here: an interactive login shell's rc files
/// (oh-my-zsh greetings, `cowsay`, a stray `echo` in `.zshrc`, ...) can
/// print arbitrary text to stdout before our own command runs, and
/// taking the last pair skips over all of it rather than mis-parsing
/// banner text that happens to contain the sentinel-like substrings.
#[cfg(unix)]
fn extract_between_sentinels(output: &str, start: &str, end: &str) -> Option<String> {
    let end_idx = output.rfind(end)?;
    let start_idx = output[..end_idx].rfind(start)? + start.len();
    Some(output[start_idx..end_idx].to_string())
}

/// Spawns the user's login shell to ask for its actually-resolved
/// `PATH` (`-ilc`: interactive + login, so `.zprofile`/`.zshrc`/
/// `.bash_profile`/Nix's `/etc/profile.d/nix.sh` etc. all get sourced
/// the same way they would in a real terminal session). Bounded by a
/// timeout since an interactive shell's rc files are arbitrary code
/// that could hang or make a slow network call -- if it doesn't answer
/// promptly, resolution falls through to "not found" rather than
/// blocking a chat turn indefinitely.
#[cfg(unix)]
async fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).or_else(|| {
        ["/bin/zsh", "/bin/bash", "/bin/sh"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .map(String::from)
    })?;

    let script = format!(
        "printf '%s' '{PATH_SENTINEL_START}'; printf '%s' \"$PATH\"; printf '%s' \
         '{PATH_SENTINEL_END}'"
    );

    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&shell).args(["-ilc", &script]).output(),
    )
    .await
    .ok()?
    .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    extract_between_sentinels(&stdout, PATH_SENTINEL_START, PATH_SENTINEL_END)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_executable(path: &Path) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn find_in_dirs_finds_an_executable_bare_named_file_in_a_later_directory() {
        let dir = tempdir();
        let empty_dir = dir.path().join("empty");
        std::fs::create_dir(&empty_dir).unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        make_executable(&bin_dir.join("claude"));

        let found = find_in_dirs(&[empty_dir, bin_dir.clone()], "claude", &[]);
        assert_eq!(found, Some(bin_dir.join("claude")));
    }

    #[test]
    fn find_in_dirs_skips_a_same_named_file_that_is_not_executable() {
        let dir = tempdir();
        std::fs::write(dir.path().join("claude"), "not executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("claude"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }

        #[cfg(unix)]
        assert_eq!(find_in_dirs(&[dir.path().to_path_buf()], "claude", &[]), None);
    }

    #[test]
    fn find_in_dirs_returns_none_when_no_directory_has_a_match() {
        let dir = tempdir();
        assert_eq!(find_in_dirs(&[dir.path().to_path_buf()], "claude", &[]), None);
    }

    #[test]
    fn find_in_dirs_tries_each_extension_in_order() {
        let dir = tempdir();
        make_executable(&dir.path().join("claude.cmd"));

        let found = find_in_dirs(&[dir.path().to_path_buf()], "claude", &["exe", "cmd", "bat"]);
        assert_eq!(found, Some(dir.path().join("claude.cmd")));
    }

    #[cfg(unix)]
    #[test]
    fn extract_between_sentinels_finds_the_marked_value() {
        let output = format!("{PATH_SENTINEL_START}/usr/bin:/opt/homebrew/bin{PATH_SENTINEL_END}");
        assert_eq!(
            extract_between_sentinels(&output, PATH_SENTINEL_START, PATH_SENTINEL_END),
            Some("/usr/bin:/opt/homebrew/bin".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_between_sentinels_skips_banner_text_before_the_real_pair() {
        // A stray `echo` in a shell rc file could plausibly print
        // something resembling a sentinel before our own command's
        // output -- `rfind` must land on the real, final pair.
        let output = format!(
            "welcome! {PATH_SENTINEL_START} not real {PATH_SENTINEL_END} \
             noise\n{PATH_SENTINEL_START}/real/path{PATH_SENTINEL_END}"
        );
        assert_eq!(
            extract_between_sentinels(&output, PATH_SENTINEL_START, PATH_SENTINEL_END),
            Some("/real/path".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_between_sentinels_returns_none_when_a_sentinel_is_missing() {
        assert_eq!(extract_between_sentinels("no markers here", "START", "END"), None);
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path { &self.0 }
    }
    impl Drop for TempDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn tempdir() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "fleet-snowfluff-cli-locator-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}
