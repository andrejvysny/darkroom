use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager, Runtime};
use tracing::Level;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

const CONFIG_FILE: &str = "logging.json";
const ACTIVE_LOG: &str = "darkroom.log";

static STATE: OnceLock<RwLock<LogState>> = OnceLock::new();
static FILTER: OnceLock<reload::Handle<EnvFilter, tracing_subscriber::Registry>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsStatus {
    pub directory: String,
    pub size_bytes: u64,
    pub file_count: usize,
    pub level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogConfig {
    directory: Option<PathBuf>,
    level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            directory: None,
            level: "debug".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct LogState {
    app_data_dir: PathBuf,
    directory: PathBuf,
    level: String,
}

#[derive(Clone)]
struct LogWriter;

struct FileLineWriter {
    file: Option<File>,
}

impl Write for FileLineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.file {
            file.write(buf)
        } else {
            io::stderr().write(buf)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()
        } else {
            io::stderr().flush()
        }
    }
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = FileLineWriter;

    fn make_writer(&'a self) -> Self::Writer {
        let file = STATE
            .get()
            .and_then(|state| state.read().ok().map(|s| s.directory.join(ACTIVE_LOG)))
            .and_then(|path| {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                OpenOptions::new().create(true).append(true).open(path).ok()
            });
        FileLineWriter { file }
    }
}

pub fn init<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    fs::create_dir_all(&app_data_dir).map_err(|e| format!("create app data dir: {e}"))?;
    let config = read_config(&app_data_dir);
    let directory = config
        .directory
        .clone()
        .unwrap_or_else(|| app_data_dir.join("logs"));
    fs::create_dir_all(&directory).map_err(|e| format!("create log dir: {e}"))?;
    validate_level(&config.level)?;

    let _ = STATE.set(RwLock::new(LogState {
        app_data_dir: app_data_dir.clone(),
        directory,
        level: config.level.clone(),
    }));

    let filter = filter_for(&config.level)?;
    let (filter_layer, handle) = reload::Layer::new(filter);
    let subscriber = tracing_subscriber::registry().with(filter_layer).with(
        tracing_subscriber::fmt::layer()
            .with_writer(LogWriter)
            .with_target(true)
            .with_thread_names(true)
            .with_ansi(false),
    );
    let _ = FILTER.set(handle);
    subscriber
        .try_init()
        .map_err(|e| format!("init logging: {e}"))?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "logging initialized");
    Ok(())
}

pub fn status() -> Result<LogsStatus, String> {
    let state = state()?;
    status_for(&state.directory, &state.level)
}

pub fn set_directory(path: &Path) -> Result<LogsStatus, String> {
    fs::create_dir_all(path).map_err(|e| format!("create log directory: {e}"))?;
    let test = path.join(".darkroom-log-write-test");
    fs::write(&test, b"ok").map_err(|e| format!("log directory is not writable: {e}"))?;
    let _ = fs::remove_file(&test);

    let mut state = state_write()?;
    copy_logs(&state.directory, path)?;
    state.directory = path.to_path_buf();
    persist_config(&state)?;
    tracing::info!("log directory changed");
    status_for(&state.directory, &state.level)
}

pub fn set_level(level: &str) -> Result<LogsStatus, String> {
    validate_level(level)?;
    if let Some(handle) = FILTER.get() {
        handle
            .modify(|filter| {
                *filter = filter_for(level).unwrap_or_else(|_| EnvFilter::new("debug"))
            })
            .map_err(|e| format!("set log level: {e}"))?;
    }
    let mut state = state_write()?;
    state.level = level.to_string();
    persist_config(&state)?;
    tracing::info!(level, "log level changed");
    status_for(&state.directory, &state.level)
}

pub fn delete_all() -> Result<LogsStatus, String> {
    let state = state()?;
    for entry in fs::read_dir(&state.directory).map_err(|e| format!("read log dir: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if is_log_file(&path) {
            let _ = fs::remove_file(&path);
        }
    }
    tracing::info!("logs deleted");
    status_for(&state.directory, &state.level)
}

