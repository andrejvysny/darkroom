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
    Undo,
    Redo,
    /// Develop edits (discrete and gesture) and their saves.
    Edit,
    DiscardUnsavedEdits,
    SetCull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnabledRequirement {
    Always,
    Running,
    CatalogReady,
    HasSelection,
    /// A Ready document whose image row (hence save identity) is loaded.
    Editable,
    CanUndo,
    CanRedo,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnabledContext {
    pub running: bool,
    pub catalog_ready: bool,
    pub has_selection: bool,
    pub editable: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

impl EnabledRequirement {
    pub fn is_enabled(self, context: EnabledContext) -> bool {
        match self {
            Self::Always => true,
            Self::Running => context.running,
            Self::CatalogReady => context.running && context.catalog_ready,
            Self::HasSelection => context.running && context.catalog_ready && context.has_selection,
            Self::Editable => Self::HasSelection.is_enabled(context) && context.editable,
            Self::CanUndo => Self::Editable.is_enabled(context) && context.can_undo,
            Self::CanRedo => Self::Editable.is_enabled(context) && context.can_redo,
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
    ActionDescriptor {
        id: "undo",
        label: "Undo",
        shortcut: Some("CmdOrCtrl+Z"),
        menu: Some("Edit"),
        route: ActionId::Undo,
        enabled: EnabledRequirement::CanUndo,
    },
    ActionDescriptor {
        id: "redo",
        label: "Redo",
        shortcut: Some("CmdOrCtrl+Shift+Z"),
        menu: Some("Edit"),
        route: ActionId::Redo,
        enabled: EnabledRequirement::CanRedo,
    },
    ActionDescriptor {
        id: "edit",
        label: "Edit",
        shortcut: None,
        menu: None,
        route: ActionId::Edit,
        enabled: EnabledRequirement::Editable,
    },
    ActionDescriptor {
        id: "discard_unsaved_edits",
        label: "Discard Unsaved Edits",
        shortcut: None,
        menu: None,
        route: ActionId::DiscardUnsavedEdits,
        enabled: EnabledRequirement::CatalogReady,
    },
    ActionDescriptor {
        id: "set_cull",
        label: "Rate / flag",
        shortcut: None,
        menu: None,
        route: ActionId::SetCull,
        enabled: EnabledRequirement::CatalogReady,
    },
];

pub fn action(id: ActionId) -> &'static ActionDescriptor {
    ACTIONS
        .iter()
        .find(|descriptor| descriptor.route == id)
        .expect("every ActionId has a descriptor")
}
