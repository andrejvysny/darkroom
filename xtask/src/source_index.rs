//! The source fence makes syntax outside the targeted scanners require explicit parity review.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Deserialize, Serialize)]
struct SourceIndex {
    sources: BTreeMap<String, String>,
}

fn collect(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("rs" | "ts" | "tsx")
        ) {
            let relative = path.strip_prefix(root).map_err(|error| error.to_string())?;
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            files.insert(
                relative.to_string_lossy().replace('\\', "/"),
                format!("{:x}", Sha256::digest(bytes)),
            );
        }
    }
    Ok(())
}

fn snapshot(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut files = BTreeMap::new();
    for directory in ["src", "src-tauri/src"] {
        collect(root, &root.join(directory), &mut files)?;
    }
    Ok(files)
}

fn validate(
    actual: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> Result<(), String> {
    let changed: Vec<_> = actual
        .keys()
        .chain(expected.keys())
        .filter(|path| actual.get(*path) != expected.get(*path))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    Err(format!("legacy sources changed: {changed:?}; review command/event/UI parity before updating tests/migration/source-index.json"))
}

pub fn check(root: &Path) -> Result<(), String> {
    let path = root.join("tests/migration/source-index.json");
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let index: SourceIndex = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    validate(&snapshot(root)?, &index.sources)
}

pub fn print(root: &Path) -> Result<(), String> {
    let index = SourceIndex {
        sources: snapshot(root)?,
    };
    let json = serde_json::to_string_pretty(&index).map_err(|error| error.to_string())?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognized_syntax_and_source_addition_or_removal_require_review() {
        let root =
            std::env::temp_dir().join(format!("darkroom-source-fence-{}", std::process::id()));
        fs::create_dir_all(root.join("src-tauri/src")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/app.tsx");
        fs::write(&file, "localStorage.getItem('old');").unwrap();
        let expected = snapshot(&root).unwrap();
        assert!(validate(&snapshot(&root).unwrap(), &expected).is_ok());
        fs::write(&file, "localStorage[key] = 'new';").unwrap();
        assert!(validate(&snapshot(&root).unwrap(), &expected).is_err());
        fs::write(&file, "localStorage.getItem('old');").unwrap();
        fs::write(root.join("src/UnusualOverlay.tsx"), "<UnusualOverlay />").unwrap();
        assert!(validate(&snapshot(&root).unwrap(), &expected).is_err());
        fs::remove_file(file).unwrap();
        assert!(validate(&snapshot(&root).unwrap(), &expected).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
