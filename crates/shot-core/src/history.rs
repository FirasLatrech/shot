//! Capture history: every capture is copied into a folder with a JSON index,
//! so it can be restored later. Entries older than the retention are pruned.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureKind {
    Area,
    Window,
    Fullscreen,
    Scrolling,
}

impl CaptureKind {
    pub fn label(self) -> &'static str {
        match self {
            CaptureKind::Area => "Area",
            CaptureKind::Window => "Window",
            CaptureKind::Fullscreen => "Fullscreen",
            CaptureKind::Scrolling => "Scrolling",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub kind: CaptureKind,
    /// Unix seconds.
    pub created: u64,
    pub width: u32,
    pub height: u32,
}

pub struct History {
    dir: PathBuf,
    entries: Vec<Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl History {
    pub fn open(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let entries = read_json_or_backup(&dir.join("index.json")).unwrap_or_default();
        Ok(Self { dir, entries })
    }

    /// Newest first.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().rev()
    }

    pub fn path(&self, e: &Entry) -> PathBuf {
        self.dir.join(format!("{}.png", e.id))
    }

    pub fn add(&mut self, kind: CaptureKind, png: &[u8], width: u32, height: u32) -> io::Result<Entry> {
        let created = now();
        // Seconds plus a counter keeps ids unique and sortable. Use one past the
        // highest counter so a removed entry's id is never reused.
        let n = self
            .entries
            .iter()
            .filter(|e| e.created == created)
            .filter_map(|e| e.id.rsplit_once('-')?.1.parse::<u32>().ok())
            .max()
            .map_or(0, |m| m + 1);
        let entry = Entry { id: format!("{created}-{n}"), kind, created, width, height };
        std::fs::write(self.path(&entry), png)?;
        self.entries.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    pub fn remove(&mut self, id: &str) -> io::Result<()> {
        if let Some(i) = self.entries.iter().position(|e| e.id == id) {
            let e = self.entries.remove(i);
            let _ = std::fs::remove_file(self.path(&e));
            self.save()?;
        }
        Ok(())
    }

    pub fn clear(&mut self) -> io::Result<()> {
        for e in std::mem::take(&mut self.entries) {
            let _ = std::fs::remove_file(self.path(&e));
        }
        self.save()
    }

    /// Drops entries created more than `max_age` seconds before `now`.
    pub fn prune(&mut self, max_age: u64, now: u64) -> io::Result<()> {
        let (old, keep): (Vec<Entry>, Vec<Entry>) =
            std::mem::take(&mut self.entries).into_iter().partition(|e| now.saturating_sub(e.created) > max_age);
        self.entries = keep;
        if old.is_empty() {
            return Ok(());
        }
        for e in &old {
            let _ = std::fs::remove_file(self.path(e));
        }
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        write_atomic(&self.dir.join("index.json"), &serde_json::to_vec(&self.entries)?)
    }
}

/// Write-then-rename so a crash or full disk never leaves a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Reads a JSON file. If it exists but can't be parsed (corrupt, or written
/// by a newer version), it's moved aside to `<name>.bad` instead of being
/// silently overwritten, and `None` is returned.
pub fn read_json_or_backup<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            let mut bad = path.as_os_str().to_owned();
            bad.push(".bad");
            eprintln!("{}: {e}; keeping a copy at {}", path.display(), PathBuf::from(&bad).display());
            let _ = std::fs::rename(path, bad);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_prune() {
        let dir = std::env::temp_dir().join(format!("shot-history-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut h = History::open(&dir).unwrap();
        let a = h.add(CaptureKind::Area, b"png", 1, 1).unwrap();
        let b = h.add(CaptureKind::Window, b"png", 1, 1).unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(h.entries().next().unwrap().id, b.id, "newest first");

        let reopened = History::open(&dir).unwrap();
        assert_eq!(reopened.entries().count(), 2);

        // Ids stay unique after a removal in the same second.
        h.remove(&a.id).unwrap();
        let c = h.add(CaptureKind::Area, b"png", 1, 1).unwrap();
        assert!(h.entries().filter(|e| e.id == c.id).count() == 1 && c.id != b.id);

        h.prune(60, now() + 3600).unwrap();
        assert_eq!(h.entries().count(), 0);
        assert!(!h.path(&a).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_index_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("shot-history-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), b"{not json").unwrap();
        let h = History::open(&dir).unwrap();
        assert_eq!(h.entries().count(), 0);
        assert!(dir.join("index.json.bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
