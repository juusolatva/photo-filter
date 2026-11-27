//! Bounded undo/redo of whole edit states. The app settles a step once each
//! continuous gesture (slider drag, held key) ends, so one drag is one step.
use filter_core::edit::EditState;
use std::collections::VecDeque;

/// Retained undo plus redo steps, excluding the live edit.
pub const MAX_STEPS: usize = 100;
/// Estimated bytes of retained steps (see [`EditState::estimated_bytes`]).
/// Ordinary edits are a few KiB; large opaque preset nodes can evict history.
pub const MAX_BYTES: usize = 64 << 20;

struct Step {
    edit: EditState,
    bytes: usize,
}
impl Step {
    fn new(edit: EditState) -> Self {
        Self {
            bytes: edit.estimated_bytes(),
            edit,
        }
    }
}

pub struct History {
    /// The live edit as of the last settled step.
    settled: Step,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    bytes: usize,
}
impl History {
    pub fn new(edit: &EditState) -> Self {
        Self {
            settled: Step::new(edit.clone()),
            undo: VecDeque::new(),
            redo: Vec::new(),
            bytes: 0,
        }
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Record `current` as a new step unless it equals the settled edit.
    /// A new step discards redo. Returns whether a step was recorded.
    pub fn commit(&mut self, current: &EditState) -> bool {
        if *current == self.settled.edit {
            return false;
        }
        let previous = std::mem::replace(&mut self.settled, Step::new(current.clone()));
        self.bytes += previous.bytes;
        self.undo.push_back(previous);
        self.redo
            .drain(..)
            .for_each(|step| self.bytes -= step.bytes);
        self.trim();
        true
    }
    /// Settle any unrecorded change, then return the previous edit.
    pub fn undo(&mut self, current: &EditState) -> Option<EditState> {
        self.commit(current);
        let step = self.undo.pop_back()?;
        self.bytes -= step.bytes;
        Some(self.swap(step, |history, live| history.redo.push(live)))
    }
    /// Return the next undone edit. An unrecorded change discards redo first.
    pub fn redo(&mut self, current: &EditState) -> Option<EditState> {
        self.commit(current);
        let step = self.redo.pop()?;
        self.bytes -= step.bytes;
        Some(self.swap(step, |history, live| history.undo.push_back(live)))
    }
    /// Make `step` the settled edit, retaining the old one through `keep`.
    fn swap(&mut self, step: Step, keep: impl FnOnce(&mut Self, Step)) -> EditState {
        let edit = step.edit.clone();
        let live = std::mem::replace(&mut self.settled, step);
        self.bytes += live.bytes;
        keep(self, live);
        self.trim();
        edit
    }
    /// Drop the oldest undo steps, then the furthest redo steps, until within
    /// both limits. A step larger than the byte budget is not retained.
    fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > MAX_STEPS || self.bytes > MAX_BYTES {
            let step = match self.undo.pop_front() {
                Some(step) => step,
                None => self.redo.remove(0),
            };
            self.bytes -= step.bytes;
        }
    }
    #[cfg(test)]
    fn retained(&self) -> (usize, usize, usize) {
        (self.undo.len(), self.redo.len(), self.bytes)
    }
}

#[cfg(test)]
#[path = "../tests/unit/history_tests.rs"]
mod tests;
