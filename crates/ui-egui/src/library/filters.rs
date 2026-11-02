//! Pure helpers over [`QueryParams`] filter dimensions: "any filter set", clear-all and the smart
//! collection predicate (legacy `smartQueryFromParams`: only six dimensions are captured).

use core_model::catalog::QueryParams;

/// Whether any user-visible filter dimension is set (sort/search/paging do not count).
pub(crate) fn any_filter(q: &QueryParams) -> bool {
    q.folder_id.is_some()
        || q.min_stars.is_some()
        || q.flag.is_some()
        || q.color_label.is_some()
        || q.keyword_id.is_some()
        || q.collection_id.is_some()
        || q.import_session_id.is_some()
        || q.capture_year.is_some()
        || q.capture_date.is_some()
        || q.format.is_some()
        || q.include_paired == Some(true)
}

/// Clears every filter dimension, keeping sort, search and limit.
pub(crate) fn clear_filters(q: &mut QueryParams) {
    q.folder_id = None;
    q.min_stars = None;
    q.flag = None;
    q.color_label = None;
    q.keyword_id = None;
    q.collection_id = None;
    q.import_session_id = None;
    q.capture_year = None;
    q.capture_date = None;
    q.detected_category = None;
    q.person_id = None;
    q.format = None;
    q.suggested = None;
    q.include_paired = None;
}

/// The six dimensions a smart collection can store.
pub(crate) fn predicate_of(q: &QueryParams) -> QueryParams {
    QueryParams {
        folder_id: q.folder_id,
        min_stars: q.min_stars,
        flag: q.flag.clone(),
        color_label: q.color_label.clone(),
        keyword_id: q.keyword_id,
        import_session_id: q.import_session_id,
        ..Default::default()
    }
}

/// Whether the query carries at least one predicate dimension.
pub(crate) fn has_predicate(q: &QueryParams) -> bool {
    predicate_of(q) != QueryParams::default()
}

/// Whether the query's predicate dimensions equal `smart`'s (the smart row is "active").
pub(crate) fn predicate_active(q: &QueryParams, smart: &QueryParams) -> bool {
    has_predicate(smart) && predicate_of(q) == predicate_of(smart)
}

/// Clears the six predicate dimensions.
pub(crate) fn clear_predicate(q: &mut QueryParams) {
    q.folder_id = None;
    q.min_stars = None;
    q.flag = None;
    q.color_label = None;
    q.keyword_id = None;
    q.import_session_id = None;
}

/// Applies `smart` onto `q`: every other filter dimension is cleared, sort/search are kept.
pub(crate) fn apply_predicate(q: &mut QueryParams, smart: &QueryParams) {
    clear_filters(q);
    q.folder_id = smart.folder_id;
    q.min_stars = smart.min_stars;
    q.flag = smart.flag.clone();
    q.color_label = smart.color_label.clone();
    q.keyword_id = smart.keyword_id;
    q.import_session_id = smart.import_session_id;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_captures_only_the_six_dimensions() {
        let q = QueryParams {
            min_stars: Some(3),
            flag: Some("pick".into()),
            color_label: Some("red".into()),
            keyword_id: Some(7),
            capture_year: Some("2026".into()),
            format: Some("raw".into()),
            collection_id: Some(2),
            search: Some("x".into()),
            sort: Some("filename".into()),
            ..Default::default()
        };
        let p = predicate_of(&q);
        assert_eq!(p.min_stars, Some(3));
        assert_eq!(p.keyword_id, Some(7));
        assert_eq!(
            p,
            QueryParams {
                min_stars: Some(3),
                flag: Some("pick".into()),
                color_label: Some("red".into()),
                keyword_id: Some(7),
                ..Default::default()
            }
        );
    }

    #[test]
    fn applying_a_predicate_clears_other_dims_but_keeps_sort() {
        let mut q = QueryParams {
            format: Some("raw".into()),
            capture_year: Some("2026".into()),
            sort: Some("filename".into()),
            ..Default::default()
        };
        let smart = QueryParams {
            min_stars: Some(4),
            ..Default::default()
        };
        apply_predicate(&mut q, &smart);
        assert_eq!(q.min_stars, Some(4));
        assert_eq!(q.format, None);
        assert_eq!(q.capture_year, None);
        assert_eq!(q.sort.as_deref(), Some("filename"));
        assert!(predicate_active(&q, &smart));
        clear_predicate(&mut q);
        assert!(!has_predicate(&q));
    }
}
