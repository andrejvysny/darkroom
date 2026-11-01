use super::*;
use serde_json::{json, Value};

fn package(name: &str, version: &str, dependencies: Vec<Value>) -> Value {
    json!({"id": format!("{name}@{version}"), "name": name, "version": version,
        "manifest_path": format!("/repo/crates/{name}/Cargo.toml"), "source": null,
        "targets": [{"name": name.replace('-', "_"), "kind": ["lib"]}], "dependencies": dependencies})
}

fn declared(name: &str, alias: Option<&str>, kind: &str, target: Option<&str>) -> Value {
    json!({"name": name, "rename": alias, "kind": if kind == "normal" { None } else { Some(kind) }, "target": target, "source": null, "path": null, "req": "*", "optional": false})
}

fn node(name: &str, version: &str, deps: Vec<(&str, &str, &str)>) -> Value {
    json!({"id": format!("{name}@{version}"), "deps": deps.into_iter().map(|(alias, name, version)|
        json!({"name": alias, "pkg": format!("{name}@{version}"), "dep_kinds": [{"kind": null, "target": null}]})).collect::<Vec<_>>()})
}

fn metadata(packages: Vec<Value>, workspace: Vec<&str>, nodes: Vec<Value>) -> Metadata {
    serde_json::from_value(json!({"packages": packages, "workspace_members": workspace,
        "resolve": {"nodes": nodes}}))
    .unwrap()
}

#[test]
fn rejects_aliased_target_dev_dependency_transitively() {
    let mut data = metadata(
        vec![
            package(
                "core-app",
                "1.0.0",
                vec![declared(
                    "helper",
                    Some("alias"),
                    "dev",
                    Some("cfg(windows)"),
                )],
            ),
            package("helper", "1.0.0", vec![]),
            package("egui-wgpu", "1.0.0", vec![]),
        ],
        vec!["core-app@1.0.0"],
        vec![
            node("core-app", "1.0.0", vec![("alias", "helper", "1.0.0")]),
            node("helper", "1.0.0", vec![("egui_wgpu", "egui-wgpu", "1.0.0")]),
            node("egui-wgpu", "1.0.0", vec![]),
        ],
    );
    data.resolve.as_mut().unwrap().nodes[0].deps[0].dep_kinds = vec![DepKind {
        kind: Some("dev".into()),
        target: Some("cfg(windows)".into()),
    }];
    assert!(check(&data)
        .unwrap_err()
        .contains("core-app may not reach egui-wgpu"));
}

#[test]
fn rejects_ui_service_and_cycles() {
    let data = metadata(
        vec![
            package(
                "ui-egui",
                "1.0.0",
                vec![declared("core-library", None, "normal", None)],
            ),
            package(
                "core-library",
                "1.0.0",
                vec![declared("ui-egui", None, "normal", None)],
            ),
        ],
        vec!["ui-egui@1.0.0", "core-library@1.0.0"],
        vec![
            node(
                "ui-egui",
                "1.0.0",
                vec![("core_library", "core-library", "1.0.0")],
            ),
            node(
                "core-library",
                "1.0.0",
                vec![("ui_egui", "ui-egui", "1.0.0")],
            ),
        ],
    );
    let error = check(&data).unwrap_err();
    assert!(error.contains("dependency cycle"));
    assert!(error.contains("ui-egui may not reach core-library"));
}

#[test]
fn distinguishes_package_versions_and_rejects_incompatible_wgpu() {
    let data = metadata(
        vec![
            package(
                "core-pipeline",
                "1.0.0",
                vec![declared("wgpu", None, "normal", None)],
            ),
            package("wgpu", "29.0.0", vec![]),
            package("wgpu", "30.0.1", vec![]),
            package("bridge", "1.0.0", vec![]),
        ],
        vec!["core-pipeline@1.0.0"],
        vec![
            node(
                "core-pipeline",
                "1.0.0",
                vec![("wgpu", "wgpu", "29.0.0"), ("bridge", "bridge", "1.0.0")],
            ),
            node("bridge", "1.0.0", vec![("wgpu", "wgpu", "30.0.1")]),
            node("wgpu", "29.0.0", vec![]),
            node("wgpu", "30.0.1", vec![]),
        ],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("incompatible wgpu identities"));
}

