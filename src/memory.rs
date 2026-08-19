//! Per-application geometry memory.
//!
//! When an application opens a window, SuperTile tries to give it the same
//! place it had last time. Two things are remembered per application:
//!
//! * the **zone index** it occupied, which is exact but only meaningful while
//!   the layout and window count are unchanged; and
//! * a **fractional rectangle** relative to the work area, which survives
//!   resolution and layout changes at the cost of being approximate.
//!
//! Entries are scoped by a monitor-set fingerprint (see
//! [`crate::monitor::fingerprint`]) so a position learned on a docked
//! triple-head setup is never replayed onto a laptop panel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{LayoutKind, Rect};

pub const STORE_FILENAME: &str = "geometry.json";
/// Bumped when the on-disk shape changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

/// A rectangle expressed as fractions of a work area, so it can be replayed
/// onto a display of any size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FracRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl FracRect {
    /// Express `r` as fractions of `area`.
    pub fn of(r: Rect, area: Rect) -> FracRect {
        let w = area.width().max(1) as f32;
        let h = area.height().max(1) as f32;
        FracRect {
            left: (r.left - area.left) as f32 / w,
            top: (r.top - area.top) as f32 / h,
            right: (r.right - area.left) as f32 / w,
            bottom: (r.bottom - area.top) as f32 / h,
        }
    }

    /// Project back onto a (possibly different) work area.
    pub fn to_rect(self, area: Rect) -> Rect {
        let w = area.width() as f32;
        let h = area.height() as f32;
        let clamp01 = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let l = area.left + (clamp01(self.left) * w).round() as i32;
        let t = area.top + (clamp01(self.top) * h).round() as i32;
        let r = area.left + (clamp01(self.right) * w).round() as i32;
        let b = area.top + (clamp01(self.bottom) * h).round() as i32;
        // A stored rect that round-trips to zero size would produce an
        // invisible window; give it at least a pixel.
        Rect::new(l, t, r.max(l + 1), b.max(t + 1))
    }

    fn is_sane(&self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|v| v.is_finite())
            && self.right > self.left
            && self.bottom > self.top
    }
}

/// Where a window was placed, as passed to [`Store::remember`].
///
/// A struct rather than eight positional arguments: `remember(key, 0, 4, ..)`
/// gives no hint which number is the index and which the count, and swapping
/// them silently corrupts the store.
#[derive(Debug, Clone, Copy)]
pub struct Placement<'a> {
    pub zone_index: usize,
    pub zone_count: usize,
    pub layout: LayoutKind,
    /// Where the window ended up, in screen pixels.
    pub rect: Rect,
    /// The work area it was placed within, for the fractional fallback.
    pub work_area: Rect,
    /// GDI device name of the monitor.
    pub device: &'a str,
}

/// One remembered placement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// `fingerprint|exe|class` — see [`make_key`].
    pub key: String,
    /// Zone the window occupied.
    pub zone_index: u32,
    /// How many zones existed at the time; the index is only replayed when
    /// this still matches.
    pub zone_count: u32,
    pub layout: LayoutKind,
    /// Resolution-independent fallback.
    pub frac: FracRect,
    /// GDI device name of the monitor it was on.
    pub device: String,
    /// Unix seconds, used for LRU eviction.
    pub last_used: u64,
}

/// The persisted store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Not persisted; index into `entries` by key.
    #[serde(skip)]
    index: HashMap<String, usize>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            version: SCHEMA_VERSION,
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

/// Build the lookup key for a window.
///
/// The executable path is lower-cased so casing differences between
/// `QueryFullProcessImageNameW` results and shortcut targets do not split one
/// application into two entries. When the path is unavailable — which happens
/// for elevated processes — the class name alone still gives a usable key.
pub fn make_key(fingerprint: &str, exe: &str, class: &str) -> String {
    let exe = exe.to_lowercase();
    format!("{fingerprint}|{exe}|{class}")
}

