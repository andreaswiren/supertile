//! Installed-application discovery for the command palette.
//!
//! Scans the per-user and machine Start Menu trees for `.lnk` and `.url`
//! shortcuts. Shortcuts are launched *as shortcuts* via `ShellExecuteW` rather
//! than resolved to their targets first: resolving needs `IShellLink` (COM,
//! and slow enough to matter across a few thousand entries), and Explorer
//! already knows how to run a `.lnk` correctly — including Store apps, MSIX
//! aliases and shortcuts with arguments and working directories.
//!
//! The scan runs on a worker thread at startup so it never delays the first
//! palette open.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One launchable entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Display name — the shortcut's file stem.
    pub name: String,
    /// Full path to the `.lnk`/`.url`, passed to `ShellExecuteW`.
    pub path: PathBuf,
    /// Immediate parent folder, shown as a subtitle to disambiguate
    /// same-named entries from different vendors.
    pub group: String,
}

/// Directory recursion limit. Start Menu trees are shallow; anything deeper is
/// either a mistake or a symlink loop.
const MAX_DEPTH: usize = 6;
/// Upper bound on entries, so a pathological tree cannot exhaust memory.
const MAX_ENTRIES: usize = 5000;

/// Shortcut names that are never what the user meant to launch.
const NOISE: &[&str] = &[
    "uninstall",
    "uninstaller",
    "readme",
    "read me",
    "release notes",
    "changelog",
    "licence",
    "license",
    "documentation",
    "help",
    "website",
    "web site",
    "homepage",
    "support",
    "report a bug",
    "report bug",
];

fn is_noise(name: &str) -> bool {
    let lower = name.to_lowercase();
    NOISE
        .iter()
        .any(|n| lower == *n || lower.starts_with(&format!("{n} ")) || lower.contains(n))
}

