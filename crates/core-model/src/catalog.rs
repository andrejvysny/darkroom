//! Catalog read values shared by application state and persistence adapters.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryParams {
    pub folder_id: Option<i64>,
    pub min_stars: Option<i64>,
    /// Exact flag match: "pick" | "reject" | "none".
    pub flag: Option<String>,
    /// Exact color-label match (e.g. "red"); use the sentinel "__none__" to match unlabeled.
    pub color_label: Option<String>,
    /// Restrict to images tagged with this keyword id.
    pub keyword_id: Option<i64>,
    /// Restrict to members of this (static) collection id.
    pub collection_id: Option<i64>,
    /// Restrict to images added by this import session id.
    pub import_session_id: Option<i64>,
    /// Restrict to a capture year ("2026"), matched against `capture_date` formatted as UTC.
    pub capture_year: Option<String>,
    /// Restrict to a capture day ("2026-06-22"), matched against `capture_date` formatted as UTC.
    pub capture_date: Option<String>,
    /// Restrict to images with a detected object in this bucket ("People" | "Animals" | "Vehicles").
    pub detected_category: Option<String>,
    /// Restrict to images containing a (confirmed or suggested) face of this person.
    pub person_id: Option<i64>,
    /// Restrict to a source format bucket ("raw" | "jpeg" | "png").
    pub format: Option<String>,
    /// Restrict to images the model badged this way ("pick" | "reject"). Withheld rows never match:
    /// their badge is hidden precisely so those labels stay uninfluenced.
    pub suggested: Option<String>,
    /// Include camera companions (the JPEG/HEIF paired to a RAW at import). Default/`None` hides
    /// them so a paired shot occupies ONE grid cell; `Some(true)` shows every file individually.
    pub include_paired: Option<bool>,
    pub search: Option<String>,
    /// "capture_desc" (default) | "capture_asc" | "filename" | "filename_desc"
    /// | "rating_desc" | "rating_asc" | "imported_desc" | "imported_asc".
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Keyset (seek) pagination cursor: the sort value (`capture_date` or `imported_at`) of the last
    /// loaded row. `None` with `cursor_id = None` = first page; `None` with `cursor_id = Some` = a
    /// cursor sitting in the NULL `capture_date` block.
    pub cursor_value: Option<i64>,
    /// Keyset cursor: `id` of the last loaded row (tie-break). Presence marks "has cursor".
    pub cursor_id: Option<i64>,
    /// Use keyset/seek pagination instead of LIMIT/OFFSET (time-based sorts only). Defaults to offset.
    pub seek: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRow {
    pub id: i64,
    pub content_hash: String,
    pub path: String,
    pub filename: String,
    pub capture_date: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub shutter: Option<String>,
    pub aperture: Option<f64>,
    pub focal_length: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub stars: i64,
    pub flag: String,
    pub color_label: Option<String>,
    /// `edits.updated_at` if the image has a develop edit — versions edit-aware previews (NULL = none).
    pub edited_at: Option<i64>,
    /// When the image was catalogued (epoch seconds). The keyset cursor for imported-date sorts and a
    /// comparator key for the frontend live sorted-merge.
    pub imported_at: i64,
    /// Source format bucket ("raw" | "jpeg" | "png"); null for legacy rows predating the column.
    pub format: Option<String>,
    /// How many camera companions (paired JPEG/HEIF) hang off this row — 0 for an unpaired image.
    /// Non-zero ⇒ this row is a pair primary and the grid should badge it.
    pub paired_count: i64,
    /// The primary this row is a companion of, when it is one (only ever visible with
    /// `include_paired`); `None` for primaries and unpaired images.
    pub paired_to: Option<i64>,
    /// The model's badge for this image ("pick" | "reject"), or `None` when there is no model, no
    /// verdict, or the badge is withheld — the frontend cannot tell those three apart, which is the
    /// point: a withheld image must look exactly like an unscored one.
    pub suggested: Option<String>,
    /// The score behind `suggested`. `None` whenever `suggested` is.
    pub suggestion_score: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateNode {
    /// `YYYY-MM-DD` (UTC) capture day, or "Unknown" when `capture_date` is NULL.
    pub date: String,
    pub count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DateTreeYear {
    /// `YYYY` (UTC) capture year, or "Unknown" when `capture_date` is NULL.
    pub year: String,
    pub count: i64,
    pub dates: Vec<DateNode>,
}

/// One keyword with its present-image count (0 when listed per image).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordRow {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

/// A static or smart collection. `query` is the smart predicate JSON (a partial camelCase
/// `QueryParams`), `None` for static collections.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionRow {
    pub id: i64,
    pub name: String,
    pub is_smart: bool,
    pub query: Option<String>,
    /// Member count (static) or live-match count (smart).
    pub count: i64,
}

impl CollectionRow {
    /// The smart predicate as query params; `None` for a static collection or an unreadable
    /// predicate (never a silently empty filter that would match everything).
    pub fn smart_query(&self) -> Option<QueryParams> {
        if !self.is_smart {
            return None;
        }
        serde_json::from_str(self.query.as_deref()?).ok()
    }
}

/// Whole-pair view for an image, from either member (RAW + camera companions).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInfo {
    /// `"primary"` (the RAW) or `"secondary"` (a companion JPEG/HEIF).
    pub role: String,
    pub primary_id: i64,
    /// Every companion, ordered by filename (includes the queried image when it is a secondary).
    pub secondaries: Vec<ImageRow>,
}

/// What "delete all rejected" would touch, for the confirmation dialog.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectSummary {
    pub images: i64,
    /// Camera companions that ride along with a rejected RAW.
    pub companions: i64,
    pub bytes: i64,
}

/// Result of a delete-rejected pass.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectDeleteResult {
    pub trashed: usize,
    pub failed: usize,
    pub companions: usize,
    pub changed: usize,
    /// Hex content hashes of the removed images (thumbnail GC).
    pub hashes: Vec<String>,
}

/// Result of a remove-from-library pass (catalog rows only; files are never touched).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveResult {
    /// Rows deleted, companions included.
    pub removed: usize,
    /// Of `removed`, the camera companions that rode along with a RAW.
    pub companions: usize,
}