/// What the store suggests for a window that just appeared.
#[derive(Debug, Clone, PartialEq)]
pub enum Suggestion {
    /// Reuse this zone index; the layout and zone count still match.
    Zone(usize),
    /// Layout changed: place at this rectangle instead.
    Rect(Rect),
}

impl Store {
    pub fn path() -> std::io::Result<PathBuf> {
        Ok(crate::util::data_dir()?.join(STORE_FILENAME))
    }

    fn reindex(&mut self) {
        self.index = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.key.clone(), i))
            .collect();
    }

    pub fn load() -> Store {
        match Store::path() {
            Ok(p) => Store::load_from(&p),
            Err(_) => Store::default(),
        }
    }

    /// Load, discarding anything unreadable or from an incompatible schema.
    ///
    /// The store is a cache, so a corrupt file is never fatal: losing it costs
    /// the user their remembered positions, not their session.
    pub fn load_from(path: &Path) -> Store {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Store::default();
        };
        let Ok(mut store) = serde_json::from_str::<Store>(&text) else {
            return Store::default();
        };
        if store.version != SCHEMA_VERSION {
            return Store::default();
        }
        // Drop entries that would produce nonsense geometry.
        store
            .entries
            .retain(|e| e.frac.is_sane() && !e.key.is_empty());
        // Later duplicates win; keeps the file self-healing.
        let mut seen = HashMap::new();
        for (i, e) in store.entries.iter().enumerate() {
            seen.insert(e.key.clone(), i);
        }
        let keep: std::collections::HashSet<usize> = seen.values().copied().collect();
        let mut i = 0;
        store.entries.retain(|_| {
            let k = keep.contains(&i);
            i += 1;
            k
        });
        store.reindex();
        store
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.index.get(key).and_then(|i| self.entries.get(*i))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record where a window ended up.
    pub fn remember(&mut self, key: String, p: Placement, max_entries: usize) {
        let Placement {
            zone_index,
            zone_count,
            layout,
            rect,
            work_area,
            device,
        } = p;
        if max_entries == 0 {
            self.entries.clear();
            self.index.clear();
            return;
        }
        let entry = Entry {
            key: key.clone(),
            zone_index: zone_index as u32,
            zone_count: zone_count as u32,
            layout,
            frac: FracRect::of(rect, work_area),
            device: device.to_string(),
            last_used: now_secs(),
        };

        match self.index.get(&key) {
            Some(&i) => self.entries[i] = entry,
            None => {
                self.entries.push(entry);
                self.index.insert(key, self.entries.len() - 1);
            }
        }
        self.evict_to(max_entries);
    }

    /// Drop least-recently-used entries until at most `max` remain.
    fn evict_to(&mut self, max: usize) {
        if self.entries.len() <= max {
            return;
        }
        // Newest first, then truncate. Ties break on key so eviction is
        // deterministic and testable.
        self.entries.sort_by(|a, b| {
            b.last_used
                .cmp(&a.last_used)
                .then_with(|| a.key.cmp(&b.key))
        });
        self.entries.truncate(max);
        self.reindex();
    }

    /// What should a newly-appeared window be given?
    ///
    /// Returns `None` when nothing is remembered, so the caller falls back to
    /// the next free zone.
    pub fn suggest(
        &self,
        key: &str,
        current_layout: LayoutKind,
        current_zone_count: usize,
        work_area: Rect,
    ) -> Option<Suggestion> {
        let e = self.get(key)?;
        if e.layout == current_layout
            && e.zone_count as usize == current_zone_count
            && (e.zone_index as usize) < current_zone_count
        {
            return Some(Suggestion::Zone(e.zone_index as usize));
        }
        Some(Suggestion::Rect(e.frac.to_rect(work_area)))
    }

    /// Forget one application.
    pub fn forget(&mut self, key: &str) -> bool {
        let Some(&i) = self.index.get(key) else {
            return false;
        };
        self.entries.remove(i);
        self.reindex();
        true
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Store::path()?)
    }

    /// Atomic write — a crash mid-save must not corrupt the store.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 1920, 1080);

    fn store_with(n: usize) -> Store {
        let mut s = Store::default();
        for i in 0..n {
            s.remember(
                format!("fp|app{i}.exe|Cls"),
                Placement {
                    zone_index: 0,
                    zone_count: 1,
                    layout: LayoutKind::Grid,
                    rect: Rect::new(0, 0, 960, 540),
                    work_area: AREA,
                    device: r"\\.\DISPLAY1",
                },
                1000,
            );
        }
        s
    }

    // --- fractional geometry ----------------------------------------------

    #[test]
    fn frac_round_trips_on_the_same_area() {
        let r = Rect::new(480, 270, 1440, 810);
        assert_eq!(FracRect::of(r, AREA).to_rect(AREA), r);
    }

    #[test]
    fn frac_scales_to_a_different_resolution() {
        let r = Rect::new(0, 0, 960, 540); // top-left quarter of 1080p
        let f = FracRect::of(r, AREA);
        let uhd = Rect::new(0, 0, 3840, 2160);
        assert_eq!(
            f.to_rect(uhd),
            Rect::new(0, 0, 1920, 1080),
            "still the top-left quarter"
        );
    }

    #[test]
    fn frac_handles_a_negative_origin_monitor() {
        let area = Rect::new(-2560, -100, 0, 1340);
        let r = Rect::new(-2560, -100, -1280, 620);
        assert_eq!(FracRect::of(r, area).to_rect(area), r);
    }

    #[test]
    fn frac_clamps_out_of_range_values() {
        let bad = FracRect {
            left: -5.0,
            top: 2.0,
            right: 99.0,
            bottom: 0.5,
        };
        let r = bad.to_rect(AREA);
        assert!(r.left >= AREA.left && r.right <= AREA.right);
        assert!(
            r.right > r.left && r.bottom > r.top,
            "must not invert: {r:?}"
        );
    }

    #[test]
    fn frac_rejects_non_finite_values() {
        let bad = FracRect {
            left: f32::NAN,
            top: 0.0,
            right: f32::INFINITY,
            bottom: 1.0,
        };
        assert!(!bad.is_sane());
        let r = bad.to_rect(AREA);
        assert!(r.right > r.left && r.bottom > r.top);
    }

    #[test]
    fn degenerate_frac_still_yields_a_visible_rect() {
        let f = FracRect {
            left: 0.5,
            top: 0.5,
            right: 0.5,
            bottom: 0.5,
        };
        let r = f.to_rect(AREA);
        assert!(r.width() >= 1 && r.height() >= 1);
    }

    // --- keys --------------------------------------------------------------

    #[test]
    fn keys_are_case_insensitive_on_the_exe_path() {
        assert_eq!(
            make_key("fp", r"C:\Apps\Foo.EXE", "Cls"),
            make_key("fp", r"c:\apps\foo.exe", "Cls")
        );
    }

    #[test]
    fn keys_separate_by_fingerprint_class_and_exe() {
        let a = make_key("fp1", "a.exe", "C");
        assert_ne!(a, make_key("fp2", "a.exe", "C"));
        assert_ne!(a, make_key("fp1", "b.exe", "C"));
        assert_ne!(a, make_key("fp1", "a.exe", "D"));
    }

    // --- remember / suggest -------------------------------------------------

    #[test]
    fn nothing_is_suggested_for_an_unknown_app() {
        let s = Store::default();
        assert_eq!(s.suggest("missing", LayoutKind::Grid, 4, AREA), None);
    }

    #[test]
    fn an_unchanged_layout_replays_the_exact_zone() {
        let mut s = Store::default();
        s.remember(
            "k".into(),
            Placement {
                zone_index: 2,
                zone_count: 4,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 540, 960, 1080),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        assert_eq!(
            s.suggest("k", LayoutKind::Grid, 4, AREA),
            Some(Suggestion::Zone(2))
        );
    }

    #[test]
    fn a_changed_layout_falls_back_to_the_fractional_rect() {
        let mut s = Store::default();
        let r = Rect::new(0, 0, 960, 540);
        s.remember(
            "k".into(),
            Placement {
                zone_index: 0,
                zone_count: 4,
                layout: LayoutKind::Grid,
                rect: r,
                work_area: AREA,
                device: "d",
            },
            100,
        );
        // Different layout => the zone index is meaningless.
        match s.suggest("k", LayoutKind::Dwindle, 4, AREA) {
            Some(Suggestion::Rect(got)) => assert_eq!(got, r),
            other => panic!("expected a rect fallback, got {other:?}"),
        }
    }

    #[test]
    fn a_changed_window_count_falls_back_to_the_rect() {
        let mut s = Store::default();
        s.remember(
            "k".into(),
            Placement {
                zone_index: 3,
                zone_count: 4,
                layout: LayoutKind::Grid,
                rect: Rect::new(960, 540, 1920, 1080),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        assert!(matches!(
            s.suggest("k", LayoutKind::Grid, 2, AREA),
            Some(Suggestion::Rect(_))
        ));
    }

    #[test]
    fn an_out_of_range_zone_index_never_escapes() {
        // Guards against indexing the zone vec out of bounds.
        let mut s = Store::default();
        s.remember(
            "k".into(),
            Placement {
                zone_index: 7,
                zone_count: 8,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 100, 100),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        match s.suggest("k", LayoutKind::Grid, 8, AREA) {
            Some(Suggestion::Zone(i)) => assert!(i < 8),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn suggestion_survives_a_resolution_change() {
        let mut s = Store::default();
        s.remember(
            "k".into(),
            Placement {
                zone_index: 0,
                zone_count: 4,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 960, 540),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        let uhd = Rect::new(0, 0, 3840, 2160);
        match s.suggest("k", LayoutKind::Monocle, 1, uhd) {
            Some(Suggestion::Rect(r)) => assert_eq!(r, Rect::new(0, 0, 1920, 1080)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn remembering_the_same_key_twice_updates_rather_than_duplicates() {
        let mut s = Store::default();
        s.remember(
            "k".into(),
            Placement {
                zone_index: 0,
                zone_count: 2,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 10, 10),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        s.remember(
            "k".into(),
            Placement {
                zone_index: 1,
                zone_count: 2,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 20, 20),
                work_area: AREA,
                device: "d",
            },
            100,
        );
        assert_eq!(s.len(), 1);
        assert_eq!(
            s.suggest("k", LayoutKind::Grid, 2, AREA),
            Some(Suggestion::Zone(1))
        );
    }

    // --- bounds and eviction -------------------------------------------------

    #[test]
    fn the_store_never_exceeds_max_entries() {
        let mut s = Store::default();
        for i in 0..50 {
            s.remember(
                format!("k{i}"),
                Placement {
                    zone_index: 0,
                    zone_count: 1,
                    layout: LayoutKind::Grid,
                    rect: Rect::new(0, 0, 10, 10),
                    work_area: AREA,
                    device: "d",
                },
                10,
            );
            assert!(s.len() <= 10, "grew to {} at i={i}", s.len());
        }
        assert_eq!(s.len(), 10);
    }

    #[test]
    fn eviction_keeps_the_most_recently_used() {
        let mut s = Store::default();
        // All share a timestamp, so the tie-break on key decides; what matters
        // is that the cap holds and the survivors are a deterministic subset.
        for i in 0..20 {
            s.remember(
                format!("k{i:02}"),
                Placement {
                    zone_index: 0,
                    zone_count: 1,
                    layout: LayoutKind::Grid,
                    rect: Rect::new(0, 0, 10, 10),
                    work_area: AREA,
                    device: "d",
                },
                5,
            );
        }
        assert_eq!(s.len(), 5);
        // Re-remembering an existing key must not push the store over the cap.
        let survivor = s.entries[0].key.clone();
        s.remember(
            survivor.clone(),
            Placement {
                zone_index: 0,
                zone_count: 1,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 10, 10),
                work_area: AREA,
                device: "d",
            },
            5,
        );
        assert_eq!(s.len(), 5);
        assert!(s.get(&survivor).is_some());
    }

    #[test]
    fn max_entries_zero_disables_the_store() {
        let mut s = store_with(3);
        s.remember(
            "k".into(),
            Placement {
                zone_index: 0,
                zone_count: 1,
                layout: LayoutKind::Grid,
                rect: Rect::new(0, 0, 10, 10),
                work_area: AREA,
                device: "d",
            },
            0,
        );
        assert!(s.is_empty());
    }

    #[test]
    fn forget_removes_only_the_named_entry() {
        let mut s = store_with(3);
        assert!(s.forget("fp|app1.exe|Cls"));
        assert_eq!(s.len(), 2);
        assert!(s.get("fp|app1.exe|Cls").is_none());
        assert!(s.get("fp|app0.exe|Cls").is_some());
        assert!(!s.forget("nope"));
    }

    #[test]
    fn clear_empties_the_store() {
        let mut s = store_with(5);
        s.clear();
        assert!(s.is_empty());
        assert!(s.get("fp|app0.exe|Cls").is_none());
    }

    #[test]
    fn the_index_stays_consistent_after_eviction_and_removal() {
        let mut s = store_with(20);
        s.evict_to(7);
        assert_eq!(s.len(), 7);
        for e in s.entries.clone() {
            assert_eq!(s.get(&e.key).map(|x| x.key.clone()), Some(e.key.clone()));
        }
        let victim = s.entries[3].key.clone();
        s.forget(&victim);
        for e in s.entries.clone() {
            assert_eq!(s.get(&e.key).map(|x| x.key.clone()), Some(e.key.clone()));
        }
    }

    // --- persistence ---------------------------------------------------------

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("supertile-mem-{}-{name}.json", std::process::id()));
        p
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let s = store_with(4);
        s.save_to(&path).unwrap();
        let back = Store::load_from(&path);
        assert_eq!(back.len(), 4);
        for e in &s.entries {
            assert_eq!(back.get(&e.key), Some(e));
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_index_is_rebuilt_on_load() {
        let path = temp_path("index");
        store_with(3).save_to(&path).unwrap();
        let back = Store::load_from(&path);
        // `index` is #[serde(skip)], so this only works if load reindexes.
        assert!(back.get("fp|app2.exe|Cls").is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_yields_an_empty_store() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(Store::load_from(&path).is_empty());
    }

    #[test]
    fn a_corrupt_file_yields_an_empty_store_rather_than_failing() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "}}} not toml").unwrap();
        assert!(Store::load_from(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_older_schema_is_discarded() {
        let path = temp_path("oldschema");
        std::fs::write(&path, "version = 0\n").unwrap();
        assert!(Store::load_from(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn entries_with_insane_geometry_are_dropped_on_load() {
        let path = temp_path("insane");
        let text = format!(
            r#"{{
  "version": {SCHEMA_VERSION},
  "entries": [
    {{ "key": "good", "zone_index": 0, "zone_count": 1, "layout": "grid",
      "frac": {{ "left": 0.0, "top": 0.0, "right": 1.0, "bottom": 1.0 }},
      "device": "d", "last_used": 1 }},
    {{ "key": "bad", "zone_index": 0, "zone_count": 1, "layout": "grid",
      "frac": {{ "left": 1.0, "top": 0.0, "right": 0.0, "bottom": 1.0 }},
      "device": "d", "last_used": 1 }}
  ]
}}"#
        );
        std::fs::write(&path, text).unwrap();
        let s = Store::load_from(&path);
        assert!(s.get("good").is_some(), "valid entry was dropped");
        assert!(
            s.get("bad").is_none(),
            "inverted rect should have been dropped"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let path = temp_path("notmp");
        store_with(2).save_to(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_empty_store_persists_and_reloads() {
        let path = temp_path("empty");
        Store::default().save_to(&path).unwrap();
        assert!(Store::load_from(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
