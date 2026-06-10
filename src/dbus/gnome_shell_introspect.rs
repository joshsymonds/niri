use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{SerializeDict, Type, Value};

use super::Start;

pub struct Introspect {
    to_niri: calloop::channel::Sender<IntrospectToNiri>,
    from_niri: async_channel::Receiver<NiriToIntrospect>,
}

pub enum IntrospectToNiri {
    GetWindows,
}

pub enum NiriToIntrospect {
    Windows(HashMap<u64, WindowProperties>),
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct WindowProperties {
    /// Window title.
    pub title: String,
    /// Window app ID.
    ///
    /// On the wire this is the name of the .desktop file (that's what
    /// xdg-desktop-portal-gnome resolves window icons from). Niri sends the raw Wayland app
    /// ID over the channel, and [`Introspect::get_windows`] resolves it to a desktop-file ID
    /// via [`DesktopIdIndex`] on the D-Bus thread, keeping the file scanning off the
    /// compositor thread.
    #[zvariant(rename = "app-id")]
    pub app_id: String,
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetWindows) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::Windows(mut windows)) => {
                let index = DesktopIdIndex::from_env();
                for props in windows.values_mut() {
                    props.app_id = index.resolve(&props.app_id);
                }
                Ok(windows)
            }
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    /// Emitted by `Niri::refresh_introspect_windows` when the window list changes.
    #[zbus(signal)]
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Introspect {
    pub fn new(
        to_niri: calloop::channel::Sender<IntrospectToNiri>,
        from_niri: async_channel::Receiver<NiriToIntrospect>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)
    }
}

/// Index of installed .desktop files for resolving Wayland app IDs to desktop-file IDs.
///
/// The Introspect `app-id` property is expected to be a desktop-file ID (that's what
/// xdg-desktop-portal-gnome resolves icons from), while Wayland app IDs are free-form.
/// This implements a small subset of GNOME Shell's window-to-app tracking: an index of
/// desktop-file IDs and their `StartupWMClass` keys, matched against the app ID in tiers.
///
/// Built by scanning XDG data dirs; cheap enough to rebuild per `GetWindows` call (which
/// happens at picker-open frequency), avoiding any file-watching infrastructure.
pub struct DesktopIdIndex {
    /// Entries in scan order; on duplicate IDs the earliest dir wins, per the desktop-entry
    /// spec's data-dir precedence.
    entries: Vec<DesktopIdEntry>,
}

struct DesktopIdEntry {
    /// Desktop-file ID, e.g. `org.kde.dolphin.desktop` (subdir separators become `-`).
    id: String,
    /// `id` without the `.desktop` suffix.
    stem: String,
    /// `StartupWMClass` from the `[Desktop Entry]` section, if any.
    wm_class: Option<String>,
}

impl DesktopIdIndex {
    /// Scans the standard XDG data dirs.
    pub fn from_env() -> Self {
        let mut dirs = Vec::new();

        let home = std::env::var_os("HOME").map(PathBuf::from);
        match std::env::var_os("XDG_DATA_HOME") {
            Some(dir) if !dir.is_empty() => dirs.push(PathBuf::from(dir)),
            _ => {
                if let Some(home) = home {
                    dirs.push(home.join(".local/share"));
                }
            }
        }

        match std::env::var_os("XDG_DATA_DIRS") {
            Some(data_dirs) if !data_dirs.is_empty() => {
                dirs.extend(std::env::split_paths(&data_dirs));
            }
            _ => {
                dirs.push(PathBuf::from("/usr/local/share"));
                dirs.push(PathBuf::from("/usr/share"));
            }
        }

        for dir in &mut dirs {
            dir.push("applications");
        }
        Self::scan_dirs(&dirs)
    }

    /// Scans the given `applications` directories, earliest dir taking precedence.
    pub fn scan_dirs(dirs: &[PathBuf]) -> Self {
        let _span = tracy_client::span!("DesktopIdIndex::scan_dirs");

        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        for dir in dirs {
            scan_dir(dir, String::new(), &mut entries, &mut seen);
        }
        Self { entries }
    }