pub fn export_zip(dest: &Path) -> Result<u64, String> {
    let state = state()?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create zip parent: {e}"))?;
    }
    let file = File::create(dest).map_err(|e| format!("create log zip: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for entry in fs::read_dir(&state.directory).map_err(|e| format!("read log dir: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !is_log_file(&path) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("darkroom.log");
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        let mut src = File::open(&path).map_err(|e| e.to_string())?;
        io::copy(&mut src, &mut zip).map_err(|e| e.to_string())?;
    }
    zip.start_file("diagnostics.json", options)
        .map_err(|e| e.to_string())?;
    let diagnostics = json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "exportedAtMs": now_ms(),
        "schema": 1,
    });
    zip.write_all(diagnostics.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let mut file = zip.finish().map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    let bytes = fs::metadata(dest).map_err(|e| e.to_string())?.len();
    tracing::info!(bytes, "logs exported");
    Ok(bytes)
}

pub fn frontend_log(level: &str, target: &str, message: &str, fields: Option<Value>) {
    let message = truncate(message, 500);
    let target = truncate(target, 80);
    let fields = fields.map(redact_value).unwrap_or(Value::Null);
    match level {
        "error" => {
            tracing::error!(target: "frontend", frontend_target = %target, fields = %fields, "{message}")
        }
        "warn" => {
            tracing::warn!(target: "frontend", frontend_target = %target, fields = %fields, "{message}")
        }
        "info" => {
            tracing::info!(target: "frontend", frontend_target = %target, fields = %fields, "{message}")
        }
        "trace" => {
            tracing::trace!(target: "frontend", frontend_target = %target, fields = %fields, "{message}")
        }
        _ => {
            tracing::debug!(target: "frontend", frontend_target = %target, fields = %fields, "{message}")
        }
    }
}

pub fn safe_error(e: &dyn std::error::Error) -> String {
    let name = std::any::type_name_of_val(e);
    name.rsplit("::").next().unwrap_or("error").to_string()
}

fn read_config(app_data_dir: &Path) -> LogConfig {
    fs::read_to_string(app_data_dir.join(CONFIG_FILE))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn persist_config(state: &LogState) -> Result<(), String> {
    let cfg = LogConfig {
        directory: Some(state.directory.clone()),
        level: state.level.clone(),
    };
    fs::write(
        state.app_data_dir.join(CONFIG_FILE),
        serde_json::to_vec_pretty(&cfg).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write logging config: {e}"))
}

fn state() -> Result<LogState, String> {
    STATE
        .get()
        .ok_or_else(|| "logging is not initialized".to_string())?
        .read()
        .map_err(|e| e.to_string())
        .map(|s| s.clone())
}

fn state_write() -> Result<std::sync::RwLockWriteGuard<'static, LogState>, String> {
    STATE
        .get()
        .ok_or_else(|| "logging is not initialized".to_string())?
        .write()
        .map_err(|e| e.to_string())
}

fn status_for(directory: &Path, level: &str) -> Result<LogsStatus, String> {
    let mut size_bytes = 0;
    let mut file_count = 0;
    if directory.exists() {
        for entry in fs::read_dir(directory).map_err(|e| format!("read log dir: {e}"))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if is_log_file(&path) {
                file_count += 1;
                size_bytes += fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    Ok(LogsStatus {
        directory: directory.display().to_string(),
        size_bytes,
        file_count,
        level: level.to_string(),
    })
}

fn copy_logs(from: &Path, to: &Path) -> Result<(), String> {
    if from == to || !from.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(from).map_err(|e| format!("read old log dir: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if !is_log_file(&path) {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        let dest = to.join(name);
        if dest.exists() {
            continue;
        }
        fs::copy(&path, dest).map_err(|e| format!("copy log: {e}"))?;
    }
    Ok(())
}

fn is_log_file(path: &Path) -> bool {
    path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("log")
}

fn filter_for(level: &str) -> Result<EnvFilter, String> {
    validate_level(level)?;
    Ok(EnvFilter::new(format!(
        "darkroom={level},darkroom_lib={level},frontend={level},warn"
    )))
}

fn validate_level(level: &str) -> Result<(), String> {
    match level {
        "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
        _ => Err("invalid log level".to_string()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    if is_sensitive_key(&k) {
                        (k, Value::String("[redacted]".to_string()))
                    } else {
                        (k, redact_value(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().take(20).map(redact_value).collect()),
        Value::String(s) => Value::String(truncate(&s, 200)),
        other => other,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("path")
        || k.contains("filename")
        || k.contains("search")
        || k.contains("caption")
        || k.contains("keyword")
        || k.contains("person")
        || k.contains("hash")
        || k.contains("url")
        || k.contains("name")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _level_to_tracing(level: &str) -> Level {
    match level {
        "error" => Level::ERROR,
        "warn" => Level::WARN,
        "info" => Level::INFO,
        "trace" => Level::TRACE,
        _ => Level::DEBUG,
    }
}
