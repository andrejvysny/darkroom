//! core-library — watched-root indexing, thumbnail cache, and catalog queries.

pub mod collections;
pub mod cull;
pub mod edits;
pub mod error;
pub mod index;
pub mod keywords;
pub mod query;
pub mod thumbs;

pub use collections::{
    add_images_to_collection, collections_for_image, create_collection, delete_collection,
    list_collections, remove_images_from_collection, rename_collection, CollectionRow,
};
pub use cull::{set_flag, set_label, set_rating};
pub use edits::{get_edit, set_edit};
pub use error::LibError;
pub use keywords::{
    add_keyword_to_image, create_or_get_keyword, delete_keyword, keywords_for_image, list_keywords,
    remove_keyword_from_image, KeywordRow,
};
pub use index::{
    add_root, enumerate_raws, existing_paths, insert_image, now_epoch, process_file, scan_root,
    IndexStats, ProcessedImage, SUPPORTED_EXT, THUMB_SIZE,
};
pub use query::{
    count_images, image_by_id, list_folders, query_images, FolderRow, ImageRow, QueryParams,
};
pub use thumbs::ThumbCache;
