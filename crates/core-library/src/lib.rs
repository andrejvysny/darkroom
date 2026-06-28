//! core-library — watched-root indexing, thumbnail cache, and catalog queries.

pub mod analysis;
pub mod collections;
pub mod cull;
pub mod edits;
pub mod error;
pub mod events;
pub mod face;
pub mod face_cluster;
pub mod features;
pub mod index;
pub mod keywords;
pub mod maintenance;
pub mod presets;
pub mod query;
pub mod reconcile;
pub mod settings;
pub mod sidecar;
pub mod snapshots;
pub mod thumbs;

pub use analysis::{
    analysis_facets, caption_for_image, detections_for_image, existing_analysis, insert_analysis,
    labeled_images, presence_for_image, present_image_count, present_images, present_targets_after,
    set_user_label, set_user_label_many, stale_count, stale_targets, user_labels, AnalysisInput,
    AnalyzeTarget, CaptionRow, DetectionRow, FacetRow, LabeledImage, PresenceRow, StageSpec,
    StaleTarget, UserLabels,
};
pub use collections::{
    add_images_to_collection, collections_for_image, create_collection, delete_collection,
    list_collections, remove_images_from_collection, rename_collection, CollectionRow,
};
pub use cull::{set_flag, set_flag_many, set_label, set_label_many, set_rating, set_rating_many};
pub use edits::{get_edit, get_edit_with_version, set_edit};
pub use error::LibError;
pub use events::{append_event, event_count, ids_json, Event};
pub use face::{
    assign_face_person, confirm_face, create_person, delete_all_face_data, faces_for_clustering,
    faces_summary, image_faces, list_people, merge_people, person_faces, prune_empty_unnamed,
    reconcile_faces, reject_face, set_person_cover, set_person_hidden, set_person_name, FaceInput,
    ImageFaceRow, PersonFaceRow, PersonRow,
};
pub use face_cluster::{cluster_assign, has_dirty_faces, ClusterParams, ClusterStats};
pub use features::{
    compute_features, has_features, images_missing_features, set_image_features, ImageFeatures,
};
pub use index::{
    add_root, enumerate_raws, existing_paths, image_kind, insert_image, now_epoch, process_file,
    relink_missing_image, scan_root, IndexStats, ProcessedImage, SUPPORTED_EXT, THUMB_SIZE,
};
pub use keywords::{
    add_keyword_to_image, add_keyword_to_images, create_or_get_keyword, delete_keyword,
    keywords_for_image, list_keywords, remove_keyword_from_image, KeywordRow,
};
pub use maintenance::reap_dangling_import_sessions;
pub use presets::{
    delete_preset, get_preset, insert_preset, is_builtin, list_presets, seed_builtin_preset,
    unique_name, update_preset, PresetFull, PresetSummary,
};
pub use query::{
    count_images, date_tree, image_by_id, list_folders, present_image_ids, query_images, DateNode,
    DateTreeYear, FolderRow, ImageRow, QueryParams,
};
pub use reconcile::{reconcile, ReconcileStats};
pub use settings::{
    animal_detector_size, face_stage_enabled, get_meta, library_root, preview_edge,
    set_animal_detector_size, set_face_stage_enabled, set_library_root, set_meta, set_preview_edge,
    set_thumb_cache_cap, thumb_cache_cap, DEFAULT_ANIMAL_DETECTOR_SIZE, DEFAULT_THUMB_CACHE_CAP,
    PREVIEW_EDGE_MAX, PREVIEW_EDGE_MIN,
};
pub use sidecar::{
    hydrate_if_blank, rebuild_from_sidecars, write_all_sidecars, write_sidecar, Sidecar,
};
pub use snapshots::{
    create_snapshot, delete_snapshot, get_snapshot_params, list_snapshots, rename_snapshot,
    unique_snapshot_name, SnapshotSummary,
};
pub use thumbs::ThumbCache;
