use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

type Version = (u32, u32, u32);

#[derive(Default)]
struct Loads {
    minimum: Option<Version>,
    macos: bool,
    rpaths: Vec<String>,
    install_id: Option<String>,
}

fn version(value: &str) -> Result<Version, String> {
    let numbers = value
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid OS version: {value}"))?;
    match numbers.as_slice() {
        [major, minor] => Ok((*major, *minor, 0)),
        [major, minor, patch] => Ok((*major, *minor, *patch)),
        _ => Err(format!("invalid OS version: {value}")),
    }
}

fn loads(text: &str) -> Result<Loads, String> {
    let mut result = Loads::default();
    let mut command = "";
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("cmd ") {
            command = value;
            result.macos |= command == "LC_VERSION_MIN_MACOSX";
        } else if command == "LC_BUILD_VERSION" && line.starts_with("platform ") {
            result.macos |= matches!(line.split_whitespace().last(), Some("1" | "MACOS"));
        } else if (command == "LC_BUILD_VERSION" && line.starts_with("minos "))
            || (command == "LC_VERSION_MIN_MACOSX" && line.starts_with("version "))
        {
            let minimum = version(line.split_whitespace().last().ok_or("missing OS version")?)?;
            result.minimum = Some(result.minimum.map_or(minimum, |old| old.max(minimum)));
        } else if command == "LC_RPATH" && line.starts_with("path ") {
            result.rpaths.push(load_name(line, "path "));
        } else if command == "LC_ID_DYLIB" && line.starts_with("name ") {
            result.install_id = Some(load_name(line, "name "));
        }
    }
    if !result.macos || result.minimum.is_none() {
        return Err("missing macOS deployment target in Mach-O load commands".into());
    }
    Ok(result)
}

