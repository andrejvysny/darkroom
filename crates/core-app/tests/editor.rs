//! Behavioural tests for the authoritative editor document (brief WP3-1).

use core_app::editor::{
    Editor, EditorCommand, EditorError, EditorEvent, EditorState, HistoryLimits, ReadyDoc,
};
use core_model::document::{EditDocument, ReadStatus};
use core_model::identity::{EditRevision, EditorSession, ImageId, ProcessVersion};
use core_model::{BrushStroke, ComponentKind, DevelopParams, Mask, MaskComponent};

const IMAGE: ImageId = ImageId(42);
const PV: ProcessVersion = ProcessVersion(4);

fn params(exposure: f32) -> DevelopParams {
    DevelopParams {
        exposure,
        ..DevelopParams::default()
    }
}

fn big_params(exposure: f32) -> DevelopParams {
    let mut p = params(exposure);
    let stroke = BrushStroke {
        points: (0..4000)
            .map(|i| [i as f32 * 1e-4, 1.0 - i as f32 * 1e-4])
            .collect(),
        size: 0.05,
        hardness: 0.7,
        flow: 0.8,
        opacity: 1.0,
        is_erase: false,
    };
    p.masks = vec![Mask {
        name: "brush".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Brush {
                strokes: vec![stroke],
            },
            ..MaskComponent::default()
        }],
        ..Mask::default()
    }];
    p
}

fn session_of(ed: &Editor) -> EditorSession {
    match ed.state() {
        EditorState::Loading { session, .. } => *session,
        EditorState::Ready(doc) => doc.session,
        other => panic!("expected Loading/Ready, got {other:?}"),
    }
}

fn ready(ed: &Editor) -> &ReadyDoc {
    match ed.state() {
        EditorState::Ready(doc) => doc,
        other => panic!("expected Ready, got {other:?}"),
    }
}

fn open_and_load(stored: Option<EditDocument>) -> (Editor, EditorSession) {
    let mut ed = Editor::new(HistoryLimits::default());
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let session = session_of(&ed);
    ed.apply(EditorCommand::Loaded { session, stored }).unwrap();
    (ed, session)
}

// --- gestures -------------------------------------------------------------

#[test]
fn gesture_commit_records_one_undo_entry_and_undo_restores_pre_gesture() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    for i in 0..10 {
        let event = ed
            .apply(EditorCommand::UpdateGesture {
                session: s,
                params: params(i as f32),
            })
            .unwrap();
        assert!(matches!(event, EditorEvent::Changed { .. }));
    }
    assert_eq!(
        ready(&ed).history().undo_len(),
        0,
        "updates stay out of history"
    );
    ed.apply(EditorCommand::CommitGesture { session: s })
        .unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 1, "one entry per gesture");
    let event = ed.apply(EditorCommand::Undo { session: s }).unwrap();
    match event {
        EditorEvent::Changed { params, .. } => assert_eq!(*params, DevelopParams::default()),
        other => panic!("expected Changed, got {other:?}"),
    }
}

#[test]
fn no_op_gesture_commit_pushes_nothing() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    let before = ready(&ed).revision;
    ed.apply(EditorCommand::CommitGesture { session: s })
        .unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 0);
    assert_eq!(
        ready(&ed).revision,
        before,
        "unchanged commit moves nothing"
    );
}

#[test]
fn cancel_restores_params_and_leaves_redo_untouched() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(1.0),
        at_ms: 0,
    })
    .unwrap();
    ed.apply(EditorCommand::Undo { session: s }).unwrap(); // redo: 1
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    ed.apply(EditorCommand::UpdateGesture {
        session: s,
        params: params(3.0),
    })
    .unwrap();
    let mid = ready(&ed).revision.0;
    ed.apply(EditorCommand::CancelGesture { session: s })
        .unwrap();
    let after = ready(&ed).revision.0;
    assert!(mid > 1 && after > mid, "cancel advances revision");
    assert_eq!(
        *ready(&ed).params,
        DevelopParams::default(),
        "pixels go back"
    );
    assert_eq!(ready(&ed).history().undo_len(), 0, "cancel pushes no entry");
    assert_eq!(ready(&ed).history().redo_len(), 1, "redo untouched");
}