#[test]
fn fails_closed_when_declared_dependency_missing_from_resolve() {
    let data = metadata(
        vec![package(
            "core-model",
            "1.0.0",
            vec![declared(
                "rfd",
                Some("dialog"),
                "build",
                Some("cfg(windows)"),
            )],
        )],
        vec!["core-model@1.0.0"],
        vec![node("core-model", "1.0.0", vec![])],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("unresolved declared dependency rfd"));
}

#[test]
fn native_executable_cannot_reach_tauri_transitively() {
    let mut app = package("darkroom", "1.0.0", vec![]);
    app["manifest_path"] = json!("/repo/apps/darkroom/Cargo.toml");
    let data = metadata(
        vec![
            app,
            package("bridge", "1.0.0", vec![]),
            package("tauri", "2.0.0", vec![]),
        ],
        vec!["darkroom@1.0.0"],
        vec![
            node("darkroom", "1.0.0", vec![("bridge", "bridge", "1.0.0")]),
            node("bridge", "1.0.0", vec![("tauri", "tauri", "2.0.0")]),
            node("tauri", "2.0.0", vec![]),
        ],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("darkroom may not reach tauri"));
}

#[test]
fn kind_and_target_must_match_each_declared_edge() {
    for kind in ["dev", "build"] {
        let data = metadata(
            vec![
                package(
                    "core-model",
                    "1.0.0",
                    vec![
                        declared("helper", None, "normal", None),
                        declared("helper", None, kind, Some("cfg(windows)")),
                    ],
                ),
                package("helper", "1.0.0", vec![]),
            ],
            vec!["core-model@1.0.0"],
            vec![
                node("core-model", "1.0.0", vec![("helper", "helper", "1.0.0")]),
                node("helper", "1.0.0", vec![]),
            ],
        );
        assert!(check(&data)
            .unwrap_err()
            .contains("unresolved declared dependency helper"));
    }
}

#[test]
fn rejects_wrong_version_and_source() {
    for field in ["req", "source", "path"] {
        let mut dep = declared("helper", None, "normal", None);
        dep[field] = match field {
            "req" => json!("^2"),
            "source" => json!("registry+https://example.com/index"),
            _ => json!("/another/helper"),
        };
        let data = metadata(
            vec![
                package("core-model", "1.0.0", vec![dep]),
                package("helper", "1.0.0", vec![]),
            ],
            vec!["core-model@1.0.0"],
            vec![
                node("core-model", "1.0.0", vec![("helper", "helper", "1.0.0")]),
                node("helper", "1.0.0", vec![]),
            ],
        );
        assert!(check(&data)
            .unwrap_err()
            .contains("unresolved declared dependency helper"));
    }
}

#[test]
fn unrenamed_package_uses_library_target_name() {
    let mut exif = package("kamadak-exif", "0.6.1", vec![]);
    exif["targets"][0]["name"] = json!("exif");
    let data = metadata(
        vec![
            package(
                "core-raw",
                "1.0.0",
                vec![declared("kamadak-exif", None, "normal", None)],
            ),
            exif,
        ],
        vec!["core-raw@1.0.0"],
        vec![
            node("core-raw", "1.0.0", vec![("exif", "kamadak-exif", "0.6.1")]),
            node("kamadak-exif", "0.6.1", vec![]),
        ],
    );
    assert!(check(&data).is_ok());
}

#[test]
fn shared_wgpu_identity_passes() {
    let mut app = package(
        "darkroom",
        "1.0.0",
        vec![declared("wgpu", None, "normal", None)],
    );
    app["manifest_path"] = json!("/repo/apps/darkroom/Cargo.toml");
    let data = metadata(
        vec![
            app,
            package(
                "core-pipeline",
                "1.0.0",
                vec![declared("wgpu", None, "normal", None)],
            ),
            package("wgpu", "30.0.1", vec![]),
        ],
        vec!["darkroom@1.0.0", "core-pipeline@1.0.0"],
        vec![
            node("darkroom", "1.0.0", vec![("wgpu", "wgpu", "30.0.1")]),
            node("core-pipeline", "1.0.0", vec![("wgpu", "wgpu", "30.0.1")]),
            node("wgpu", "30.0.1", vec![]),
        ],
    );
    assert!(check(&data).is_ok());
}

