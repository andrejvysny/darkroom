use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
    resolve: Option<Resolve>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    manifest_path: String,
    source: Option<String>,
    targets: Vec<Target>,
    dependencies: Vec<Declared>,
}

#[derive(Deserialize)]
struct Declared {
    name: String,
    rename: Option<String>,
    req: String,
    kind: Option<String>,
    target: Option<String>,
    source: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id: String,
    deps: Vec<Resolved>,
}

#[derive(Deserialize)]
struct Resolved {
    name: String,
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Deserialize)]
struct DepKind {
    kind: Option<String>,
    target: Option<String>,
}

fn source_matches(declared: &Declared, package: &Package) -> bool {
    if let Some(path) = &declared.path {
        return package.source.is_none()
            && Path::new(&package.manifest_path).parent() == Some(Path::new(path));
    }
    match (&declared.source, &package.source) {
        (Some(a), Some(b)) if a.starts_with("git+") => a.split('#').next() == b.split('#').next(),
        (a, b) => a == b,
    }
}

fn matches_edge(
    declared: &Declared,
    edge: &Resolved,
    package: &Package,
    req: &semver::VersionReq,
) -> bool {
    let alias = declared.rename.as_deref().map(|s| s.replace('-', "_"));
    let name_matches = match alias {
        Some(alias) => edge.name.replace('-', "_") == alias,
        None => package.targets.iter().any(|target| {
            target
                .kind
                .iter()
                .any(|kind| matches!(kind.as_str(), "lib" | "proc-macro"))
                && target.name.replace('-', "_") == edge.name.replace('-', "_")
        }),
    };
    package.name == declared.name
        && name_matches
        && source_matches(declared, package)
        && semver::Version::parse(&package.version).is_ok_and(|version| req.matches(&version))
        && edge
            .dep_kinds
            .iter()
            .any(|kind| kind.kind == declared.kind && kind.target == declared.target)
}

struct Graph<'a> {
    packages: HashMap<&'a str, &'a Package>,
    edges: HashMap<&'a str, Vec<&'a str>>,
    workspace: HashSet<&'a str>,
}

impl<'a> Graph<'a> {
    fn new(metadata: &'a Metadata) -> Result<Self, String> {
        let packages: HashMap<_, _> = metadata
            .packages
            .iter()
            .map(|p| (p.id.as_str(), p))
            .collect();
        let resolve = metadata
            .resolve
            .as_ref()
            .ok_or("cargo metadata has no resolve graph")?;
        let mut edges = HashMap::new();
        for node in &resolve.nodes {
            let package = packages
                .get(node.id.as_str())
                .ok_or("unknown resolve node")?;
            if metadata.workspace_members.contains(&node.id) {
                check_declared(package, node, &packages)?;
            }
            let mut dependencies = Vec::new();
            for dep in &node.deps {
                if !packages.contains_key(dep.pkg.as_str()) {
                    return Err(format!(
                        "{}: unknown resolved dependency {}",
                        package.name, dep.pkg
                    ));
                }
                dependencies.push(dep.pkg.as_str());
            }
            edges.insert(node.id.as_str(), dependencies);
        }
        Ok(Self {
            packages,
            edges,
            workspace: metadata
                .workspace_members
                .iter()
                .map(String::as_str)
                .collect(),
        })
    }

    fn reachable(&self, start: &'a str) -> HashSet<&'a str> {
        let mut seen = HashSet::new();
        let mut pending = vec![start];
        while let Some(id) = pending.pop() {
            if seen.insert(id) {
                pending.extend(self.edges.get(id).into_iter().flatten().copied());
            }
        }
        seen
    }

    fn cycle(
        &self,
        id: &'a str,
        active: &mut HashSet<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> bool {
        if active.contains(id) {
            return true;
        }
        if done.contains(id) {
            return false;
        }
        active.insert(id);
        for dep in self.edges.get(id).into_iter().flatten() {
            if self.cycle(dep, active, done) {
                return true;
            }
        }
        active.remove(id);
        done.insert(id);
        false
    }
}

fn gui(name: &str) -> bool {
    matches!(name, "tauri" | "egui" | "eframe" | "winit" | "rfd")
        || name.starts_with("tauri-")
        || name.starts_with("egui-")
}

fn tauri(name: &str) -> bool {
    name == "tauri" || name.starts_with("tauri-")
}

fn sql(name: &str) -> bool {
    matches!(
        name,
        "rusqlite" | "sqlx" | "diesel" | "sqlite" | "libsqlite3-sys"
    ) || name.starts_with("sqlx-")
        || name.starts_with("diesel-")
}

fn wgpu_family(name: &str) -> bool {
    matches!(
        name,
        "wgpu" | "wgpu-core" | "wgpu-hal" | "wgpu-types" | "naga"
    )
}

/// Architectural role of a package, driving the forbidden-reachability matrix in [`check`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Model,
    App,
    Runtime,
    Engine,
    Renderer,
    Ui,
    Host,
    External,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Role::Model => "model",
            Role::App => "app",
            Role::Runtime => "runtime",
            Role::Engine => "engine",
            Role::Renderer => "renderer",
            Role::Ui => "ui",
            Role::Host => "host",
            Role::External => "external",
        }
    }
}

