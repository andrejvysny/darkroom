//! Catalog backup: periodic + manual `VACUUM INTO` snapshots of `catalog.db`, via core-db's
//! `backup` module. IPC surface for this lives in `commands.rs` (`catalog_backup_now` /
//! `catalog_backup_status`); this module owns the policy (where, how many, when).

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, Runtime};

/// Rotated backups to keep on disk.
const KEEP: usize = 7;
/// Startup check backs up again only once the newest copy is at least this old.
const MIN_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupStatus {
    /// Epoch millis of the newest backup's mtime, or `None` if there isn't one yet.
    pub last_ms: Option<i64>,
    pub count: usize,
    pub dir: String,
}

fn backups_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .join("backups"))
}

fn status_for(dir: &std::path::Path) -> BackupStatus {
    let entries = core_db::backup::backups(dir);
    BackupStatus {
        last_ms: entries.first().map(|(_, mtime)| to_millis(*mtime)),
        count: entries.len(),
        dir: dir.display().to_string(),
    }
}

fn to_millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Current backup status — no DB lock needed, just a directory listing.
pub fn status<R: Runtime>(app: &AppHandle<R>) -> Result<BackupStatus, String> {
    Ok(status_for(&backups_dir(app)?))
}

/// Run a backup now (briefly holds the `db` lock for the `VACUUM INTO`), returning fresh status.
pub fn run_now<R: Runtime>(app: &AppHandle<R>) -> Result<BackupStatus, String> {
    let dir = backups_dir(app)?;
    let st = app.state::<AppState>();
    let db = st.db.lock().map_err(|e| e.to_string())?;
    core_db::backup::backup_now(&db.conn, &dir, KEEP).map_err(|e| e.to_string())?;
    Ok(status_for(&dir))
}

/// Best-effort startup check: if the newest backup is missing or older than 24h, take one in the
/// background so a long-running session is never more than a day from a recovery point. Runs off
/// the setup thread and logs its own result — nothing here can fail app startup.
pub fn maybe_backup_on_startup<R: Runtime + 'static>(app: AppHandle<R>) {
    std::thread::spawn(move || {
        let dir = match backups_dir(&app) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "catalog backup: app data dir unavailable");
                return;
            }
        };
        let stale = match core_db::backup::backups(&dir).first() {
            Some((_, mtime)) => mtime.elapsed().unwrap_or(MIN_INTERVAL) >= MIN_INTERVAL,
            None => true,
        };
        if !stale {
            return;
        }
        let st = app.state::<AppState>();
        let result = st.db.lock().map_err(|e| e.to_string()).and_then(|db| {
            core_db::backup::backup_now(&db.conn, &dir, KEEP).map_err(|e| e.to_string())
        });
        match result {
            // Log the filename only, not the full path — app-data paths embed the OS username.
            Ok(path) => tracing::info!(
                file = %path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                "catalog backup complete"
            ),
            Err(error) => tracing::warn!(error = %error, "catalog backup failed"),
        }
    });
}