#[test]
fn same_major_wgpu_versions_cannot_create_distinct_device_types() {
    for other_version in ["30.0.0", "30.0.1+other-source"] {
        let mut data = metadata(
            vec![
                package("core-pipeline", "1.0.0", vec![]),
                package("wgpu", "30.0.1", vec![]),
                package("wgpu", other_version, vec![]),
            ],
            vec!["core-pipeline@1.0.0"],
            vec![
                node(
                    "core-pipeline",
                    "1.0.0",
                    vec![
                        ("first", "wgpu", "30.0.1"),
                        ("second", "wgpu", other_version),
                    ],
                ),
                node("wgpu", "30.0.1", vec![]),
                node("wgpu", other_version, vec![]),
            ],
        );
        if other_version.contains('+') {
            let id = "registry+https://other.example/index#wgpu@30.0.1";
            data.packages[2].id = id.into();
            data.packages[2].version = "30.0.1".into();
            data.packages[2].source = Some("registry+https://other.example/index".into());
            let nodes = &mut data.resolve.as_mut().unwrap().nodes;
            nodes[0].deps[1].pkg = id.into();
            nodes[2].id = id.into();
        }
        assert!(check(&data)
            .unwrap_err()
            .contains("incompatible wgpu identities"));
    }
}

#[test]
fn model_rejects_gpu_database_engine_and_gui_dependencies_transitively() {
    for forbidden in [
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "bytemuck",
        "core-raw",
        "core-db",
        "rusqlite",
        "libsqlite3-sys",
        "tauri",
        "egui",
    ] {
        let data = metadata(
            vec![
                package(
                    "core-model",
                    "1.0.0",
                    vec![declared("helper", Some("alias"), "normal", None)],
                ),
                package("helper", "1.0.0", vec![]),
                package(forbidden, "1.0.0", vec![]),
            ],
            vec!["core-model@1.0.0"],
            vec![
                node("core-model", "1.0.0", vec![("alias", "helper", "1.0.0")]),
                node("helper", "1.0.0", vec![(forbidden, forbidden, "1.0.0")]),
                node(forbidden, "1.0.0", vec![]),
            ],
        );
        assert!(check(&data)
            .unwrap_err()
            .contains(&format!("core-model may not reach {forbidden}")));
    }
}

#[test]
fn role_matrix_rejects_each_forbidden_edge() {
    // (from, forbidden target, from-role, target-role); roles resolve by package name,
    // so the default `/repo/crates/{name}/Cargo.toml` manifest is correct for these.
    for (from, to, from_role, to_role) in [
        ("core-raw", "core-app", "engine", "app"),
        ("core-raw", "core-runtime", "engine", "runtime"),
        ("core-library", "ui-egui", "engine", "ui"),
        ("render-wgpu", "core-app", "renderer", "app"),
        ("render-wgpu", "core-runtime", "renderer", "runtime"),
        ("render-wgpu", "egui", "renderer", "external"),
        ("render-wgpu", "rusqlite", "renderer", "external"),
        ("core-runtime", "render-wgpu", "runtime", "renderer"),
        ("core-runtime", "egui", "runtime", "external"),
        ("core-runtime", "wgpu", "runtime", "external"),
        ("ui-egui", "core-runtime", "ui", "runtime"),
        ("ui-egui", "core-library", "ui", "engine"),
        ("core-model", "core-app", "model", "app"),
        ("core-app", "core-runtime", "app", "runtime"),
    ] {
        let data = metadata(
            vec![
                package(from, "1.0.0", vec![declared(to, None, "normal", None)]),
                package(to, "1.0.0", vec![]),
            ],
            vec![&format!("{from}@1.0.0")[..]],
            vec![
                node(
                    from,
                    "1.0.0",
                    vec![(to.replace('-', "_").as_str(), to, "1.0.0")],
                ),
                node(to, "1.0.0", vec![]),
            ],
        );
        assert!(check(&data).unwrap_err().contains(&format!(
            "{from} may not reach {to} (1.0.0) [{from_role} -> {to_role}]"
        )));
    }
}

#[test]
fn engine_rejects_host_package() {
    let mut host = package("darkroom-native", "1.0.0", vec![]);
    host["manifest_path"] = json!("/repo/apps/darkroom/Cargo.toml");
    let data = metadata(
        vec![
            package(
                "core-import",
                "1.0.0",
                vec![declared("darkroom-native", None, "normal", None)],
            ),
            host,
        ],
        vec!["core-import@1.0.0"],
        vec![
            node(
                "core-import",
                "1.0.0",
                vec![("darkroom_native", "darkroom-native", "1.0.0")],
            ),
            node("darkroom-native", "1.0.0", vec![]),
        ],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("core-import may not reach darkroom-native (1.0.0) [engine -> host]"));
}