fn role(package_name: &str, manifest_path: &str) -> Role {
    let manifest = manifest_path.replace('\\', "/");
    match package_name {
        "core-model" => Role::Model,
        "core-app" => Role::App,
        "core-runtime" => Role::Runtime,
        // Crate does not exist yet; the rule is enforced the day `render-wgpu` lands.
        "render-wgpu" => Role::Renderer,
        "ui-egui" => Role::Ui,
        "darkroom" | "darkroom-native" => Role::Host,
        // The layer checker itself is exempt from the matrix.
        "xtask" => Role::External,
        _ => {
            if manifest.ends_with("/apps/darkroom/Cargo.toml")
                || manifest.ends_with("/src-tauri/Cargo.toml")
            {
                Role::Host
            } else if package_name.starts_with("core-") {
                Role::Engine
            } else {
                Role::External
            }
        }
    }
}

/// Whether `from` (role of the source package, `from_native_host` true for the
/// `apps/darkroom` host but not the legacy `src-tauri` host) may not reach a
/// package of role `to` named `to_name`.
fn forbidden(from: Role, to: Role, from_native_host: bool, to_name: &str) -> bool {
    match from {
        Role::Model => {
            matches!(
                to,
                Role::App | Role::Runtime | Role::Engine | Role::Renderer | Role::Ui | Role::Host
            ) || gui(to_name)
                || sql(to_name)
                || wgpu_family(to_name)
                || to_name == "bytemuck"
        }
        Role::App => {
            matches!(
                to,
                Role::Runtime | Role::Engine | Role::Renderer | Role::Ui | Role::Host
            ) || gui(to_name)
                || sql(to_name)
                || wgpu_family(to_name)
                || to_name == "bytemuck"
        }
        Role::Engine => {
            matches!(
                to,
                Role::App | Role::Runtime | Role::Renderer | Role::Ui | Role::Host
            ) || gui(to_name)
        }
        Role::Renderer => {
            matches!(to, Role::App | Role::Runtime | Role::Ui | Role::Host)
                || gui(to_name)
                || sql(to_name)
                || tauri(to_name)
        }
        Role::Runtime => {
            matches!(to, Role::Renderer | Role::Ui | Role::Host)
                || gui(to_name)
                || wgpu_family(to_name)
        }
        // Ui may use egui and Model/App only: no GPU, windowing, database, dialogs or tauri.
        Role::Ui => {
            matches!(
                to,
                Role::Runtime | Role::Engine | Role::Renderer | Role::Host
            ) || sql(to_name)
                || tauri(to_name)
                || wgpu_family(to_name)
                || matches!(to_name, "winit" | "eframe" | "rfd")
                || to_name.starts_with("egui-")
        }
        // apps/darkroom is the native executable: no tauri. src-tauri is the legacy host: no new rules.
        Role::Host => from_native_host && tauri(to_name),
        Role::External => false,
    }
}

fn check(metadata: &Metadata) -> Result<(), String> {
    let graph = Graph::new(metadata)?;
    let mut errors = Vec::new();
    let mut done = HashSet::new();
    for id in &graph.workspace {
        if graph.cycle(id, &mut HashSet::new(), &mut done) {
            errors.push(format!(
                "dependency cycle reachable from {}",
                graph.packages[id].name
            ));
        }
    }
    let mut wgpu = HashSet::new();
    for id in &graph.workspace {
        let package = graph.packages[id];
        let native = package
            .manifest_path
            .replace('\\', "/")
            .ends_with("/apps/darkroom/Cargo.toml");
        let from = role(&package.name, &package.manifest_path);
        for dep in graph.reachable(id) {
            if dep == *id {
                continue;
            }
            let target = graph.packages[dep];
            let to = role(&target.name, &target.manifest_path);
            if forbidden(from, to, native, &target.name) {
                errors.push(format!(
                    "{} may not reach {} ({}) [{} -> {}]",
                    package.name,
                    target.name,
                    target.version,
                    from.label(),
                    to.label()
                ));
            }
            if (native || package.name == "core-pipeline" || from == Role::Renderer)
                && target.name == "wgpu"
            {
                wgpu.insert(target.id.as_str());
            }
        }
    }
    if wgpu.len() > 1 {
        errors.push(format!("incompatible wgpu identities: {wgpu:?}"));
    }
    errors.sort();
    errors.dedup();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

pub fn run(root: &Path) -> Result<(), String> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Metadata = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid cargo metadata: {e}"))?;
    check(&metadata)?;
    println!("dependency boundaries pass (metadata cannot enforce direct std filesystem access)");
    Ok(())
}

#[cfg(test)]
mod tests;

fn check_declared(
    package: &Package,
    node: &Node,
    packages: &HashMap<&str, &Package>,
) -> Result<(), String> {
    for declared in &package.dependencies {
        let requirement = semver::VersionReq::parse(&declared.req)
            .map_err(|e| format!("invalid requirement {}: {e}", declared.req))?;
        if !node.deps.iter().any(|edge| {
            packages
                .get(edge.pkg.as_str())
                .is_some_and(|target| matches_edge(declared, edge, target, &requirement))
        }) {
            return Err(format!(
                "{}: unresolved declared dependency {} ({:?}, {:?}, {})",
                package.name, declared.name, declared.kind, declared.target, declared.req
            ));
        }
    }
    Ok(())
}
