//! Shared action metadata for native menus, shortcuts, and controls.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionId {
    OpenCatalog,
    CloseCatalog,
    QueryLibrary,
    RefreshLibrary,
    SelectImage,
    LoadFolders,
    LoadDates,
    RequestShutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnabledRequirement {
    Always,
    Running,
    CatalogReady,
    HasSelection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnabledContext {
    pub running: bool,
    pub catalog_ready: bool,
    pub has_selection: bool,
}

impl EnabledRequirement {
    pub fn is_enabled(self, context: EnabledContext) -> bool {
        match self {
            Self::Always => true,
            Self::Running => context.running,
            Self::CatalogReady => context.running && context.catalog_ready,
            Self::HasSelection => context.running && context.catalog_ready && context.has_selection,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub shortcut: Option<&'static str>,
    pub menu: Option<&'static str>,
    pub route: ActionId,
    pub enabled: EnabledRequirement,
}

pub static ACTIONS: &[ActionDescriptor] = &[
    ActionDescriptor {
        id: "open_catalog",
        label: "Open Catalog",
        shortcut: Some("CmdOrCtrl+O"),
        menu: Some("File"),
        route: ActionId::OpenCatalog,
        enabled: EnabledRequirement::Running,
    },
    ActionDescriptor {
        id: "close_catalog",
        label: "Close Catalog",
        shortcut: Some("CmdOrCtrl+W"),
        menu: Some("File"),
        route: ActionId::CloseCatalog,
        enabled: EnabledRequirement::Running,
    },
    ActionDescriptor {
        id: "query_library",
        label: "Query Library",
        shortcut: None,
        menu: None,
        route: ActionId::QueryLibrary,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "refresh_library",
        label: "Refresh Library",
        shortcut: None,
        menu: Some("Library"),
        route: ActionId::RefreshLibrary,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "select_image",
        label: "Select Image",
        shortcut: None,
        menu: None,
        route: ActionId::SelectImage,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "load_folders",
        label: "Load Folders",
        shortcut: None,
        menu: None,
        route: ActionId::LoadFolders,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "load_dates",
        label: "Load Dates",
        shortcut: None,
        menu: None,
        route: ActionId::LoadDates,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "request_shutdown",
        label: "Quit",
        shortcut: Some("CmdOrCtrl+Q"),
        menu: Some("File"),
        route: ActionId::RequestShutdown,
        enabled: EnabledRequirement::Running,
    },
];

pub fn action(id: ActionId) -> &'static ActionDescriptor {
    ACTIONS
        .iter()
        .find(|descriptor| descriptor.route == id)
        .expect("every ActionId has a descriptor")
}
