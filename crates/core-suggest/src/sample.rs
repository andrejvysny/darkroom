//! Training samples and their label provenance.
//!
//! Not every pick/reject is equally informative. A label the user volunteered with no prompt is the
//! ground truth we want to fit; a label that merely *confirms* a suggestion the model already made is
//! partly the model teaching itself (confirmation feedback loop), so it is down-weighted — hardest for
//! high-confidence suggestions, where an "agree" carries almost no new information. A correction of a
//! suggestion is the most informative event there is. Bulk/batch actions are logged but never trained
//! on: they are keyboard-mashing, not judgement.

/// Where a label came from — sets its trust ([`LabelProvenance::base_weight`]) and whether it may be
/// used to *measure* the model ([`LabelProvenance::eval_ok`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelProvenance {
    /// User acted with no suggestion shown.
    Unprompted,
    /// User contradicted a suggestion.
    Override,
    /// User agreed with a *low*-confidence suggestion.
    AgreeLo,
    /// User agreed with a *high*-confidence suggestion — nearly pure confirmation bias.
    AgreeHi,
    /// Bulk action over a selection. Recorded, never trained on.
    Batch,
}

impl LabelProvenance {
    pub fn base_weight(self) -> f32 {
        match self {
            LabelProvenance::Unprompted => 1.0,
            LabelProvenance::Override => 2.75,
            LabelProvenance::AgreeLo => 0.85,
            LabelProvenance::AgreeHi => 0.2,
            LabelProvenance::Batch => 0.0,
        }
    }

    /// May this label be used to *evaluate* the model? Only labels the model did not influence —
    /// scoring on agreements would report the model's own echo as accuracy.
    pub fn eval_ok(self) -> bool {
        matches!(
            self,
            LabelProvenance::Unprompted | LabelProvenance::Override
        )
    }

    /// Part of the capped "agree" mass (see [`crate::compute_weights`]).
    pub fn is_agree(self) -> bool {
        matches!(self, LabelProvenance::AgreeLo | LabelProvenance::AgreeHi)
    }
}

/// One labeled image: assembled features, the label, its provenance, and its burst.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Assembled feature vector (see [`crate::assemble`]); may contain `NaN` for missing signals.
    pub x: Vec<f32>,
    /// `true` = pick, `false` = reject.
    pub y: bool,
    pub provenance: LabelProvenance,
    /// Burst id. A singleton image must still get an id unique to itself — group ids drive both
    /// pair derivation and the leakage-free CV split.
    pub group: u64,
}

/// A within-burst preference: `winner` was picked over `loser`. Indices point into the sample slice
/// the pair was derived from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    pub winner: usize,
    pub loser: usize,
    pub weight: f32,
}
