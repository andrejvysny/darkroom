//! Legacy presentation keeps startup wording while the shared runtime retains typed failures.
use core_app::AppError;
use std::error::Error;

pub(crate) fn startup_error(error: AppError) -> String {
    match error {
        AppError::SchemaTooNew { found, supported } => format!(
            "Your photo catalog was last opened by a newer version of Darkroom \
             (catalog format v{found}; this version supports up to v{supported}).\n\n\
             Install the latest Darkroom release to keep using this catalog. \
             Your photos and edits are safe and untouched."
        ),
        AppError::Corrupt(source) => {
            if let Some(core_db::DbError::Corrupt(detail)) = source
                .source()
                .and_then(|e| e.downcast_ref::<core_db::DbError>())
            {
                format!("The photo catalog failed its integrity check ({detail}).\n\n\
                    You can restore a recent copy from the \"backups\" folder next to the catalog \
                    in Application Support, or rebuild the catalog from your library's sidecar files.")
            } else {
                format!("The photo catalog could not be opened: {source}")
            }
        }
        other => format!("The photo catalog could not be opened: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_app::error::ErrorSource;
    #[test]
    fn startup_presentation_preserves_schema_and_integrity_guidance() {
        let newer = startup_error(AppError::SchemaTooNew {
            found: 26,
            supported: 25,
        });
        assert!(newer.contains("catalog format v26; this version supports up to v25"));
        assert!(newer.ends_with("Your photos and edits are safe and untouched."));
        let corrupt = startup_error(AppError::Corrupt(ErrorSource::new(
            core_db::DbError::Corrupt("broken page".into()),
        )));
        assert!(corrupt.starts_with("The photo catalog failed its integrity check (broken page)."));
        assert!(corrupt.contains("restore a recent copy"));
    }
    #[test]
    fn sqlite_corruption_keeps_legacy_generic_text_and_original_source() {
        let engine = core_db::DbError::Sqlite(core_db::rusqlite::Error::InvalidQuery);
        let expected = format!("The photo catalog could not be opened: {engine}");
        assert_eq!(
            startup_error(AppError::Corrupt(ErrorSource::new(engine))),
            expected
        );
    }
}