    /// Resolves a Wayland app ID to a desktop-file ID.
    ///
    /// Matching tiers, mirroring (a simplified) GNOME Shell window tracker: exact desktop-file
    /// ID, exact `StartupWMClass`, case-insensitive ID, case-insensitive `StartupWMClass`.
    /// Unmatched app IDs fall back to `{app_id}.desktop`, which is correct for the common case
    /// of well-behaved apps whose ID matches their desktop file. An empty app ID stays empty.
    pub fn resolve(&self, app_id: &str) -> String {
        if app_id.is_empty() {
            return String::new();
        }

        let tiers: [&dyn Fn(&DesktopIdEntry) -> bool; 4] = [
            &|entry| entry.stem == app_id,
            &|entry| entry.wm_class.as_deref() == Some(app_id),
            &|entry| entry.stem.eq_ignore_ascii_case(app_id),
            &|entry| {
                entry
                    .wm_class
                    .as_deref()
                    .is_some_and(|wm_class| wm_class.eq_ignore_ascii_case(app_id))
            },
        ];
        for tier in tiers {
            if let Some(entry) = self.entries.iter().find(|entry| tier(entry)) {
                return entry.id.clone();
            }
        }

        format!("{app_id}.desktop")
    }
}

fn scan_dir(
    dir: &Path,
    id_prefix: String,
    entries: &mut Vec<DesktopIdEntry>,
    seen: &mut HashSet<String>,
) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };

    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if path.is_dir() {
            scan_dir(&path, format!("{id_prefix}{name}-"), entries, seen);
        } else if let Some(stem) = name.strip_suffix(".desktop") {
            let id = format!("{id_prefix}{name}");
            if !seen.insert(id.clone()) {
                continue;
            }
            entries.push(DesktopIdEntry {
                stem: format!("{id_prefix}{stem}"),
                wm_class: read_startup_wm_class(&path),
                id,
            });
        }
    }
}

/// Reads `StartupWMClass` from the `[Desktop Entry]` section of a desktop file.
fn read_startup_wm_class(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;

    let mut in_desktop_entry = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop_entry = line == "[Desktop Entry]";
        } else if in_desktop_entry {
            if let Some(value) = line.strip_prefix("StartupWMClass=") {
                return Some(value.trim().to_owned());
            }
        }
    }
    None
}

/// Order-independent signature of the window list as exposed over Introspect.
///
/// XOR-folds a per-window hash of (id, title, app-id), so reordering windows yields the same
/// signature while any open/close/retitle/re-app-id changes it. Used to decide when to emit
/// [`Introspect::windows_changed`].
pub fn windows_signature<'a>(windows: impl Iterator<Item = (u64, &'a str, &'a str)>) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};

    windows.fold(0, |acc, (id, title, app_id)| {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        title.hash(&mut hasher);
        app_id.hash(&mut hasher);
        acc ^ hasher.finish()
    })
}