#[test]
fn focus_lost_commits_and_escape_reverts() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    ed.apply(EditorCommand::UpdateGesture {
        session: s,
        params: params(5.0),
    })
    .unwrap();
    ed.apply(EditorCommand::FocusLost { session: s }).unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 1, "focus loss commits");
    ed.apply(EditorCommand::Undo { session: s }).unwrap();
    assert_eq!(*ready(&ed).params, DevelopParams::default());

    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    ed.apply(EditorCommand::UpdateGesture {
        session: s,
        params: params(7.0),
    })
    .unwrap();
    ed.apply(EditorCommand::CancelGesture { session: s })
        .unwrap();
    assert_eq!(
        *ready(&ed).params,
        DevelopParams::default(),
        "escape reverts"
    );
}

#[test]
fn focus_lost_without_gesture_is_unchanged() {
    let (mut ed, s) = open_and_load(None);
    assert_eq!(
        ed.apply(EditorCommand::FocusLost { session: s }).unwrap(),
        EditorEvent::Unchanged
    );
}

#[test]
fn gesture_errors_and_mutual_exclusion() {
    let (mut ed, s) = open_and_load(None);
    assert_eq!(
        ed.apply(EditorCommand::UpdateGesture {
            session: s,
            params: params(1.0)
        }),
        Err(EditorError::NoGesture)
    );
    assert_eq!(
        ed.apply(EditorCommand::CommitGesture { session: s }),
        Err(EditorError::NoGesture)
    );
    assert_eq!(
        ed.apply(EditorCommand::CancelGesture { session: s }),
        Err(EditorError::NoGesture)
    );
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    assert_eq!(
        ed.apply(EditorCommand::BeginGesture { session: s }),
        Err(EditorError::GestureActive)
    );
    assert_eq!(
        ed.apply(EditorCommand::Apply {
            session: s,
            params: params(1.0),
            at_ms: 0
        }),
        Err(EditorError::GestureActive)
    );
    assert_eq!(
        ed.apply(EditorCommand::Undo { session: s }),
        Err(EditorError::GestureActive)
    );
    assert_eq!(
        ed.apply(EditorCommand::Redo { session: s }),
        Err(EditorError::GestureActive)
    );
    assert_eq!(
        ed.apply(EditorCommand::Reset { session: s }),
        Err(EditorError::GestureActive)
    );
    assert_eq!(
        ed.apply(EditorCommand::Paste {
            session: s,
            params: params(2.0)
        }),
        Err(EditorError::GestureActive)
    );
}

// --- undo / redo / burst ---------------------------------------------------

#[test]
fn new_apply_after_undo_clears_redo() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(1.0),
        at_ms: 0,
    })
    .unwrap();
    ed.apply(EditorCommand::Undo { session: s }).unwrap();
    assert_eq!(ready(&ed).history().redo_len(), 1);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(2.0),
        at_ms: 5000,
    })
    .unwrap();
    assert_eq!(ready(&ed).history().redo_len(), 0, "redo invalidated");
}

#[test]
fn burst_coalescing_700ms() {
    let (mut ed, s) = open_and_load(None);
    for at in [0u64, 300, 900] {
        ed.apply(EditorCommand::Apply {
            session: s,
            params: params(at as f32),
            at_ms: at,
        })
        .unwrap();
    }
    assert_eq!(
        ready(&ed).history().undo_len(),
        1,
        "sliding window: each gap (300, 600) is <= 700, one undo step"
    );

    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(1.0),
        at_ms: 0,
    })
    .unwrap();
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(2.0),
        at_ms: 701,
    })
    .unwrap();
    assert_eq!(
        ready(&ed).history().undo_len(),
        2,
        "701 ms gap is a new burst"
    );

    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(1.0),
        at_ms: 0,
    })
    .unwrap();
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(2.0),
        at_ms: 700,
    })
    .unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 1, "<= 700 ms coalesces");
}

#[test]
fn undo_then_apply_opens_new_burst_within_window() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(1.0),
        at_ms: 0,
    })
    .unwrap();
    ed.apply(EditorCommand::Undo { session: s }).unwrap();
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(2.0),
        at_ms: 100,
    })
    .unwrap();
    assert_eq!(
        ready(&ed).history().undo_len(),
        1,
        "non-apply mutation ends the burst"
    );
}