#[test]
fn role_matrix_rejects_transitive_engine_to_app() {
    let data = metadata(
        vec![
            package(
                "core-raw",
                "1.0.0",
                vec![declared("helper", None, "normal", None)],
            ),
            package(
                "helper",
                "1.0.0",
                vec![declared("core-app", None, "normal", None)],
            ),
            package("core-app", "1.0.0", vec![]),
        ],
        vec!["core-raw@1.0.0"],
        vec![
            node("core-raw", "1.0.0", vec![("helper", "helper", "1.0.0")]),
            node("helper", "1.0.0", vec![("core_app", "core-app", "1.0.0")]),
            node("core-app", "1.0.0", vec![]),
        ],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("core-raw may not reach core-app (1.0.0) [engine -> app]"));
}

#[test]
fn role_matrix_rejects_aliased_engine_to_app_dependency() {
    let data = metadata(
        vec![
            package(
                "core-raw",
                "1.0.0",
                vec![declared("core-app", Some("domain"), "normal", None)],
            ),
            package("core-app", "1.0.0", vec![]),
        ],
        vec!["core-raw@1.0.0"],
        vec![
            node("core-raw", "1.0.0", vec![("domain", "core-app", "1.0.0")]),
            node("core-app", "1.0.0", vec![]),
        ],
    );
    assert!(check(&data)
        .unwrap_err()
        .contains("core-raw may not reach core-app (1.0.0) [engine -> app]"));
}

#[test]
fn role_matrix_accepts_valid_layered_graph() {
    let mut host = package(
        "darkroom-native",
        "1.0.0",
        vec![
            declared("ui-egui", None, "normal", None),
            declared("core-app", None, "normal", None),
            declared("core-runtime", None, "normal", None),
            declared("core-raw", None, "normal", None),
            declared("core-pipeline", None, "normal", None),
            declared("render-wgpu", None, "normal", None),
            declared("wgpu", None, "normal", None),
            declared("egui", None, "normal", None),
        ],
    );
    host["manifest_path"] = json!("/repo/apps/darkroom/Cargo.toml");
    let data = metadata(
        vec![
            package(
                "ui-egui",
                "1.0.0",
                vec![declared("core-app", None, "normal", None)],
            ),
            package(
                "core-app",
                "1.0.0",
                vec![declared("core-model", None, "normal", None)],
            ),
            package("core-model", "1.0.0", vec![]),
            package(
                "core-runtime",
                "1.0.0",
                vec![
                    declared("core-app", None, "normal", None),
                    declared("core-raw", None, "normal", None),
                ],
            ),
            package("core-raw", "1.0.0", vec![]),
            package(
                "render-wgpu",
                "1.0.0",
                vec![
                    declared("core-pipeline", None, "normal", None),
                    declared("core-model", None, "normal", None),
                ],
            ),
            package(
                "core-pipeline",
                "1.0.0",
                vec![
                    declared("core-model", None, "normal", None),
                    declared("core-raw", None, "normal", None),
                    declared("wgpu", None, "normal", None),
                ],
            ),
            package("wgpu", "30.0.1", vec![]),
            package("egui", "0.37.0", vec![]),
            host,
        ],
        vec![
            "ui-egui@1.0.0",
            "core-app@1.0.0",
            "core-model@1.0.0",
            "core-runtime@1.0.0",
            "core-raw@1.0.0",
            "render-wgpu@1.0.0",
            "core-pipeline@1.0.0",
            "darkroom-native@1.0.0",
        ],
        vec![
            node("ui-egui", "1.0.0", vec![("core_app", "core-app", "1.0.0")]),
            node(
                "core-app",
                "1.0.0",
                vec![("core_model", "core-model", "1.0.0")],
            ),
            node("core-model", "1.0.0", vec![]),
            node(
                "core-runtime",
                "1.0.0",
                vec![
                    ("core_app", "core-app", "1.0.0"),
                    ("core_raw", "core-raw", "1.0.0"),
                ],
            ),
            node("core-raw", "1.0.0", vec![]),
            node(
                "render-wgpu",
                "1.0.0",
                vec![
                    ("core_pipeline", "core-pipeline", "1.0.0"),
                    ("core_model", "core-model", "1.0.0"),
                ],
            ),
            node(
                "core-pipeline",
                "1.0.0",
                vec![
                    ("core_model", "core-model", "1.0.0"),
                    ("core_raw", "core-raw", "1.0.0"),
                    ("wgpu", "wgpu", "30.0.1"),
                ],
            ),
            node("wgpu", "30.0.1", vec![]),
            node("egui", "0.37.0", vec![]),
            node(
                "darkroom-native",
                "1.0.0",
                vec![
                    ("ui_egui", "ui-egui", "1.0.0"),
                    ("core_app", "core-app", "1.0.0"),
                    ("core_runtime", "core-runtime", "1.0.0"),
                    ("core_raw", "core-raw", "1.0.0"),
                    ("core_pipeline", "core-pipeline", "1.0.0"),
                    ("render_wgpu", "render-wgpu", "1.0.0"),
                    ("wgpu", "wgpu", "30.0.1"),
                    ("egui", "egui", "0.37.0"),
                ],
            ),
        ],
    );
    assert!(check(&data).is_ok());
}

