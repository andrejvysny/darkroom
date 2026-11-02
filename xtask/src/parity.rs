use regex::Regex;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Contract {
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    surface: Vec<Surface>,
}

#[derive(Deserialize)]
struct Command {
    name: String,
    source: String,
    behavior: String,
    target: String,
    task: String,
    test: String,
}

#[derive(Deserialize)]
struct Surface {
    kind: String,
    name: String,
    source: String,
    evidence: String,
    behavior: String,
    target: String,
    task: String,
    test: String,
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn captures(pattern: &str, text: &str) -> Vec<String> {
    Regex::new(pattern)
        .expect("valid pattern")
        .captures_iter(text)
        .map(|capture| capture[1].to_owned())
        .collect()
}

fn unique(
    items: impl IntoIterator<Item = String>,
    label: &str,
) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    for item in items {
        if !result.insert(item.clone()) {
            return Err(format!("duplicate {label}: {item}"));
        }
    }
    Ok(result)
}

fn compare(
    actual: BTreeSet<String>,
    expected: BTreeSet<String>,
    label: &str,
) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "{label} drift: missing from contract {:?}; stale {:?}",
            actual.difference(&expected).collect::<Vec<_>>(),
            expected.difference(&actual).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

fn registered(text: &str) -> Result<BTreeSet<String>, String> {
    let start = text
        .find("tauri::generate_handler![")
        .ok_or("handler registry missing")?;
    let rest = &text[start + "tauri::generate_handler![".len()..];
    let end = rest.find(']').ok_or("handler registry unterminated")?;
    let body = &rest[..end];
    let re = Regex::new(r"(?m)^\s*commands::([a-zA-Z0-9_]+)\s*,\s*$").unwrap();
    let names = captures(re.as_str(), body);
    let remainder = re.replace_all(body, "");
    if !remainder.trim().is_empty() {
        return Err("unclassified handler syntax".into());
    }
    unique(names, "registered handler")
}

fn files(dir: &Path, extensions: &[&str], result: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            files(&path, extensions, result)?;
        } else if path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|e| extensions.contains(&e))
        {
            result.push(path);
        }
    }
    Ok(())
}

