use core_app::registry::{action, ActionId, EnabledContext, EnabledRequirement, ACTIONS};
use std::collections::HashSet;

#[test]
fn descriptors_have_unique_ids_routes_and_shortcuts() {
    let mut ids = HashSet::new();
    let mut routes = HashSet::new();
    let mut shortcuts = HashSet::new();
    for descriptor in ACTIONS {
        assert!(ids.insert(descriptor.id));
        assert!(routes.insert(descriptor.route));
        if let Some(shortcut) = descriptor.shortcut {
            assert!(shortcuts.insert(shortcut));
        }
        assert_eq!(action(descriptor.route), descriptor);
    }
    assert_eq!(ACTIONS.len(), 18);
}

#[test]
fn enabled_requirements_follow_concrete_application_state() {
    let stopped = EnabledContext::default();
    let closed = EnabledContext {
        running: true,
        ..stopped
    };
    let open = EnabledContext {
        catalog_ready: true,
        ..closed
    };
    let selected = EnabledContext {
        has_selection: true,
        ..open
    };
    assert!(EnabledRequirement::Always.is_enabled(stopped));
    assert!(!EnabledRequirement::Running.is_enabled(stopped));
    assert!(EnabledRequirement::Running.is_enabled(closed));
    assert!(!EnabledRequirement::CatalogReady.is_enabled(closed));
    assert!(EnabledRequirement::CatalogReady.is_enabled(open));
    assert!(!EnabledRequirement::HasSelection.is_enabled(open));
    assert!(EnabledRequirement::HasSelection.is_enabled(selected));
    for id in [
        ActionId::QueryLibrary,
        ActionId::RefreshLibrary,
        ActionId::SelectImage,
        ActionId::LoadFolders,
        ActionId::LoadDates,
    ] {
        assert!(!action(id).enabled.is_enabled(closed));
        assert!(action(id).enabled.is_enabled(open));
    }
    for id in [
        ActionId::OpenCatalog,
        ActionId::CloseCatalog,
        ActionId::RequestShutdown,
    ] {
        assert!(!action(id).enabled.is_enabled(stopped));
        assert!(action(id).enabled.is_enabled(closed));
    }
}
