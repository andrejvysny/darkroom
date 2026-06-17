//! core-library — watched-root indexing, thumbnail cache, and catalog queries.

pub mod analysis;
pub mod collections;
pub mod cull;
pub mod edits;
pub mod error;
pub mod events;
pub mod features;
pub mod index;
pub mod keywords;
pub mod query;
pub mod reconcile;
pub mod settings;
pub mod thumbs;

pub use analysis::{
    analysis_facets, caption_for_image, detections_for_image, existing_analysis, insert_analysis,
    present_image_count, present_images, set_user_label, user_labels, AnalysisInput, AnalyzeTarget,
    CaptionRow, DetectionRow, FacetRow, UserLabels,
};
pub use collections::{
    add_images_to_collection, collections_for_image, create_collection, delete_collection,
    list_collections, remove_images_from_collection, rename_collection, CollectionRow,
};
pub use cull::{set_flag, set_flag_many, set_label, set_label_many, set_rating, set_rating_many};
pub use edits::{get_edit, set_edit};
pub use error::LibError;
pub use events::{append_event, event_count, ids_json, Event};
pub use features::{
    compute_features, has_features, images_missing_features, set_image_features, ImageFeatures,
};
pub use index::{
    add_root, enumerate_raws, existing_paths, insert_image, now_epoch, process_file, scan_root,
    IndexStats, ProcessedImage, SUPPORTED_EXT, THUMB_SIZE,
};
pub use keywords::{
    add_keyword_to_image, add_keyword_to_images, create_or_get_keyword, delete_keyword,
    keywords_for_image, list_keywords, remove_keyword_from_image, KeywordRow,
};
pub use query::{
    count_images, image_by_id, list_folders, query_images, FolderRow, ImageRow, QueryParams,
};
pub use reconcile::{reconcile, ReconcileStats};
pub use settings::{
    animal_detector_size, get_meta, set_animal_detector_size, set_meta, set_thumb_cache_cap,
    thumb_cache_cap, DEFAULT_ANIMAL_DETECTOR_SIZE, DEFAULT_THUMB_CACHE_CAP,
};
pub use thumbs::ThumbCache;
