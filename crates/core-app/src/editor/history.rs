//! Bounded undo/redo stacks of full parameter snapshots (legacy: 60-entry cap in
//! `src/store/develop.ts`, extended here with a byte budget for mask-heavy edits).

use std::collections::VecDeque;
use std::sync::Arc;

use core_model::DevelopParams;

/// Caps applied independently to each of the undo and redo stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    /// Maximum retained snapshots per stack (legacy parity: 60).
    pub max_entries: usize,
    /// Maximum retained serialized bytes per stack (32 MiB default).
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_entries: 60,
            max_bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
struct Stack {
    entries: VecDeque<Arc<DevelopParams>>,
    bytes: usize,
}

impl Stack {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, entry: Arc<DevelopParams>, limits: &HistoryLimits) {
        self.bytes += entry.estimated_bytes();
        self.entries.push_back(entry);
        // The newest entry is always kept even if it alone exceeds max_bytes.
        while (self.entries.len() > limits.max_entries || self.bytes > limits.max_bytes)
            && self.entries.len() > 1
        {
            if let Some(oldest) = self.entries.pop_front() {
                self.bytes -= oldest.estimated_bytes();
            }
        }
    }

    fn pop(&mut self) -> Option<Arc<DevelopParams>> {
        let entry = self.entries.pop_back()?;
        self.bytes -= entry.estimated_bytes();
        Some(entry)
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// Undo/redo pair of snapshot stacks with cached byte sizes.
#[derive(Debug, Clone)]
pub struct History {
    undo: Stack,
    redo: Stack,
    limits: HistoryLimits,
}

impl History {
    pub fn new(limits: HistoryLimits) -> Self {
        Self {
            undo: Stack::new(),
            redo: Stack::new(),
            limits,
        }
    }

    pub fn push_undo(&mut self, entry: Arc<DevelopParams>) {
        self.undo.push(entry, &self.limits);
    }

    pub fn push_redo(&mut self, entry: Arc<DevelopParams>) {
        self.redo.push(entry, &self.limits);
    }

    pub fn pop_undo(&mut self) -> Option<Arc<DevelopParams>> {
        self.undo.pop()
    }

    pub fn pop_redo(&mut self) -> Option<Arc<DevelopParams>> {
        self.redo.pop()
    }

    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    pub fn undo_len(&self) -> usize {
        self.undo.entries.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.entries.len()
    }

    pub fn undo_bytes(&self) -> usize {
        self.undo.bytes
    }

    pub fn redo_bytes(&self) -> usize {
        self.redo.bytes
    }
}