fn events(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut paths = Vec::new();
    files(&root.join("src-tauri/src"), &["rs"], &mut paths)?;
    let mut names = BTreeSet::new();
    for path in paths {
        let text = read(&path)?;
        for arg in captures(r#"\.emit\(\s*([^,\s]+)"#, &text) {
            if let Some(name) = arg.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                names.insert(name.to_owned());
            } else if !((arg == "event" && path.ends_with("model_mgmt.rs"))
                || (arg == "FLUSH_EVENT" && path.ends_with("quit.rs")))
            {
                return Err(format!(
                    "unclassified event expression {arg} in {}",
                    path.display()
                ));
            }
        }
        names.extend(captures(r#"emit_dl\([^,]+,\s*"([^"]+)""#, &text));
        names.extend(captures(
            r#"const FLUSH_EVENT:\s*&str\s*=\s*"([^"]+)""#,
            &text,
        ));
    }
    Ok(names)
}

fn frontend(root: &Path, kind: &str) -> Result<BTreeSet<String>, String> {
    let mut paths = Vec::new();
    files(&root.join("src"), &["ts", "tsx"], &mut paths)?;
    let mut result = BTreeSet::new();
    for path in paths {
        if path.to_string_lossy().contains("/dev/") {
            continue;
        }
        let text = read(&path)?;
        if kind == "screen" && path.extension().is_some_and(|ext| ext == "tsx") {
            result.extend(captures(
                r"<([A-Z][A-Za-z0-9]*(?:Modal|Dialog|View|Review|Palette))\b",
                &text,
            ));
        } else if kind == "storage" && text.contains("localStorage") {
            result.extend(storage_keys(&text)?);
        }
    }
    Ok(result)
}

fn storage_keys(text: &str) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    let args = captures(
        r"localStorage\.(?:getItem|setItem|removeItem)\(\s*([^,\s)]+)",
        text,
    );
    for arg in args {
        let name = if (arg.starts_with('"') && arg.ends_with('"'))
            || (arg.starts_with('\'') && arg.ends_with('\''))
        {
            arg[1..arg.len() - 1].to_owned()
        } else {
            let pattern = format!(r#"const\s+{}\s*=\s*["']([^"']+)["']"#, regex::escape(&arg));
            captures(&pattern, text)
                .into_iter()
                .next()
                .ok_or_else(|| format!("unclassified localStorage key expression {arg}"))?
        };
        result.insert(name);
    }
    Ok(result)
}

fn metadata(root: &Path, source: &str, target: &str, task: &str, test: &str) -> Result<(), String> {
    if !root.join(source).is_file() {
        return Err(format!("missing source {source}"));
    }
    if ![
        "core-app",
        "core-runtime",
        "render-wgpu",
        "ui-egui",
        "apps/darkroom",
    ]
    .contains(&target)
        || test.trim().is_empty()
        || !Regex::new(r"^DARKROOM-[1-9][0-9]*$")
            .unwrap()
            .is_match(task)
    {
        return Err(format!("invalid metadata for {source}"));
    }
    if !test.starts_with("coverage gap:") && !root.join(test).is_file() {
        return Err(format!("missing test reference {test}"));
    }
    Ok(())
}

fn validate(root: &Path, contract: &Contract) -> Result<(), String> {
    let names = unique(contract.commands.iter().map(|x| x.name.clone()), "command")?;
    compare(
        registered(&read(&root.join("src-tauri/src/lib.rs"))?)?,
        names,
        "commands",
    )?;
    for command in &contract.commands {
        metadata(
            root,
            &command.source,
            &command.target,
            &command.task,
            &command.test,
        )?;
        if command.behavior.trim().is_empty()
            || !read(&root.join(&command.source))?.contains(&format!("fn {}(", command.name))
        {
            return Err(format!(
                "missing command behavior or source: {}",
                command.name
            ));
        }
    }
    validate_surface(root, contract)
}

fn validate_surface(root: &Path, contract: &Contract) -> Result<(), String> {
    for item in &contract.surface {
        metadata(root, &item.source, &item.target, &item.task, &item.test)?;
        if item.behavior.trim().is_empty()
            || !read(&root.join(&item.source))?.contains(&item.evidence)
        {
            return Err(format!(
                "missing evidence for {}: {}",
                item.name, item.evidence
            ));
        }
        if !["event", "screen", "storage", "contract"].contains(&item.kind.as_str()) {
            return Err(format!("unknown surface kind {}", item.kind));
        }
    }
    for kind in ["event", "screen", "storage", "contract"] {
        let expected = unique(
            contract
                .surface
                .iter()
                .filter(|x| x.kind == kind)
                .map(|x| x.name.clone()),
            kind,
        )?;
        if kind == "contract" {
            continue;
        }
        let actual = if kind == "event" {
            events(root)?
        } else {
            frontend(root, kind)?
        };
        compare(actual, expected, kind)?;
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<(), String> {
    crate::source_index::check(root)?;
    let dir = root.join("tests/migration/parity");
    let mut paths = Vec::new();
    files(&dir, &["json"], &mut paths)?;
    paths.sort();
    if paths.is_empty() {
        return Err("no parity manifests".into());
    }
    let mut contract = Contract {
        commands: Vec::new(),
        surface: Vec::new(),
    };
    for path in paths {
        let part: Contract =
            serde_json::from_str(&read(&path)?).map_err(|e| format!("{}: {e}", path.display()))?;
        contract.commands.extend(part.commands);
        contract.surface.extend(part.surface);
    }
    validate(root, &contract)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handler_addition_and_removal_fail() {
        let base =
            registered("tauri::generate_handler![\n commands::a,\n commands::b,\n]").unwrap();
        let added =
            registered("tauri::generate_handler![\n commands::a,\n commands::b,\n commands::c,\n]")
                .unwrap();
        let removed = registered("tauri::generate_handler![\n commands::a,\n]").unwrap();
        assert!(compare(added, base.clone(), "commands").is_err());
        assert!(compare(removed, base, "commands").is_err());
    }
    #[test]
    fn screen_discovery_ignores_typescript_generics() {
        let root = std::env::temp_dir().join(format!("darkroom-screens-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/hook.ts"), "useRef<DerivedView | null>(null)").unwrap();
        fs::write(root.join("src/app.tsx"), "<LibraryView />").unwrap();
        assert_eq!(
            frontend(&root, "screen").unwrap(),
            BTreeSet::from(["LibraryView".to_owned()])
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn event_and_storage_drift_fail() {
        let root = std::env::temp_dir().join(format!("darkroom-parity-{}", std::process::id()));
        let native = root.join("src-tauri/src");
        let frontend_dir = root.join("src");
        fs::create_dir_all(&native).unwrap();
        fs::create_dir_all(&frontend_dir).unwrap();
        let event_file = native.join("events.rs");
        let storage_file = frontend_dir.join("storage.ts");
        fs::write(&event_file, "app.emit(\"existing:event\", ());").unwrap();
        fs::write(
            &storage_file,
            "localStorage.getItem(\"darkroom.existing\");",
        )
        .unwrap();
        let expected_events = events(&root).unwrap();
        let expected_storage = frontend(&root, "storage").unwrap();
        fs::write(&event_file, "app.emit(\"new:event\", ());").unwrap();
        fs::write(&storage_file, "localStorage.getItem(\"darkroom.new\");").unwrap();
        assert!(compare(events(&root).unwrap(), expected_events, "event").is_err());
        assert!(compare(
            frontend(&root, "storage").unwrap(),
            expected_storage,
            "storage"
        )
        .is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
