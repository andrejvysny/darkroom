use crate::registry::ActionId;
use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use core_model::identity::{CatalogEpoch, EditorSession, ImageId, SelectionGeneration};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    OpenCatalog(PathBuf),
    CloseCatalog,
    QueryLibrary(Box<QueryParams>),
    RefreshLibrary,
    SelectImage(Option<ImageId>),
    LoadFolders,
    LoadDates,
    RequestShutdown,
}
impl Command {
    pub fn action(&self) -> ActionId {
        match self {
            Self::OpenCatalog(_) => ActionId::OpenCatalog,
            Self::CloseCatalog => ActionId::CloseCatalog,
            Self::QueryLibrary(_) => ActionId::QueryLibrary,
            Self::RefreshLibrary => ActionId::RefreshLibrary,
            Self::SelectImage(_) => ActionId::SelectImage,
            Self::LoadFolders => ActionId::LoadFolders,
            Self::LoadDates => ActionId::LoadDates,
            Self::RequestShutdown => ActionId::RequestShutdown,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestToken {
    pub catalog_epoch: CatalogEpoch,
    pub request_id: RequestId,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub token: RequestToken,
    pub operation: Operation,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    OpenCatalog(PathBuf),
    CloseCatalog,
    QueryLibrary(Box<QueryParams>),
    LoadFolders,
    LoadDates,
    SelectImage {
        image_id: ImageId,
        editor_session: EditorSession,
        selection_generation: SelectionGeneration,
    },
}
impl Operation {
    pub fn action(&self) -> ActionId {
        match self {
            Self::OpenCatalog(_) => ActionId::OpenCatalog,
            Self::CloseCatalog => ActionId::CloseCatalog,
            Self::QueryLibrary(_) => ActionId::QueryLibrary,
            Self::LoadFolders => ActionId::LoadFolders,
            Self::LoadDates => ActionId::LoadDates,
            Self::SelectImage { .. } => ActionId::SelectImage,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub token: RequestToken,
    pub result: Result<Outcome, AppError>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Opened,
    Closed,
    Library { rows: Vec<ImageRow>, count: i64 },
    Folders(Vec<FolderRow>),
    Dates(Vec<DateTreeYear>),
    Image(Option<Box<ImageRow>>),
}
