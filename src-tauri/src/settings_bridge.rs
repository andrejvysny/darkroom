//! Legacy-app settings bridge: the browser UI captures its `localStorage` preferences as a
//! [`core_model::settings::WebSnapshot`] and hands it to these commands, so the native settings
//! store can import the user's choices once on first launch.
//!
//! Two write paths, both atomic (same-dir temp file + `sync_all` + rename): the fixed bridge
//! file under the catalog's app-data dir that `core_runtime::settings_store` ingests, and a
//! user-chosen export file ("Export settings…"). Values are the raw stored strings — this layer
//! never re-encodes them.

use core_model::settings::parse_web_snapshot;
use core_runtime::settings_store::SettingsPaths;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Manager};

static WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Validate a snapshot envelope and return the exact JSON text to persist.
fn validate(snapshot: &serde_json::Value) -> Result<String, String> {
    // Serialize first so the native parser sees the bytes that would hit disk — a Value that
    // parses but re-serializes differently is rejected before, not after, the write.
    let text = serde_json::to_string(snapshot).map_err(|e| format!("snapshot: {e}"))?;
    parse_web_snapshot(&text).map_err(|e| e.to_string())?;
    Ok(text)
}

/// Same-dir temp file + fsync + rename: a crash leaves either the old or the new file.
fn atomic_write(file: &Path, contents: &str) -> Result<(), String> {
    let dir = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let file_name = file
        .file_name()
        .ok_or_else(|| format!("{}: not a file", file.display()))?
        .to_string_lossy()
        .into_owned();
    let tmp = dir.join(format!(
        "{file_name}.tmp-{}-{}",
        std::process::id(),
        WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), String> {
        let mut handle = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        handle
            .write_all(contents.as_bytes())
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        handle
            .sync_all()
            .map_err(|e| format!("{} sync: {e}", tmp.display()))?;
        drop(handle);
        fs::rename(&tmp, file).map_err(|e| format!("rename to {}: {e}", file.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Write the bridge file, honoring the never-go-backwards rule: if the file on disk already
/// parses as a snapshot captured at or after `snapshot`, the write is skipped (Ok) — a slower
/// UI must never replace a newer snapshot. A file that doesn't parse is replaced.
/// Takes the path so the rule is testable without a running app.
fn write_bridge(file: &Path, snapshot: &serde_json::Value) -> Result<(), String> {
    let text = validate(snapshot)?;
    let incoming = parse_web_snapshot(&text).map_err(|e| e.to_string())?;
    if let Ok(existing) = fs::read_to_string(file) {
        if let Ok(current) = parse_web_snapshot(&existing) {
            if current.captured_at_ms >= incoming.captured_at_ms {
                return Ok(());
            }
        }
    }
    atomic_write(file, &text)
}

/// Persist a validated web snapshot to the bridge file the native settings store ingests
/// (`<app data dir>/bridge/web-settings-v1.json`). Idempotent against newer snapshots.
#[tauri::command]
pub async fn settings_bridge_write(
    app: AppHandle,
    snapshot: serde_json::Value,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "settings_bridge_write",
        move || {
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app_data_dir: {e}"))?;
            let file = SettingsPaths::from_app_data_dir(data_dir).bridge_file();
            write_bridge(&file, &snapshot)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Persist a validated web snapshot to a user-chosen path ("Export settings…"). Unlike the
/// bridge file this overwrites: the user picked this file to receive the current snapshot.
#[tauri::command]
pub async fn settings_bridge_export(
    app: AppHandle,
    path: String,
    snapshot: serde_json::Value,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "settings_bridge_export",
        move || {
            let text = validate(&snapshot)?;
            atomic_write(&PathBuf::from(path), &text)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(captured_at_ms: u64, bootstrapped: &str) -> serde_json::Value {
        json!({
            "format": "darkroom-web-settings",
            "version": 1,
            "capturedAtMs": captured_at_ms,
            "values": { "darkroom.bootstrapped": bootstrapped }
        })
    }

    fn captured_at(file: &Path) -> u64 {
        let text = fs::read_to_string(file).expect("bridge file exists");
        parse_web_snapshot(&text).expect("parses").captured_at_ms
    }

    #[test]
    fn bridge_write_never_replaces_a_newer_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "darkroom-settings-bridge-{}-{}",
            std::process::id(),
            WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let file = dir.join("bridge").join("web-settings-v1.json");

        // First write creates the bridge/ directory and the file.
        write_bridge(&file, &snapshot(2000, "1")).expect("first write");
        assert_eq!(captured_at(&file), 2000);

        // An older snapshot is skipped — the newer file stays byte-identical.
        let before = fs::read_to_string(&file).unwrap();
        write_bridge(&file, &snapshot(1000, "0")).expect("older write skips cleanly");
        assert_eq!(fs::read_to_string(&file).unwrap(), before);
        assert_eq!(captured_at(&file), 2000);

        // Equal timestamp is also "not newer" (the `>=` rule): still skipped.
        write_bridge(&file, &snapshot(2000, "0")).expect("equal write skips");
        assert_eq!(fs::read_to_string(&file).unwrap(), before);

        // A strictly newer snapshot replaces it.
        write_bridge(&file, &snapshot(3000, "0")).expect("newer write");
        assert_eq!(captured_at(&file), 3000);
        assert!(fs::read_to_string(&file)
            .unwrap()
            .contains("\"darkroom.bootstrapped\":\"0\""));

        // An unparseable existing file is replaced rather than quarantined here — the store
        // quarantines on load; the bridge just refuses to be blocked by garbage.
        fs::write(&file, "not json at all").unwrap();
        write_bridge(&file, &snapshot(500, "1")).expect("corrupt file is replaced");
        assert_eq!(captured_at(&file), 500);

        // An invalid envelope never touches disk, even over a corrupt file.
        fs::write(&file, "not json at all").unwrap();
        assert!(write_bridge(&file, &json!({ "format": "other" })).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "not json at all");
        assert!(write_bridge(&file, &json!({ "format": "darkroom-web-settings", "version": 2, "capturedAtMs": 9, "values": {} })).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "not json at all");

        fs::remove_dir_all(&dir).ok();
    }
}
