//! Selection, filtering and grouping state of the import dialog. Pure data; no egui painting, so
//! tests drive it directly. Derived data (day groups, pair map, kinds) is cached against the
//! `Arc` identity of the host's file list and rebuilt only when the list or a filter changes.

use core_app::import::{
    FileStatus, ImportFile, ImportMode, ImportOptions, ImportPhase, ImportPlan, ImportView, Pairing,
};
use core_app::workflow::CullFlag;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// One line of the flattened, grouped list.
#[derive(Clone, Debug)]
pub(super) enum Row {
    /// Day header; `members` indexes [`Cache::order`].
    Header { day: String, members: Range<usize> },
    /// Index into the view's `files`.
    File(usize),
}

pub(super) struct Cache {
    files: Arc<Vec<ImportFile>>,
    skip: bool,
    kind: Option<String>,
    pub rows: Vec<Row>,
    /// File indices in display order (grouped), visible set only.
    pub order: Vec<usize>,
    pub kinds: Vec<&'static str>,
    by_path: HashMap<String, usize>,
    pairs: HashMap<String, Vec<usize>>,
    /// Selectable files over the whole listing (denominator of "N of M").
    pub selectable_total: usize,
}

/// Collection choice for "apply during import".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectionChoice {
    #[default]
    None,
    Existing(i64),
    New,
}

/// Transient dialog state (selection checkboxes, filters, chosen settings).
pub struct ImportUi {
    generation: Option<u64>,
    reset_pending: bool,
    cache: Option<Cache>,
    pub selected: std::collections::HashSet<String>,
    pub skip_duplicates: bool,
    /// `None` = all kinds.
    pub kind_filter: Option<String>,
    pub mode: ImportMode,
    pub pairing: Pairing,
    pub rating: u8,
    pub flag: Option<CullFlag>,
    pub keywords_text: String,
    pub collection: CollectionChoice,
    pub new_collection_name: String,
    pub preview: Option<String>,
    /// Test hooks, refreshed every frame.
    pub import_button: Option<egui::Rect>,
    pub import_enabled: bool,
    pub laid_out_rows: usize,
    pub pill_rect: Option<egui::Rect>,
    pub dialog_shown: bool,
}

impl Default for ImportUi {
    fn default() -> Self {
        Self {
            generation: None,
            reset_pending: false,
            cache: None,
            selected: HashSet::new(),
            skip_duplicates: true,
            kind_filter: None,
            mode: ImportMode::default(),
            pairing: Pairing::default(),
            rating: 0,
            flag: None,
            keywords_text: String::new(),
            collection: CollectionChoice::None,
            new_collection_name: String::new(),
            preview: None,
            import_button: None,
            import_enabled: false,
            laid_out_rows: 0,
            pill_rect: None,
            dialog_shown: false,
        }
    }
}

pub(super) fn is_dup(status: FileStatus) -> bool {
    matches!(
        status,
        FileStatus::DuplicateLibrary | FileStatus::DuplicateBatch
    )
}

