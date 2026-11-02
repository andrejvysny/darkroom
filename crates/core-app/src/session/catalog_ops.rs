//! Library catalog reads (keywords, collections, image tags, delete-rejected summary) and durable
//! mutations: validation, the ordered effects, and the follow-up reads a committed mutation needs.
use super::{edit::QUITTING, ApplicationSession, BarrierKind};
use crate::state::ImageTags;
use crate::workflow::{CullChange, Effect, Mutation, MutationReport, Operation};
use crate::AppError;
use core_model::catalog::{QueryParams, RejectSummary};
use core_model::identity::ImageId;
use std::sync::Arc;

/// Upper bound of follow-up effects one committed mutation can issue (see [`follow_ups`]).
pub(super) const MAX_FOLLOW_UPS: u64 = 6;

fn invalid(message: &str) -> AppError {
    AppError::InvalidCommand(message.into())
}

fn check_images(images: &[ImageId]) -> Result<(), AppError> {
    if images.is_empty() {
        return Err(invalid("no images given"));
    }
    if images.iter().any(|id| id.0 <= 0) {
        return Err(invalid("image id must be positive"));
    }
    Ok(())
}

fn check_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(invalid("name must not be empty"));
    }
    Ok(())
}

fn check_id(id: i64) -> Result<(), AppError> {
    if id <= 0 {
        return Err(invalid("id must be positive"));
    }
    Ok(())
}

pub(super) fn validate(mutation: &Mutation) -> Result<(), AppError> {
    match mutation {
        Mutation::Cull { images, change } => {
            check_images(images)?;
            if matches!(change, CullChange::Rating(stars) if *stars > 5) {
                return Err(invalid("rating must be 0..=5"));
            }
            Ok(())
        }
        Mutation::AddKeyword { images, name } => {
            check_images(images)?;
            check_name(name)
        }
        Mutation::RemoveKeyword { images, keyword_id } => {
            check_images(images)?;
            check_id(*keyword_id)
        }
        Mutation::DeleteKeyword(id) | Mutation::DeleteCollection(id) => check_id(*id),
        Mutation::CreateCollection {
            name,
            smart,
            images,
        } => {
            check_name(name)?;
            if smart.is_some() && !images.is_empty() {
                return Err(invalid("a smart collection has no static members"));
            }
            if images.iter().any(|id| id.0 <= 0) {
                return Err(invalid("image id must be positive"));
            }
            Ok(())
        }
        Mutation::RenameCollection { id, name } => {
            check_id(*id)?;
            check_name(name)
        }
        Mutation::AddToCollection { id, images }
        | Mutation::RemoveFromCollection { id, images } => {
            check_id(*id)?;
            check_images(images)
        }
        Mutation::UnlinkPair { secondary } => check_id(secondary.0),
        Mutation::DeleteRejected(_) => Ok(()),
    }
}

impl ApplicationSession {
    pub(super) fn load_image_tags(&mut self, image: ImageId) -> Result<Vec<Effect>, AppError> {
        check_id(image.0)?;
        Ok(vec![self.replace(Operation::LoadImageTags(image))])
    }

    pub(super) fn load_reject_summary(
        &mut self,
        query: Box<QueryParams>,
    ) -> Result<Vec<Effect>, AppError> {
        Arc::make_mut(&mut self.snapshot).library.reject_summary = None;
        Ok(vec![self.replace(Operation::LoadRejectSummary(query))])
    }

    pub(super) fn mutate(&mut self, mutation: Mutation) -> Result<Vec<Effect>, AppError> {
        validate(&mutation)?;
        if matches!(&self.barrier, Some(b) if matches!(b.kind, BarrierKind::Shutdown)) {
            return Err(AppError::EditRefused(QUITTING.into()));
        }
        // Never coalesced or replaced: every mutation is its own ordered, durable effect.
        Ok(vec![self.issue(Operation::Mutate(mutation))])
    }

    pub(super) fn tags_loaded(
        &mut self,
        requested: ImageId,
        tags: ImageTags,
    ) -> Result<(), AppError> {
        if tags.image != requested {
            return Err(invalid("image tags do not match the request"));
        }
        Arc::make_mut(&mut self.snapshot).library.tags = Some(Arc::new(tags));
        Ok(())
    }

