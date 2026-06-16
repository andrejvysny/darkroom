//! core-library — watched-root indexing, thumbnail cache, and catalog queries.

pub mod cull;
pub mod edits;
pub mod error;
pub mod index;
pub mod query;
pub mod thumbs;

pub use cull::{set_flag, set_label, set_rating};
pub use edits::{get_edit, set_edit};
pub use error::LibError;
pub use index::{
    add_root, enumerate_raws, existing_paths, insert_image, now_epoch, process_file, scan_root,
    IndexStats, ProcessedImage, SUPPORTED_EXT, THUMB_SIZE,
};
pub use query::{
    count_images, image_by_id, list_folders, query_images, FolderRow, ImageRow, QueryParams,
};
pub use thumbs::ThumbCache;