/// UTC `YYYY-MM-DD` of epoch seconds; "Unknown" when the mtime is missing.
pub(super) fn day_of(mtime: i64) -> String {
    if mtime == 0 {
        return "Unknown".to_owned();
    }
    let z = mtime.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn build(view: &ImportView, skip: bool, kind: &Option<String>) -> (Cache, Option<String>) {
    let files = &view.files;
    let present: HashSet<&str> = files.iter().map(|f| f.kind.as_str()).collect();
    let kinds: Vec<&'static str> = ["raw", "jpeg", "png", "heif"]
        .into_iter()
        .filter(|k| present.contains(k))
        .collect();
    let kind = kind
        .clone()
        .filter(|k| kinds.iter().any(|present| present == k));
    let mut by_path = HashMap::with_capacity(files.len());
    let mut pairs: HashMap<String, Vec<usize>> = HashMap::new();
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut selectable_total = 0;
    for (i, f) in files.iter().enumerate() {
        by_path.insert(f.path.clone(), i);
        if let Some(key) = &f.pair_key {
            pairs.entry(key.clone()).or_default().push(i);
        }
        if !(skip && is_dup(f.status)) {
            selectable_total += 1;
        }
        if kind.as_deref().is_none_or(|k| k == f.kind) {
            groups.entry(day_of(f.mtime)).or_default().push(i);
        }
    }
    let mut days: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
    // Newest first, "Unknown" last.
    days.sort_by(|a, b| match (a.0 == "Unknown", b.0 == "Unknown") {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => b.0.cmp(&a.0),
    });
    let mut rows = Vec::with_capacity(files.len() + days.len());
    let mut order = Vec::with_capacity(files.len());
    for (day, members) in days {
        let start = order.len();
        order.extend(members);
        rows.push(Row::Header {
            day,
            members: start..order.len(),
        });
        rows.extend(order[start..].iter().map(|&i| Row::File(i)));
    }
    let cache = Cache {
        files: Arc::clone(files),
        skip,
        kind: kind.clone(),
        rows,
        order,
        kinds,
        by_path,
        pairs,
        selectable_total,
    };
    (cache, kind)
}

impl ImportUi {
    /// Reconciles with the host's view: resets the selection on a new `generation`, rebuilds the
    /// derived cache when the list/filter changed, and deselects duplicates while skipping them.
    pub fn sync(&mut self, view: &ImportView) {
        if self.generation != Some(view.generation) {
            self.generation = Some(view.generation);
            self.cache = None;
            self.selected.clear();
            self.kind_filter = None;
            self.preview = None;
            self.reset_pending = true;
        }
        let fresh = self.cache.as_ref().is_some_and(|c| {
            Arc::ptr_eq(&c.files, &view.files)
                && c.skip == self.skip_duplicates
                && c.kind == self.kind_filter
        });
        if fresh {
            return;
        }
        let (cache, kind) = build(view, self.skip_duplicates, &self.kind_filter);
        self.kind_filter = kind;
        // The generation advances when listing *starts* (files still empty); the default selection
        // is taken from the first listing result, not from that empty list.
        if self.reset_pending && !matches!(view.phase, ImportPhase::Listing) {
            self.reset_pending = false;
            self.selected = view
                .files
                .iter()
                .filter(|f| matches!(f.status, FileStatus::New | FileStatus::Pending))
                .map(|f| f.path.clone())
                .collect();
        } else if self.skip_duplicates {
            let files = &view.files;
            self.selected.retain(|p| {
                cache
                    .by_path
                    .get(p)
                    .is_none_or(|&i| !is_dup(files[i].status))
            });
        }
        self.cache = Some(cache);
    }

    pub(super) fn cache(&self) -> Option<&Cache> {
        self.cache.as_ref()
    }

    pub fn is_selected(&self, path: &str) -> bool {
        self.selected.contains(path)
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Selectable files over the whole listing.
    pub fn selectable_total(&self) -> usize {
        self.cache.as_ref().map_or(0, |c| c.selectable_total)
    }

    fn selectable(&self, f: &ImportFile) -> bool {
        !(self.skip_duplicates && is_dup(f.status))
    }

    /// Selects/deselects file `i` and its pair partners (legacy `withPair`).
    fn set_with_pair(&mut self, view: &ImportView, i: usize, on: bool) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let f = &view.files[i];
        let members: Vec<usize> = match f.pair_key.as_ref().and_then(|k| cache.pairs.get(k)) {
            Some(m) => m.clone(),
            None => vec![i],
        };
        for m in members {
            let file = &view.files[m];
            if !on {
                self.selected.remove(&file.path);
            } else if self.selectable(file) {
                self.selected.insert(file.path.clone());
            }
        }
    }

    /// Toggle one file by path (partners follow).
    pub fn toggle_path(&mut self, view: &ImportView, path: &str, on: bool) {
        self.sync(view);
        if let Some(i) = self
            .cache
            .as_ref()
            .and_then(|c| c.by_path.get(path).copied())
        {
            self.set_with_pair(view, i, on);
        }
    }

    /// Group header state and toggle over `members` of `order`.
    pub(super) fn group_checked(&self, view: &ImportView, members: &Range<usize>) -> bool {
        let Some(cache) = self.cache.as_ref() else {
            return false;
        };
        let mut any = false;
        for &i in &cache.order[members.clone()] {
            let f = &view.files[i];
            if self.selectable(f) {
                any = true;
                if !self.selected.contains(&f.path) {
                    return false;
                }
            }
        }
        any
    }

    pub(super) fn set_group(&mut self, view: &ImportView, members: Range<usize>, on: bool) {
        let idx: Vec<usize> = self
            .cache
            .as_ref()
            .map(|c| c.order[members].to_vec())
            .unwrap_or_default();
        for i in idx {
            self.set_with_pair(view, i, on);
        }
    }

    fn visible(&self) -> Vec<usize> {
        self.cache
            .as_ref()
            .map(|c| c.order.clone())
            .unwrap_or_default()
    }

    /// Select every selectable file in the current (filtered) list.
    pub fn select_all(&mut self, view: &ImportView) {
        self.sync(view);
        for i in self.visible() {
            self.set_with_pair(view, i, true);
        }
    }

    /// Deselect every file in the current (filtered) list.
    pub fn select_none(&mut self, view: &ImportView) {
        self.sync(view);
        for i in self.visible() {
            self.set_with_pair(view, i, false);
        }
    }

    /// In the current list: select new/pending files, deselect duplicates.
    pub fn select_only_new(&mut self, view: &ImportView) {
        self.sync(view);
        for i in self.visible() {
            let on = matches!(view.files[i].status, FileStatus::New | FileStatus::Pending);
            let file = &view.files[i];
            if on {
                self.selected.insert(file.path.clone());
            } else {
                self.selected.remove(&file.path);
            }
        }
    }

    pub fn set_skip_duplicates(&mut self, view: &ImportView, skip: bool) {
        self.skip_duplicates = skip;
        self.sync(view);
    }

    /// `(pairs, paired_files)` inside the selection; gates the Pair/Standalone prompt.
    pub fn selected_pairs(&self, view: &ImportView) -> (usize, usize) {
        let Some(cache) = self.cache.as_ref() else {
            return (0, 0);
        };
        let (mut pairs, mut files) = (0, 0);
        for path in &self.selected {
            if let Some(&i) = cache.by_path.get(path) {
                let f = &view.files[i];
                if f.pair_key.is_some() {
                    files += 1;
                    if f.pair_role.as_deref() == Some("primary") {
                        pairs += 1;
                    }
                }
            }
        }
        (pairs, files)
    }

    pub fn destination<'a>(&self, view: &'a ImportView) -> Option<&'a std::path::Path> {
        view.destination.as_deref().or(view.library_root.as_deref())
    }

    /// Why Import is unavailable right now (`None` = ready).
    pub fn import_blocker(&self, view: &ImportView) -> Option<&'static str> {
        if view.source.is_none() {
            return Some("Choose a source folder first.");
        }
        match view.phase {
            ImportPhase::Listing => return Some("Listing files…"),
            ImportPhase::Importing { .. } | ImportPhase::Stopping { .. } => {
                return Some("An import is running.")
            }
            _ => {}
        }
        if self.selected.is_empty() {
            return Some("Nothing selected.");
        }
        if self.mode != ImportMode::Reference && self.destination(view).is_none() {
            return Some("No destination: choose a folder (no library root is configured).");
        }
        None
    }

    fn options(&self) -> ImportOptions {
        let keywords = self
            .keywords_text
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect();
        let name = self.new_collection_name.trim();
        ImportOptions {
            rating: (self.rating > 0).then_some(self.rating),
            flag: self.flag,
            keywords,
            collection_id: match self.collection {
                CollectionChoice::Existing(id) => Some(id),
                _ => None,
            },
            new_collection: (self.collection == CollectionChoice::New && !name.is_empty())
                .then(|| name.to_owned()),
        }
    }

    /// The captured plan for the whole selection (not the filtered subset), or `None` when blocked.
    pub fn plan(&mut self, view: &ImportView) -> Option<ImportPlan> {
        self.sync(view);
        if self.import_blocker(view).is_some() {
            return None;
        }
        let cache = self.cache.as_ref()?;
        let mut selected: Vec<String> = self
            .selected
            .iter()
            .filter(|p| cache.by_path.contains_key(*p))
            .cloned()
            .collect();
        selected.sort();
        let destination = (self.mode != ImportMode::Reference)
            .then(|| self.destination(view).map(std::path::Path::to_path_buf))
            .flatten();
        Some(ImportPlan {
            source: view.source.clone()?,
            mode: self.mode,
            destination,
            selected,
            pairing: self.pairing,
            options: self.options(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::day_of;

    #[test]
    fn utc_days() {
        assert_eq!(day_of(0), "Unknown");
        assert_eq!(day_of(1_700_000_000), "2023-11-14");
        assert_eq!(day_of(86_400), "1970-01-02");
    }
}