    pub(super) fn reject_summary_loaded(&mut self, query: QueryParams, summary: RejectSummary) {
        Arc::make_mut(&mut self.snapshot).library.reject_summary = Some((query, summary));
    }

    /// Patch the snapshot for a committed mutation and issue the reads it invalidated.
    pub(super) fn mutated(&mut self, mutation: Mutation, report: MutationReport) -> Vec<Effect> {
        if let Mutation::Cull { images, change } = &mutation {
            for image in images {
                self.cull_applied(*image, *change);
            }
        }
        let library = &mut Arc::make_mut(&mut self.snapshot).library;
        // Cull marks are not tags: bumping here would re-read the info panel on every rating.
        if !matches!(mutation, Mutation::Cull { .. }) {
            library.tags_revision = library.tags_revision.wrapping_add(1);
        }
        library.last_report = Some(report);
        if matches!(mutation, Mutation::DeleteRejected(_)) {
            library.reject_summary = None;
        }
        let plan = follow_ups(&mutation, &self.snapshot.library.query);
        let mut effects = Vec::new();
        if let Some(query) = plan.requery {
            let library = &mut Arc::make_mut(&mut self.snapshot).library;
            if library.query != query {
                library.query = query.clone();
                library.rows = Arc::new(Vec::new());
                library.count = 0;
            }
            effects.push(self.replace(Operation::QueryLibrary(Box::new(query))));
        }
        if plan.keywords {
            effects.push(self.replace(Operation::LoadKeywords));
        }
        if plan.collections {
            effects.push(self.replace(Operation::LoadCollections));
        }
        if plan.dates {
            effects.push(self.replace(Operation::LoadDates));
            effects.push(self.replace(Operation::LoadFolders));
        }
        let tags_image = self
            .snapshot
            .library
            .tags
            .as_ref()
            .map(|tags| tags.image)
            .or(self.snapshot.editor.selected);
        if plan.tags {
            if let Some(image) = tags_image {
                effects.push(self.replace(Operation::LoadImageTags(image)));
            }
        }
        effects
    }
}

#[derive(Default)]
struct FollowUps {
    requery: Option<QueryParams>,
    keywords: bool,
    collections: bool,
    dates: bool,
    tags: bool,
}

/// Reads a committed mutation invalidates. A filtered grid is re-queried only when the mutation
/// can change which rows match it (the cull marks of unfiltered rows are patched in place).
fn follow_ups(mutation: &Mutation, query: &QueryParams) -> FollowUps {
    let mut plan = FollowUps::default();
    let refresh = || Some(query.clone());
    match mutation {
        Mutation::Cull { .. } => {
            plan.collections = true;
        }
        Mutation::AddKeyword { .. } | Mutation::RemoveKeyword { .. } => {
            plan.keywords = true;
            plan.tags = true;
            plan.collections = true;
            if query.keyword_id.is_some() {
                plan.requery = refresh();
            }
        }
        Mutation::DeleteKeyword(id) => {
            plan.keywords = true;
            plan.tags = true;
            plan.collections = true;
            if query.keyword_id == Some(*id) {
                let mut next = query.clone();
                next.keyword_id = None;
                plan.requery = Some(reset_paging(next));
            } else if query.keyword_id.is_some() {
                plan.requery = refresh();
            }
        }
        Mutation::DeleteCollection(id) => {
            plan.collections = true;
            plan.tags = true;
            if query.collection_id == Some(*id) {
                let mut next = query.clone();
                next.collection_id = None;
                plan.requery = Some(reset_paging(next));
            }
        }
        Mutation::CreateCollection { .. }
        | Mutation::RenameCollection { .. }
        | Mutation::AddToCollection { .. }
        | Mutation::RemoveFromCollection { .. } => {
            plan.collections = true;
            plan.tags = true;
            if query.collection_id.is_some() {
                plan.requery = refresh();
            }
        }
        Mutation::UnlinkPair { .. } => {
            plan.tags = true;
            plan.requery = refresh();
        }
        Mutation::DeleteRejected(_) => {
            plan.requery = refresh();
            plan.keywords = true;
            plan.collections = true;
            plan.dates = true;
            plan.tags = true;
        }
    }
    plan
}

fn reset_paging(mut query: QueryParams) -> QueryParams {
    query.offset = Some(0);
    query.cursor_value = None;
    query.cursor_id = None;
    query
}