#[cfg(test)]
mod desktop_id_tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    struct TempDirs {
        root: PathBuf,
        dirs: Vec<PathBuf>,
    }

    impl TempDirs {
        fn new(name: &str, count: usize) -> Self {
            let root = std::env::temp_dir().join(format!(
                "niri-desktop-id-test-{}-{}",
                name,
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let dirs = (0..count)
                .map(|i| {
                    let dir = root.join(format!("data{i}/applications"));
                    fs::create_dir_all(&dir).unwrap();
                    dir
                })
                .collect();
            Self { root, dirs }
        }

        fn write(&self, dir: usize, rel_path: &str, wm_class: Option<&str>) {
            let path = self.dirs[dir].join(rel_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut contents = String::from("[Desktop Entry]\nType=Application\n");
            if let Some(wm_class) = wm_class {
                contents.push_str(&format!("StartupWMClass={wm_class}\n"));
            }
            contents.push_str("[Desktop Action other]\nStartupWMClass=decoy\n");
            fs::write(path, contents).unwrap();
        }

        fn index(&self) -> DesktopIdIndex {
            DesktopIdIndex::scan_dirs(&self.dirs)
        }
    }

    impl Drop for TempDirs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn exact_file_id_wins() {
        let tmp = TempDirs::new("exact", 1);
        tmp.write(0, "firefox.desktop", None);
        assert_eq!(tmp.index().resolve("firefox"), "firefox.desktop");
    }

    #[test]
    fn exact_wm_class_beats_case_insensitive_stem() {
        let tmp = TempDirs::new("wmclass", 1);
        // Case-insensitive stem candidate, listed first.
        tmp.write(0, "aoom.desktop", None);
        tmp.write(0, "ZOOM.desktop", None);
        // Exact StartupWMClass match must win over it.
        tmp.write(0, "Zoom.desktop", Some("zoom"));
        assert_eq!(tmp.index().resolve("zoom"), "Zoom.desktop");
    }

    #[test]
    fn case_insensitive_stem_beats_case_insensitive_wm_class() {
        let tmp = TempDirs::new("ci-stem", 1);
        tmp.write(0, "other.desktop", Some("SLACK"));
        tmp.write(0, "Slack.desktop", None);
        assert_eq!(tmp.index().resolve("slack"), "Slack.desktop");
    }

    #[test]
    fn case_insensitive_wm_class_matches() {
        let tmp = TempDirs::new("ci-wmclass", 1);
        tmp.write(0, "other.desktop", Some("TeAmS"));
        assert_eq!(tmp.index().resolve("teams"), "other.desktop");
    }

    #[test]
    fn unmatched_falls_back_to_appending_desktop() {
        let tmp = TempDirs::new("fallback", 1);
        tmp.write(0, "firefox.desktop", None);
        assert_eq!(tmp.index().resolve("rs.bxt.niri"), "rs.bxt.niri.desktop");
    }

    #[test]
    fn empty_app_id_stays_empty() {
        let tmp = TempDirs::new("empty", 1);
        tmp.write(0, "firefox.desktop", None);
        assert_eq!(tmp.index().resolve(""), "");
    }

    #[test]
    fn subdirectory_files_get_dash_separated_ids() {
        let tmp = TempDirs::new("subdir", 1);
        tmp.write(0, "kde/org.kde.dolphin.desktop", None);
        assert_eq!(
            tmp.index().resolve("kde-org.kde.dolphin"),
            "kde-org.kde.dolphin.desktop"
        );
    }

    #[test]
    fn earlier_dir_wins_on_id_collision() {
        let tmp = TempDirs::new("collision", 2);
        tmp.write(0, "app.desktop", Some("collide1"));
        tmp.write(1, "app.desktop", Some("collide2"));
        let index = tmp.index();
        // dir0's entry shadows dir1's entirely.
        assert_eq!(index.resolve("collide1"), "app.desktop");
        assert_eq!(index.resolve("collide2"), "collide2.desktop");
    }

    #[test]
    fn wm_class_outside_desktop_entry_section_is_ignored() {
        let tmp = TempDirs::new("section", 1);
        // `write` always appends a decoy [Desktop Action] section with
        // StartupWMClass=decoy; it must not be picked up.
        tmp.write(0, "app.desktop", None);
        assert_eq!(tmp.index().resolve("decoy"), "decoy.desktop");
    }

    #[test]
    fn non_desktop_files_are_skipped() {
        let tmp = TempDirs::new("nondesktop", 1);
        fs::write(tmp.dirs[0].join("README.md"), "not a desktop file").unwrap();
        tmp.write(0, "app.desktop", None);
        assert_eq!(tmp.index().resolve("README"), "README.desktop");
    }
}

#[cfg(test)]
mod signature_tests {
    use super::*;

    const A: (u64, &str, &str) = (1, "Terminal — ~", "kitty");
    const B: (u64, &str, &str) = (2, "Meeting", "Zoom");
    const C: (u64, &str, &str) = (3, "inbox", "thunderbird");

    #[test]
    fn order_independent() {
        let fwd = windows_signature([A, B, C].into_iter());
        let rev = windows_signature([C, B, A].into_iter());
        assert_eq!(fwd, rev);
    }

    #[test]
    fn add_remove_retitle_change_signature() {
        let base = windows_signature([A, B].into_iter());

        let added = windows_signature([A, B, C].into_iter());
        assert_ne!(base, added);

        let removed = windows_signature([A].into_iter());
        assert_ne!(base, removed);

        let retitled = windows_signature([A, (2, "Meeting — sharing", "Zoom")].into_iter());
        assert_ne!(base, retitled);

        let new_app_id = windows_signature([A, (2, "Meeting", "zoom")].into_iter());
        assert_ne!(base, new_app_id);
    }

    #[test]
    fn empty_is_stable() {
        let a = windows_signature(std::iter::empty());
        let b = windows_signature(std::iter::empty());
        assert_eq!(a, b);
    }
}