#[test]
fn mutating_commands_strictly_advance_revision() {
    let (mut ed, s) = open_and_load(None);
    assert_eq!(
        ed.apply(EditorCommand::Undo { session: s }).unwrap(),
        EditorEvent::Unchanged
    );
    assert_eq!(
        ed.apply(EditorCommand::Redo { session: s }).unwrap(),
        EditorEvent::Unchanged
    );
    let mut last = ready(&ed).revision;
    for cmd in [
        EditorCommand::Apply {
            session: s,
            params: params(1.0),
            at_ms: 0,
        },
        EditorCommand::Undo { session: s },
        EditorCommand::Redo { session: s },
        EditorCommand::Reset { session: s },
        EditorCommand::Paste {
            session: s,
            params: params(4.0),
        },
    ] {
        let event = ed.apply(cmd).unwrap();
        match event {
            EditorEvent::Changed { revision, .. } => {
                assert!(revision.0 > last.0, "revision strictly increases");
                last = revision;
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }
}

#[test]
fn equal_params_reset_pushes_nothing_but_advances_revision() {
    let (mut ed, s) = open_and_load(None);
    let before = ready(&ed).revision;
    ed.apply(EditorCommand::Reset { session: s }).unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 0);
    assert_eq!(ready(&ed).revision.0, before.0 + 1);
}

#[test]
fn count_bound_keeps_60_newest() {
    let (mut ed, s) = open_and_load(None);
    for i in 0..100u64 {
        ed.apply(EditorCommand::Apply {
            session: s,
            params: params(i as f32),
            at_ms: i * 1000,
        })
        .unwrap();
    }
    assert_eq!(ready(&ed).history().undo_len(), 60);
}

#[test]
fn byte_bound_evicts_oldest_keeps_newest() {
    let (mut ed, s) = {
        let mut ed = Editor::new(HistoryLimits {
            max_entries: 60,
            max_bytes: 4096,
        });
        ed.apply(EditorCommand::Open {
            image: IMAGE,
            default_process_version: PV,
        })
        .unwrap();
        let s = session_of(&ed);
        ed.apply(EditorCommand::Loaded {
            session: s,
            stored: None,
        })
        .unwrap();
        (ed, s)
    };
    for i in 0..3u64 {
        ed.apply(EditorCommand::Apply {
            session: s,
            params: big_params(i as f32),
            at_ms: i * 1000,
        })
        .unwrap();
    }
    let doc = ready(&ed);
    assert_eq!(
        doc.history().undo_len(),
        1,
        "oversized entries evicted to newest"
    );
    assert!(
        doc.history().undo_bytes() <= big_params(0.0).estimated_bytes(),
        "single retained entry is one snapshot"
    );
    ed.apply(EditorCommand::Undo { session: s }).unwrap();
    assert_eq!(*ready(&ed).params, big_params(1.0), "newest entry survived");
    assert_eq!(
        ed.apply(EditorCommand::Undo { session: s }).unwrap(),
        EditorEvent::Unchanged,
        "older entries were evicted"
    );
}

#[test]
fn byte_bound_multi_entry_stays_under_limit() {
    let entry = big_params(0.0).estimated_bytes();
    let mut ed = Editor::new(HistoryLimits {
        max_entries: 60,
        max_bytes: entry * 2,
    });
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let s = session_of(&ed);
    ed.apply(EditorCommand::Loaded {
        session: s,
        stored: None,
    })
    .unwrap();
    for i in 0..5u64 {
        ed.apply(EditorCommand::Apply {
            session: s,
            params: big_params(i as f32),
            at_ms: i * 1000,
        })
        .unwrap();
    }
    let doc = ready(&ed);
    assert!(doc.history().undo_bytes() <= entry * 2, "budget respected");
    assert!(
        doc.history().undo_len() >= 2,
        "budget fits more than one entry"
    );
}

// --- sessions / loading -----------------------------------------------------

#[test]
fn delayed_load_for_previous_image_is_stale() {
    let mut ed = Editor::new(HistoryLimits::default());
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let s1 = session_of(&ed);
    ed.apply(EditorCommand::Open {
        image: ImageId(43),
        default_process_version: PV,
    })
    .unwrap();
    let s2 = session_of(&ed);
    assert_eq!(
        ed.apply(EditorCommand::Loaded {
            session: s1,
            stored: None
        }),
        Err(EditorError::StaleSession)
    );
    match ed.state() {
        EditorState::Loading { image, session } => {
            assert_eq!(*image, ImageId(43));
            assert_eq!(*session, s2);
        }
        other => panic!("still loading B, got {other:?}"),
    }
}

#[test]
fn repeated_open_allocates_distinct_sessions() {
    let mut ed = Editor::new(HistoryLimits::default());
    let mut opened = Vec::new();
    for image in [IMAGE, ImageId(43), IMAGE] {
        let event = ed
            .apply(EditorCommand::Open {
                image,
                default_process_version: PV,
            })
            .unwrap();
        match event {
            EditorEvent::Opened(session) => opened.push(session),
            other => panic!("expected Opened, got {other:?}"),
        }
    }
    assert_ne!(opened[0], opened[1]);
    assert_ne!(opened[1], opened[2]);
    assert_ne!(opened[0], opened[2]);
    assert_eq!(
        ed.apply(EditorCommand::Loaded {
            session: opened[0],
            stored: None
        }),
        Err(EditorError::StaleSession),
        "first A's load result is rejected"
    );
    ed.apply(EditorCommand::Loaded {
        session: opened[2],
        stored: None,
    })
    .unwrap();
    assert!(matches!(ed.state(), EditorState::Ready(_)));
}

#[test]
fn stored_document_selects_unsupported_or_corrupt() {
    let (ed, _) = open_and_load(Some(EditDocument::read(r#"{"exposure":1}"#, 99)));
    match ed.state() {
        EditorState::Unsupported {
            image,
            document,
            reason,
            ..
        } => {
            assert_eq!(*image, IMAGE);
            assert_eq!(document.raw_json(), r#"{"exposure":1}"#);
            assert!(!reason.is_empty());
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }

    let (ed, _) = open_and_load(Some(EditDocument::read("{ not json", 4)));
    assert!(matches!(ed.state(), EditorState::Corrupt { .. }));

    let (ed, _) = open_and_load(Some(EditDocument::read(r#"{"exposure":1,"madeUp":5}"#, 4)));
    assert!(matches!(ed.state(), EditorState::Unsupported { .. }));

    let mut ed = Editor::new(HistoryLimits::default());
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let s = session_of(&ed);
    ed.apply(EditorCommand::LoadFailed {
        session: s,
        message: "io".into(),
    })
    .unwrap();
    match ed.state() {
        EditorState::Error { message, .. } => assert_eq!(message, "io"),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn supported_stored_document_loads_clean_with_stored_process_version() {
    let (mut ed, s) = open_and_load(Some(EditDocument::read(r#"{"exposure":1.5}"#, 5)));
    let doc = ready(&ed);
    assert_eq!(doc.process_version, ProcessVersion(5));
    assert_eq!(doc.params.exposure, 1.5);
    assert!(!doc.is_dirty());
    assert_eq!(doc.saved_revision, None);
    for cmd in [
        EditorCommand::Apply {
            session: s,
            params: params(2.0),
            at_ms: 0,
        },
        EditorCommand::Undo { session: s },
        EditorCommand::Reset { session: s },
        EditorCommand::Paste {
            session: s,
            params: params(3.0),
        },
    ] {
        ed.apply(cmd).unwrap();
        assert_eq!(
            ready(&ed).process_version,
            ProcessVersion(5),
            "process version survives"
        );
    }
}

// --- protected states --------------------------------------------------------

fn with_session(cmd: &EditorCommand, session: EditorSession) -> EditorCommand {
    match cmd {
        EditorCommand::BeginGesture { .. } => EditorCommand::BeginGesture { session },
        EditorCommand::UpdateGesture { params, .. } => EditorCommand::UpdateGesture {
            session,
            params: params.clone(),
        },
        EditorCommand::CommitGesture { .. } => EditorCommand::CommitGesture { session },
        EditorCommand::CancelGesture { .. } => EditorCommand::CancelGesture { session },
        EditorCommand::FocusLost { .. } => EditorCommand::FocusLost { session },
        EditorCommand::Apply { params, at_ms, .. } => EditorCommand::Apply {
            session,
            params: params.clone(),
            at_ms: *at_ms,
        },
        EditorCommand::Undo { .. } => EditorCommand::Undo { session },
        EditorCommand::Redo { .. } => EditorCommand::Redo { session },
        EditorCommand::Reset { .. } => EditorCommand::Reset { session },
        EditorCommand::Paste { params, .. } => EditorCommand::Paste {
            session,
            params: params.clone(),
        },
        EditorCommand::MarkSaved { revision, .. } => EditorCommand::MarkSaved {
            session,
            revision: *revision,
        },
        _ => panic!("not a mutating command"),
    }
}

#[test]
fn protected_states_refuse_every_mutation_and_preserve_json() {
    let raw = r#"{"exposure":1,"madeUp":{"nested":[1,2]}}"#;
    let docs = vec![
        EditDocument::read(raw, 99),   // UnsupportedVersion
        EditDocument::read(raw, 4),    // UnsupportedFields
        EditDocument::read("nope", 4), // Invalid -> Corrupt
    ];
    let cmds = vec![
        EditorCommand::BeginGesture {
            session: EditorSession(0),
        },
        EditorCommand::UpdateGesture {
            session: EditorSession(0),
            params: params(1.0),
        },
        EditorCommand::CommitGesture {
            session: EditorSession(0),
        },
        EditorCommand::CancelGesture {
            session: EditorSession(0),
        },
        EditorCommand::FocusLost {
            session: EditorSession(0),
        },
        EditorCommand::Apply {
            session: EditorSession(0),
            params: params(1.0),
            at_ms: 0,
        },
        EditorCommand::Undo {
            session: EditorSession(0),
        },
        EditorCommand::Redo {
            session: EditorSession(0),
        },
        EditorCommand::Reset {
            session: EditorSession(0),
        },
        EditorCommand::Paste {
            session: EditorSession(0),
            params: params(1.0),
        },
        EditorCommand::MarkSaved {
            session: EditorSession(0),
            revision: EditRevision(1),
        },
    ];
    for doc in docs {
        let corrupt = matches!(doc.status(), ReadStatus::Invalid);
        let expected_raw = if corrupt { "nope" } else { raw };
        let (mut ed, s) = open_and_load(Some(doc));
        for cmd in &cmds {
            assert_eq!(ed.apply(with_session(cmd, s)), Err(EditorError::Protected));
            assert_eq!(
                ed.apply(with_session(cmd, EditorSession(999))),
                Err(EditorError::StaleSession)
            );
        }
        let preserved = match ed.state() {
            EditorState::Unsupported { document, .. } | EditorState::Corrupt { document, .. } => {
                document.raw_json().to_string()
            }
            other => panic!("expected protected state, got {other:?}"),
        };
        assert_eq!(preserved, expected_raw, "raw JSON must survive untouched");
    }
}

#[test]
fn loading_and_empty_refuse_mutations_as_not_ready() {
    let mut ed = Editor::new(HistoryLimits::default());
    assert_eq!(
        ed.apply(EditorCommand::Reset {
            session: EditorSession(1)
        }),
        Err(EditorError::NotReady),
        "Empty has no session, so any session is stale"
    );
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let s = session_of(&ed);
    assert_eq!(
        ed.apply(EditorCommand::Reset { session: s }),
        Err(EditorError::NotReady)
    );
    assert_eq!(
        ed.apply(EditorCommand::BeginGesture { session: s }),
        Err(EditorError::NotReady)
    );
    // Loading ignores second load results for its own session.
    assert_eq!(
        ed.apply(EditorCommand::Loaded {
            session: s,
            stored: None
        }),
        Ok(EditorEvent::Opened(s))
    );
    assert_eq!(
        ed.apply(EditorCommand::Loaded {
            session: s,
            stored: None
        }),
        Ok(EditorEvent::Unchanged)
    );
}

// --- saved marker / close -----------------------------------------------------

#[test]
fn untouched_default_document_is_clean() {
    let (ed, _) = open_and_load(None);
    assert!(!ready(&ed).is_dirty());
    assert_eq!(ready(&ed).revision, EditRevision(1));
    assert_eq!(ready(&ed).saved_revision, None);
}

#[test]
fn mark_saved_rejects_out_of_order_and_future_acks() {
    let (mut ed, s) = open_and_load(None);
    for i in 0..4u64 {
        ed.apply(EditorCommand::Apply {
            session: s,
            params: params(i as f32),
            at_ms: i * 1000,
        })
        .unwrap();
    }
    assert_eq!(ready(&ed).revision, EditRevision(5));
    assert!(ready(&ed).is_dirty(), "dirty until acknowledged");
    assert_eq!(
        ed.apply(EditorCommand::MarkSaved {
            session: s,
            revision: EditRevision(6),
        })
        .unwrap(),
        EditorEvent::Unchanged,
        "future ack refused"
    );
    assert_eq!(
        ed.apply(EditorCommand::MarkSaved {
            session: s,
            revision: EditRevision(5),
        })
        .unwrap(),
        EditorEvent::Saved(EditRevision(5))
    );
    assert!(!ready(&ed).is_dirty(), "clean when saved matches revision");
    assert_eq!(
        ed.apply(EditorCommand::MarkSaved {
            session: s,
            revision: EditRevision(3),
        })
        .unwrap(),
        EditorEvent::Unchanged,
        "stale ack refused"
    );
    assert_eq!(ready(&ed).saved_revision, Some(EditRevision(5)));
    ed.apply(EditorCommand::Undo { session: s }).unwrap();
    assert!(ready(&ed).is_dirty(), "editing after save dirties again");
}

#[test]
fn close_returns_pending_only_for_dirty_docs() {
    let (mut ed, _) = open_and_load(None);
    match ed.apply(EditorCommand::Close).unwrap() {
        EditorEvent::Closed { pending } => assert!(pending.is_none()),
        other => panic!("expected Closed, got {other:?}"),
    }
    assert!(matches!(ed.state(), EditorState::Empty));
    assert_eq!(
        ed.apply(EditorCommand::Close).unwrap(),
        EditorEvent::Unchanged
    );

    let (mut ed, s) = open_and_load(Some(EditDocument::read(r#"{"exposure":1.5}"#, 5)));
    ed.apply(EditorCommand::Apply {
        session: s,
        params: params(2.5),
        at_ms: 0,
    })
    .unwrap();
    let revision = ready(&ed).revision;
    match ed.apply(EditorCommand::Close).unwrap() {
        EditorEvent::Closed {
            pending: Some((image, rev, pending_params, pv)),
        } => {
            assert_eq!(image, IMAGE);
            assert_eq!(rev, revision);
            assert_eq!(*pending_params, params(2.5));
            assert_eq!(pv, ProcessVersion(5));
        }
        other => panic!("expected pending close, got {other:?}"),
    }
}

#[test]
fn close_commits_active_gesture_into_pending() {
    let (mut ed, s) = open_and_load(None);
    ed.apply(EditorCommand::BeginGesture { session: s })
        .unwrap();
    ed.apply(EditorCommand::UpdateGesture {
        session: s,
        params: params(9.0),
    })
    .unwrap();
    match ed.apply(EditorCommand::Close).unwrap() {
        EditorEvent::Closed {
            pending: Some((_, _, params, _)),
        } => assert_eq!(params.exposure, 9.0, "pending carries the dragged state"),
        other => panic!("expected pending close, got {other:?}"),
    }
    // History of the closed doc is gone with it: reopening starts fresh.
    ed.apply(EditorCommand::Open {
        image: IMAGE,
        default_process_version: PV,
    })
    .unwrap();
    let s = session_of(&ed);
    ed.apply(EditorCommand::Loaded {
        session: s,
        stored: None,
    })
    .unwrap();
    assert_eq!(ready(&ed).history().undo_len(), 0);
}

// --- revision monotonicity under a random op mix ------------------------------

#[test]
fn fifty_random_ops_never_repeat_a_revision() {
    let (mut ed, s) = open_and_load(None);
    let mut seed: u64 = 0x243F_6A88_85A3_08D3;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 59) as usize
    };
    let mut seen: Vec<u64> = vec![ready(&ed).revision.0];
    for step in 0..50u64 {
        let cmd = match next() {
            0 => EditorCommand::BeginGesture { session: s },
            1 => EditorCommand::UpdateGesture {
                session: s,
                params: params(step as f32),
            },
            2 => EditorCommand::CommitGesture { session: s },
            3 => EditorCommand::CancelGesture { session: s },
            4 => EditorCommand::Apply {
                session: s,
                params: params(step as f32 / 2.0),
                at_ms: step * 400,
            },
            5 => EditorCommand::Undo { session: s },
            6 => EditorCommand::Redo { session: s },
            _ => EditorCommand::Reset { session: s },
        };
        if let Ok(EditorEvent::Changed { revision, .. }) = ed.apply(cmd) {
            assert!(
                !seen.contains(&revision.0),
                "revision {} repeated",
                revision.0
            );
            assert!(
                revision.0 > *seen.last().unwrap(),
                "revision must not regress"
            );
            seen.push(revision.0);
        }
    }
}