/// The Start Menu roots to scan.
pub fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(programdata) = std::env::var("ProgramData") {
        roots.push(PathBuf::from(programdata).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    roots.retain(|p| p.is_dir());
    roots
}

/// Walk `root`, collecting shortcuts. Bounded in both depth and count.
fn walk(root: &Path, base: &Path, depth: usize, out: &mut Vec<AppEntry>) {
    if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_ENTRIES {
            return;
        }
        let path = entry.path();
        // `file_type` does not follow links, so a directory symlink loop cannot
        // trap the walk.
        let Ok(ft) = entry.file_type() else { continue };

        if ft.is_dir() {
            walk(&path, base, depth + 1, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "lnk" && ext != "url" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.trim().is_empty() || is_noise(stem) {
            continue;
        }

        let group = path
            .parent()
            .filter(|p| *p != base)
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("Start Menu")
            .to_string();

        out.push(AppEntry {
            name: stem.to_string(),
            path: path.clone(),
            group,
        });
    }
}

/// Scan every Start Menu root, de-duplicated and sorted.
///
/// Entries with the same display name collapse to one — a machine-wide and a
/// per-user shortcut to the same app are the same app to the user.
pub fn scan() -> Vec<AppEntry> {
    let mut out = Vec::new();
    for root in start_menu_roots() {
        walk(&root, &root, 0, &mut out);
    }
    dedupe(out)
}

fn dedupe(mut entries: Vec<AppEntry>) -> Vec<AppEntry> {
    entries.sort_by_key(|a| a.name.to_lowercase());
    entries.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
    entries
}

/// A shared, lazily-populated app list.
///
/// The palette reads this on the UI thread while the scan writes it from a
/// worker; the lock is held only for the swap, never across I/O.
#[derive(Clone, Default)]
pub struct AppIndex {
    inner: Arc<Mutex<Vec<AppEntry>>>,
    ready: Arc<std::sync::atomic::AtomicBool>,
}

impl AppIndex {
    pub fn new() -> AppIndex {
        AppIndex::default()
    }

    /// Kick off a background scan. Returns immediately.
    ///
    /// `on_done` runs on the worker thread once the list is in place — used to
    /// post a repaint to the UI thread.
    pub fn refresh_async<F>(&self, on_done: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let ready = Arc::clone(&self.ready);
        // A detached thread is right here: the scan is pure I/O with no
        // teardown obligations, and abandoning it at exit is harmless.
        std::thread::spawn(move || {
            let found = scan();
            if let Ok(mut guard) = inner.lock() {
                *guard = found;
            }
            ready.store(true, std::sync::atomic::Ordering::Release);
            on_done();
        });
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Snapshot the current list.
    pub fn entries(&self) -> Vec<AppEntry> {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    fn set(&self, v: Vec<AppEntry>) {
        *self.inner.lock().unwrap() = v;
        self.ready.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Launch a shortcut (or any path) through the shell.
///
/// Returns false if the shell refused. `ShellExecuteW` with the `open` verb is
/// what Explorer itself uses, so shortcut arguments, working directories and
/// Store-app activation all behave as the user expects.
pub fn launch(path: &Path) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Some(text) = path.to_str() else {
        return false;
    };
    let wide = crate::util::WideStr::new(text);
    let verb = crate::util::WideStr::new("open");

    // SAFETY: both strings stay alive across the call. ShellExecuteW returns a
    // pseudo-HINSTANCE; values <= 32 are documented error codes.
    let result = unsafe {
        ShellExecuteW(
            None,
            verb.as_pcwstr(),
            wide.as_pcwstr(),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    result.0 as usize > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> AppEntry {
        AppEntry {
            name: name.into(),
            path: PathBuf::from(format!("{name}.lnk")),
            group: "g".into(),
        }
    }

    #[test]
    fn noise_shortcuts_are_filtered() {
        for n in [
            "Uninstall",
            "uninstall Foo",
            "Readme",
            "Release Notes",
            "Documentation",
        ] {
            assert!(is_noise(n), "{n} should be filtered");
        }
    }

    #[test]
    fn real_application_names_survive_the_noise_filter() {
        for n in [
            "Visual Studio Code",
            "Firefox",
            "Notepad++",
            "7-Zip File Manager",
            "Steam",
        ] {
            assert!(!is_noise(n), "{n} was wrongly filtered");
        }
    }

    #[test]
    fn dedupe_collapses_case_insensitive_duplicates() {
        let out = dedupe(vec![entry("Firefox"), entry("firefox"), entry("Chrome")]);
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|e| e.name.eq_ignore_ascii_case("firefox")));
        assert!(out.iter().any(|e| e.name == "Chrome"));
    }

    #[test]
    fn dedupe_sorts_case_insensitively() {
        let out = dedupe(vec![entry("zebra"), entry("Apple"), entry("mango")]);
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Apple", "mango", "zebra"]);
    }

    #[test]
    fn an_empty_index_is_safe_to_read() {
        let idx = AppIndex::new();
        assert!(idx.is_empty());
        assert!(!idx.is_ready());
        assert_eq!(idx.entries().len(), 0);
    }

    #[test]
    fn the_index_is_shared_across_clones() {
        let a = AppIndex::new();
        let b = a.clone();
        a.set(vec![entry("Foo")]);
        assert_eq!(b.len(), 1);
        assert!(b.is_ready());
    }

    #[test]
    fn walking_a_temp_tree_finds_only_shortcuts() {
        let base = std::env::temp_dir().join(format!("supertile-applist-{}", std::process::id()));
        let sub = base.join("Vendor");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("Alpha.lnk"), b"").unwrap();
        std::fs::write(base.join("Notes.txt"), b"").unwrap();
        std::fs::write(base.join("Uninstall.lnk"), b"").unwrap();
        std::fs::write(sub.join("Beta.url"), b"").unwrap();
        std::fs::write(sub.join("Gamma.lnk"), b"").unwrap();

        let mut out = Vec::new();
        walk(&base, &base, 0, &mut out);
        let mut names: Vec<String> = out.iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["Alpha", "Beta", "Gamma"], "got {out:?}");

        // Nested entries carry their folder as the group; top-level ones do not.
        let beta = out.iter().find(|e| e.name == "Beta").unwrap();
        assert_eq!(beta.group, "Vendor");
        let alpha = out.iter().find(|e| e.name == "Alpha").unwrap();
        assert_eq!(alpha.group, "Start Menu");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn walking_respects_the_depth_limit() {
        let base = std::env::temp_dir().join(format!("supertile-depth-{}", std::process::id()));
        let mut p = base.clone();
        for i in 0..(MAX_DEPTH + 3) {
            p = p.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("TooDeep.lnk"), b"").unwrap();
        std::fs::write(base.join("Shallow.lnk"), b"").unwrap();

        let mut out = Vec::new();
        walk(&base, &base, 0, &mut out);
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"Shallow"));
        assert!(!names.contains(&"TooDeep"), "depth limit not enforced");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn walking_a_missing_directory_is_not_an_error() {
        let mut out = Vec::new();
        walk(
            Path::new(r"C:\definitely\not\here\at\all"),
            Path::new("x"),
            0,
            &mut out,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn scanning_the_live_start_menu_returns_plausible_entries() {
        let apps = scan();
        // A Windows install always has *something* in the Start Menu, but do
        // not hard-fail a minimal CI image.
        for a in &apps {
            assert!(!a.name.trim().is_empty());
            let ext = a
                .path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            assert!(ext == "lnk" || ext == "url", "unexpected {a:?}");
        }
        assert!(apps.len() <= MAX_ENTRIES);
        // Names must be unique after dedupe.
        let mut lower: Vec<String> = apps.iter().map(|a| a.name.to_lowercase()).collect();
        let n = lower.len();
        lower.sort();
        lower.dedup();
        assert_eq!(lower.len(), n, "scan returned duplicate names");
    }
}