#[test]
fn application_rejects_gpu_database_engine_and_gui_dependencies_transitively() {
    for forbidden in [
        "wgpu",
        "wgpu-core",
        "wgpu-hal",
        "bytemuck",
        "core-raw",
        "core-db",
        "rusqlite",
        "libsqlite3-sys",
        "tauri",
        "egui",
    ] {
        let data = metadata(
            vec![
                package(
                    "core-app",
                    "1.0.0",
                    vec![declared("helper", Some("alias"), "normal", None)],
                ),
                package("helper", "1.0.0", vec![]),
                package(forbidden, "1.0.0", vec![]),
            ],
            vec!["core-app@1.0.0"],
            vec![
                node("core-app", "1.0.0", vec![("alias", "helper", "1.0.0")]),
                node("helper", "1.0.0", vec![(forbidden, forbidden, "1.0.0")]),
                node(forbidden, "1.0.0", vec![]),
            ],
        );
        assert!(check(&data)
            .unwrap_err()
            .contains(&format!("core-app may not reach {forbidden}")));
    }
}

#[test]
fn ui_rejects_gpu_windowing_database_and_service_dependencies_transitively() {
    for forbidden in [
        "core-runtime",
        "core-db",
        "core-library",
        "core-raw",
        "core-pipeline",
        "render-wgpu",
        "wgpu",
        "winit",
        "egui-wgpu",
        "egui-winit",
        "eframe",
        "rfd",
        "rusqlite",
        "tauri",
    ] {
        let data = metadata(
            vec![
                package(
                    "ui-egui",
                    "1.0.0",
                    vec![declared("helper", Some("alias"), "normal", None)],
                ),
                package("helper", "1.0.0", vec![]),
                package(forbidden, "1.0.0", vec![]),
            ],
            vec!["ui-egui@1.0.0"],
            vec![
                node("ui-egui", "1.0.0", vec![("alias", "helper", "1.0.0")]),
                node("helper", "1.0.0", vec![(forbidden, forbidden, "1.0.0")]),
                node(forbidden, "1.0.0", vec![]),
            ],
        );
        assert!(
            check(&data)
                .unwrap_err()
                .contains(&format!("ui-egui may not reach {forbidden}")),
            "{forbidden}"
        );
    }
}

#[test]
fn ui_accepts_egui_core_app_and_core_model_only() {
    let data = metadata(
        vec![
            package(
                "ui-egui",
                "1.0.0",
                vec![
                    declared("egui", None, "normal", None),
                    declared("core-app", None, "normal", None),
                    declared("core-model", None, "normal", None),
                ],
            ),
            package(
                "core-app",
                "1.0.0",
                vec![declared("core-model", None, "normal", None)],
            ),
            package("core-model", "1.0.0", vec![]),
            package("egui", "0.37.0", vec![]),
        ],
        vec!["ui-egui@1.0.0"],
        vec![
            node(
                "ui-egui",
                "1.0.0",
                vec![
                    ("egui", "egui", "0.37.0"),
                    ("core_app", "core-app", "1.0.0"),
                    ("core_model", "core-model", "1.0.0"),
                ],
            ),
            node(
                "core-app",
                "1.0.0",
                vec![("core_model", "core-model", "1.0.0")],
            ),
            node("core-model", "1.0.0", vec![]),
            node("egui", "0.37.0", vec![]),
        ],
    );
    check(&data).unwrap();
}

#[test]
fn real_workspace_passes_layer_rules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    run(root).unwrap();
}
