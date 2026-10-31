use core_model::identity::*;

fn render() -> RenderIdentity {
    RenderIdentity {
        source: SourceIdentity {
            catalog_epoch: CatalogEpoch(1),
            image_id: ImageId(7),
            source_revision: SourceRevision(1),
        },
        editor_session: EditorSession(1),
        selection_generation: SelectionGeneration(1),
        edit_revision: EditRevision(1),
        process_version: ProcessVersion(1),
        renderer_version: RendererVersion(1),
        cache_version: CacheVersion(1),
        viewport_revision: ViewportRevision(1),
        display_revision: DisplayRevision(1),
        device_epoch: DeviceEpoch(1),
    }
}

#[test]
fn reset_and_reselection_cannot_match_stale_render() {
    let first = render();
    let mut reset = first;
    reset.source.catalog_epoch = reset.source.catalog_epoch.checked_next().unwrap();
    assert_ne!(first, reset);
    let mut selected_again = first;
    selected_again.selection_generation = SelectionGeneration(3);
    assert_ne!(first, selected_again);
    selected_again.editor_session = EditorSession(2);
    selected_again.selection_generation = first.selection_generation;
    assert_ne!(first, selected_again);
}

#[test]
fn revisions_do_not_wrap_and_serialize_transparently() {
    assert_eq!(EditRevision(u64::MAX).checked_next(), None);
    assert_eq!(EditRevision(0).checked_next(), Some(EditRevision(1)));
    assert_eq!(serde_json::to_string(&ImageId(-2)).unwrap(), "-2");
    assert_eq!(
        serde_json::from_str::<SelectionGeneration>("3").unwrap(),
        SelectionGeneration(3)
    );
    let original = render();
    assert_eq!(
        serde_json::from_str::<RenderIdentity>(&serde_json::to_string(&original).unwrap()).unwrap(),
        original
    );
}
