//! Atomic on-disk settings store plus the legacy web-snapshot bridge ingestion.
//!
//! Persistence only: the merge/provenance rules live in `core_model::settings`. A crash
//! mid-write must leave either the old or the new file, so every write goes through a
//! same-directory temp file + rename, and corrupt files are quarantined, never deleted.

use core_model::settings::{
    import_web, parse_settings, parse_web_snapshot, to_json, ImportReport, Settings, SettingsError,
};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Where settings live. Production: the app-data dir of identity `com.andrejvysny.darkroom`
/// (macOS `~/Library/Application Support/com.andrejvysny.darkroom/`), i.e. the same directory as
/// `catalog.db`. Tests pass a temp dir.
#[derive(Clone, Debug)]
pub struct SettingsPaths {
    pub dir: PathBuf,
}

impl SettingsPaths {
    /// Hosts resolve the app-data directory (Tauri `app_data_dir` or equivalent); this crate
    /// never derives a home path itself.
    pub fn from_app_data_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.dir.join("settings.json")
    }

    /// Written by the legacy app for us to pick up once.
    pub fn bridge_file(&self) -> PathBuf {
        self.dir.join("bridge").join("web-settings-v1.json")
    }
}

#[derive(Debug)]
pub enum LoadOutcome {
    Loaded,
    CreatedDefaults,
    /// Corrupt file renamed aside (never deleted); defaults are in use.
    RecoveredFromCorrupt {
        moved_to: PathBuf,
        message: String,
    },
    /// File written by a newer schema; in-memory defaults, store is read-only.
    FutureSchema {
        found: u32,
    },
}

pub struct SettingsStore {
    paths: SettingsPaths,
    settings: Settings,
    read_only: bool,
}

impl SettingsStore {
    /// Load or create. Corrupt file -> renamed to `settings.json.corrupt-<now_ms>` (never
    /// deleted), defaults used, message returned. Future schema -> defaults in memory,
    /// `read_only = true` (never overwrite a newer build's file), outcome reports it.
    pub fn open(paths: SettingsPaths, now_ms: u64) -> io::Result<(Self, LoadOutcome)> {
        let (settings, read_only, outcome) = load_document(&paths, now_ms)?;
        let store = Self {
            paths,
            settings,
            read_only,
        };
        store.cleanup_stale_tmp();
        Ok((store, outcome))
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mutate + persist atomically. Err if read_only.
    pub fn update(&mut self, f: impl FnOnce(&mut Settings)) -> Result<(), StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        let mut candidate = self.settings.clone();
        f(&mut candidate);
        self.persist(&candidate)?;
        self.settings = candidate;
        Ok(())
    }

    /// Import the legacy bridge file if present (idempotent via `import_web`); persists only if
    /// something changed. Missing file -> Ok(None).
    pub fn import_bridge(&mut self, now_ms: u64) -> Result<Option<ImportReport>, StoreError> {
        let file = self.paths.bridge_file();
        match read_candidate(&file) {
            Ok(None) => Ok(None),
            Err(error) => Err(error),
            Ok(Some(json)) => self
                .apply_snapshot(&json, &file.display().to_string(), now_ms)
                .map(Some),
        }
    }

    /// Same for a user-chosen exported settings file (the skipped-bridge recovery path).
    pub fn import_file(&mut self, path: &Path, now_ms: u64) -> Result<ImportReport, StoreError> {
        let json = read_candidate(path)?
            .ok_or_else(|| StoreError::Invalid(format!("{}: no such file", path.display())))?;
        self.apply_snapshot(&json, &path.display().to_string(), now_ms)
    }

    fn apply_snapshot(
        &mut self,
        json: &str,
        source: &str,
        now_ms: u64,
    ) -> Result<ImportReport, StoreError> {
        if self.read_only {
            return Err(StoreError::ReadOnly);
        }
        let snapshot = parse_web_snapshot(json).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let (candidate, report) = import_web(&self.settings, &snapshot, source, now_ms);
        // Rejected keys still record the BridgeRecord, so any non-idempotent outcome persists.
        if !report.already_imported && (!report.applied.is_empty() || !report.rejected.is_empty()) {
            self.persist(&candidate)?;
            self.settings = candidate;
        }
        Ok(report)
    }

    fn persist(&self, candidate: &Settings) -> Result<(), StoreError> {
        atomic_write(&self.paths.settings_file(), &to_json(candidate))?;
        Ok(())
    }

    /// Best-effort: temp files left by a crashed write are junk we generated.
    fn cleanup_stale_tmp(&self) {
        let Ok(entries) = fs::read_dir(&self.paths.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("settings.json.tmp-") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[derive(Debug)]
pub enum StoreError {
    ReadOnly,
    Io(io::Error),
    Invalid(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "settings store is read-only"),
            Self::Io(e) => write!(f, "settings io error: {e}"),
            Self::Invalid(reason) => write!(f, "settings source is invalid: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

type Loaded = (Settings, bool, LoadOutcome);

fn load_document(paths: &SettingsPaths, now_ms: u64) -> io::Result<Loaded> {
    let file = paths.settings_file();
    let json = match fs::read_to_string(&file) {
        Ok(json) => json,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let settings = Settings::default();
            atomic_write(&file, &to_json(&settings))?;
            return Ok((settings, false, LoadOutcome::CreatedDefaults));
        }
        Err(e) => return Err(e),
    };
    match parse_settings(&json) {
        Ok(settings) => Ok((settings, false, LoadOutcome::Loaded)),
        Err(SettingsError::FutureSchema(found)) => Ok((
            Settings::default(),
            true,
            LoadOutcome::FutureSchema { found },
        )),
        Err(SettingsError::Corrupt(message)) => {
            let moved_to = quarantine(&file, now_ms)?;
            let settings = Settings::default();
            atomic_write(&file, &to_json(&settings))?;
            Ok((
                settings,
                false,
                LoadOutcome::RecoveredFromCorrupt { moved_to, message },
            ))
        }
    }
}

/// Rename aside a corrupt file, never overwriting an existing quarantine copy.
fn quarantine(file: &Path, now_ms: u64) -> io::Result<PathBuf> {
    let base = format!("{}.corrupt-{now_ms}", file.display());
    let mut candidate = PathBuf::from(&base);
    let mut suffix = 1_u32;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{base}-{suffix}"));
        suffix += 1;
    }
    fs::rename(file, &candidate)?;
    Ok(candidate)
}

fn read_candidate(path: &Path) -> Result<Option<String>, StoreError> {
    match fs::read_to_string(path) {
        Ok(json) => Ok(Some(json)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Same-dir temp file + fsync + rename, then best-effort dir fsync: a crash leaves either the
/// old or the new file, never a partial one.
fn atomic_write(file: &Path, contents: &str) -> io::Result<()> {
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let file_name = file
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: not a file", file.display()),
            )
        })?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(
        "{file_name}.tmp-{}-{}",
        std::process::id(),
        WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = write_and_rename(&tmp, file, contents, dir);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn write_and_rename(tmp: &Path, file: &Path, contents: &str, dir: &Path) -> io::Result<()> {
    let mut handle = File::create(tmp)?;
    io::Write::write_all(&mut handle, contents.as_bytes())?;
    handle.sync_all()?;
    drop(handle);
    fs::rename(tmp, file)?;
    // Durability of the rename itself; best-effort where dir fsync is unsupported.
    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}