fn load_name(line: &str, prefix: &str) -> String {
    line.trim_start_matches(prefix)
        .split(" (offset ")
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn dependencies(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with('\t'))
        .map(|line| {
            line.trim()
                .split(" (compatibility version ")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

fn tool(program: &str, args: &[&str], path: &Path) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .arg(path)
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn expand(value: &str, loader: &Path, executable: &Path) -> Option<PathBuf> {
    if value == "@loader_path" {
        Some(loader.to_owned())
    } else if value == "@executable_path" {
        Some(executable.to_owned())
    } else if let Some(suffix) = value.strip_prefix("@loader_path/") {
        Some(loader.join(suffix))
    } else if let Some(suffix) = value.strip_prefix("@executable_path/") {
        Some(executable.join(suffix))
    } else if Path::new(value).is_absolute() {
        Some(PathBuf::from(value))
    } else {
        None
    }
}

fn resolve(
    name: &str,
    loader: &Path,
    executable: &Path,
    rpaths: &[PathBuf],
    search: &[PathBuf],
) -> Result<PathBuf, String> {
    let mut candidates: Vec<_> = Path::new(name)
        .file_name()
        .map(|leaf| search.iter().map(|path| path.join(leaf)).collect())
        .unwrap_or_default();
    if let Some(suffix) = name.strip_prefix("@rpath/") {
        candidates.extend(rpaths.iter().map(|path| path.join(suffix)));
    } else {
        candidates.extend(expand(name, loader, executable));
    }
    candidates
        .iter()
        .find_map(|path| path.canonicalize().ok().filter(|p| p.is_file()))
        .ok_or_else(|| format!("unresolved dependency {name} from {}", loader.display()))
}

fn inspect(path: &Path, arch: &str, maximum: Version, report: bool) -> Result<Loads, String> {
    let architectures = tool("lipo", &["-archs"], path)?;
    if !architectures.split_whitespace().any(|value| value == arch) {
        return Err(format!("{} has no {arch} slice", path.display()));
    }
    let info = loads(&tool("otool", &["-arch", arch, "-l"], path)?)?;
    let minimum = info.minimum.ok_or("missing macOS minimum")?;
    if minimum > maximum {
        return Err(format!(
            "{} ({arch}) requires macOS {}.{}.{}, exceeds {}.{}.{}",
            path.display(),
            minimum.0,
            minimum.1,
            minimum.2,
            maximum.0,
            maximum.1,
            maximum.2
        ));
    }
    if report {
        println!(
            "{}: {arch}, macOS {}.{}.{}",
            path.display(),
            minimum.0,
            minimum.1,
            minimum.2
        );
    }
    Ok(info)
}

fn walk(binary: &Path, arch: &str, maximum: &str, report: bool) -> Result<Vec<PathBuf>, String> {
    if !matches!(arch, "arm64" | "x86_64") {
        return Err("architecture must be arm64 or x86_64".into());
    }
    let maximum = version(maximum)?;
    let binary = binary.canonicalize().map_err(|error| error.to_string())?;
    let executable = binary.parent().ok_or("binary has no parent")?;
    let search: Vec<_> = std::env::var_os("DYLD_LIBRARY_PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    let mut queue = vec![(binary.clone(), Vec::new())];
    let mut seen = HashSet::new();
    while let Some((path, inherited)) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let info = inspect(&path, arch, maximum, report)?;
        let loader = path.parent().ok_or("dependency has no parent")?;
        let mut rpaths: Vec<_> = info
            .rpaths
            .iter()
            .map(|value| {
                expand(value, loader, executable)
                    .ok_or_else(|| format!("unsupported rpath: {value}"))
            })
            .collect::<Result<_, _>>()?;
        rpaths.extend(inherited);
        for name in dependencies(&tool("otool", &["-arch", arch, "-L"], &path)?) {
            if name.starts_with("/usr/lib/")
                || name.starts_with("/System/Library/")
                || info.install_id.as_deref() == Some(&name)
            {
                continue;
            }
            queue.push((
                resolve(&name, loader, executable, &rpaths, &search)?,
                rpaths.clone(),
            ));
        }
    }
    let mut paths: Vec<_> = seen.into_iter().collect();
    paths.sort();
    Ok(paths)
}

pub fn check(binary: &Path, arch: &str, maximum: &str) -> Result<(), String> {
    let paths = walk(binary, arch, maximum, true)?;
    println!("macOS deployment closure passes: {} Mach-O files; runtime/API availability still requires OS testing", paths.len());
    Ok(())
}

pub fn files(binary: &Path, arch: &str) -> Result<(), String> {
    let paths = walk(binary, arch, "12.0", false)?;
    let paths = paths
        .iter()
        .map(|path| {
            let text = path.to_str().ok_or("non-UTF8 deployment path")?;
            if text.contains(['\n', '\r']) {
                return Err("newline in deployment path");
            }
            Ok(text)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for path in paths {
        println!("{path}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_deployment_versions_without_lexicographic_ordering() {
        assert!(version("12.0").unwrap() < version("15.0").unwrap());
        assert!(version("12.0.1").unwrap() > version("12.0").unwrap());
        assert!(version("unknown").is_err());
        assert!(version("12.0.0.1").is_err());
    }

    #[test]
    fn distinguishes_macos_and_missing_or_other_platform_metadata() {
        assert!(loads("cmd LC_BUILD_VERSION\nplatform 2\nminos 12.0").is_err());
        assert!(loads("cmd LC_BUILD_VERSION\nplatform 1").is_err());
        assert_eq!(
            loads("cmd LC_VERSION_MIN_MACOSX\nversion 10.13")
                .unwrap()
                .minimum,
            Some((10, 13, 0))
        );
        assert_eq!(
            loads("cmd LC_BUILD_VERSION\nplatform 1\nminos 26.0")
                .unwrap()
                .minimum,
            Some((26, 0, 0))
        );
    }

    #[test]
    fn retains_paths_with_spaces_and_install_identity() {
        assert_eq!(
            expand("@loader_path", Path::new("/lib"), Path::new("/bin")),
            Some(PathBuf::from("/lib"))
        );
        let info = loads("cmd LC_BUILD_VERSION\nplatform 1\nminos 12.0\ncmd LC_RPATH\npath @loader_path/My Libs (offset 12)\ncmd LC_ID_DYLIB\nname @rpath/libheif.1.dylib (offset 24)").unwrap();
        assert_eq!(info.rpaths, ["@loader_path/My Libs"]);
        assert_eq!(info.install_id.as_deref(), Some("@rpath/libheif.1.dylib"));
        assert_eq!(dependencies("/tmp/executable:\n\t/tmp/My Libs/libheif.1.dylib (compatibility version 1.0.0, current version 1.23.5)"), ["/tmp/My Libs/libheif.1.dylib"]);
    }

    #[test]
    fn dyld_override_applies_to_absolute_loader_and_rpath_names() {
        let root = std::env::temp_dir().join(format!(
            "darkroom-dyld-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = root.join("original");
        let replacement = root.join("replacement");
        std::fs::create_dir_all(&original).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(original.join("libcodec.dylib"), []).unwrap();
        std::fs::write(replacement.join("libcodec.dylib"), []).unwrap();
        for name in [
            original.join("libcodec.dylib").to_str().unwrap(),
            "@loader_path/libcodec.dylib",
            "@rpath/libcodec.dylib",
        ] {
            assert_eq!(
                resolve(
                    name,
                    &original,
                    &root,
                    std::slice::from_ref(&original),
                    std::slice::from_ref(&replacement)
                )
                .unwrap(),
                replacement.join("libcodec.dylib").canonicalize().unwrap()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
