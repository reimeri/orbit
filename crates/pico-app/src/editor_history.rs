//! In-memory undo for the composer. Only native text deltas are retained.
use iced::{
    advanced::graphics::text::cosmic_text::{Change, ChangeItem, Cursor, Selection},
    widget::text_editor::{Action, Content, Edit},
};
use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    time::{Duration, Instant},
};

const MAX_STEPS: usize = 128;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const PARKED_DRAFTS: usize = 15;
const TYPING_PAUSE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
    Undo,
    Redo,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Insert,
    Backspace,
    Delete,
}
type Caret = (Cursor, Selection);

struct Step {
    change: Change,
    before: Caret,
    after: Caret,
    bytes: usize,
}

#[derive(Default)]
pub struct History {
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    undo_bytes: usize,
    group: Option<(Group, Instant)>,
}

impl History {
    pub fn separate(&mut self) {
        self.group = None;
    }

    pub fn perform(&mut self, content: &mut Content, action: Action) {
        let Action::Edit(edit) = &action else {
            if !matches!(action, Action::Scroll { .. }) {
                self.separate();
            }
            content.perform(action);
            return;
        };
        let group = match edit {
            Edit::Insert(_) => Some(Group::Insert),
            Edit::Backspace => Some(Group::Backspace),
            Edit::Delete => Some(Group::Delete),
            _ => None,
        };
        self.record(content, group, |c| c.perform(action));
    }

    /// Capture a compound edit including any preparatory selection changes.
    pub fn transaction<T>(
        &mut self,
        content: &mut Content,
        edit: impl FnOnce(&mut Content) -> T,
    ) -> T {
        self.record(content, None, edit)
    }

    fn record<T>(
        &mut self,
        content: &mut Content,
        group: Option<Group>,
        edit: impl FnOnce(&mut Content) -> T,
    ) -> T {
        let before = content.with_editor_mut(|e| {
            let before = e.selection_state();
            e.start_change();
            before
        });
        let result = edit(content);
        let (mut change, after) =
            content.with_editor_mut(|e| (e.finish_change(), e.selection_state()));
        if change.items.is_empty() {
            return result;
        }
        self.redo.clear();
        let group = group.filter(|_| before.1 == Selection::None && after.1 == Selection::None);
        let now = Instant::now();
        let bytes = change
            .items
            .iter()
            .map(|item| size_of::<ChangeItem>() + item.text.capacity())
            .sum::<usize>();
        self.undo_bytes += bytes;
        if self.group.is_some_and(|(kind, last)| {
            Some(kind) == group && now.duration_since(last) < TYPING_PAUSE
        }) && let Some(step) = self.undo.back_mut()
            && step.after == before
        {
            step.change.items.append(&mut change.items);
            step.after = after;
            step.bytes += bytes;
        } else {
            self.undo.push_back(Step {
                before,
                after,
                change,
                bytes,
            });
        }
        self.group = group.map(|kind| (kind, now));
        // An oversized edit is a barrier: never replay older positions through it.
        while self.undo.len() > MAX_STEPS || self.undo_bytes > MAX_BYTES {
            self.undo_bytes -= self.undo.pop_front().expect("over budget").bytes;
        }
        result
    }

    pub fn replay(&mut self, content: &mut Content, direction: Direction) -> bool {
        self.separate();
        match direction {
            Direction::Undo => {
                let Some(mut step) = self.undo.pop_back() else {
                    return false;
                };
                step.change.reverse();
                content.with_editor_mut(|e| e.replay_change(&step.change, step.before));
                step.change.reverse();
                self.undo_bytes -= step.bytes;
                self.redo.push(step);
            }
            Direction::Redo => {
                let Some(step) = self.redo.pop() else {
                    return false;
                };
                content.with_editor_mut(|e| e.replay_change(&step.change, step.after));
                self.undo_bytes += step.bytes;
                self.undo.push_back(step);
            }
        }
        true
    }
}

#[derive(PartialEq, Eq)]
pub enum Draft {
    New,
    Session(String),
}

#[derive(Default)]
pub struct DraftHistories {
    pub active: History,
    owner: Option<Draft>,
    parked: VecDeque<(Draft, u64, History)>,
}

// Native offsets refer to logical lines, not a flattened string. Verify them
// once on draft switches, never by serializing the buffer on each keystroke.
fn fingerprint(content: &Content) -> u64 {
    content.with_editor(|e| {
        let mut hash = DefaultHasher::new();
        for line in &e.buffer().lines {
            line.text().hash(&mut hash);
            line.ending().as_str().hash(&mut hash);
        }
        hash.finish()
    })
}

impl DraftHistories {
    pub fn switch(&mut self, old: &Content, owner: Option<Draft>, new: &Content) {
        let mut outgoing = std::mem::take(&mut self.active);
        outgoing.separate();
        if let Some(key) = self.owner.take()
            && !(outgoing.undo.is_empty() && outgoing.redo.is_empty())
        {
            self.parked.push_back((key, fingerprint(old), outgoing));
        }
        if let Some(i) = self
            .parked
            .iter()
            .position(|(key, ..)| Some(key) == owner.as_ref())
        {
            let (_, expected, history) = self.parked.remove(i).expect("located draft");
            if fingerprint(new) == expected {
                self.active = history;
            }
        }
        if self.parked.len() > PARKED_DRAFTS {
            self.parked.pop_front();
        }
        self.owner = owner;
    }

    pub fn reset(&mut self, owner: Option<Draft>) {
        self.active = History::default();
        self.owner = owner;
    }

    pub fn retain(&mut self, keep: impl Fn(&Draft) -> bool) {
        self.parked.retain(|(draft, ..)| keep(draft));
        if self.owner.as_ref().is_some_and(|draft| !keep(draft)) {
            self.reset(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::{Redo, Undo};
    use iced::widget::text_editor::Motion;
    use std::sync::Arc;

    #[test]
    fn grouped_edits_branch_only_after_mutation() {
        let mut c = Content::new();
        let mut h = History::default();
        for ch in "λ界!".chars() {
            h.perform(&mut c, Action::Edit(Edit::Insert(ch)));
        }
        for _ in 0..2 {
            h.perform(&mut c, Action::Edit(Edit::Backspace));
        }
        assert_eq!(c.text(), "λ");
        assert!(h.replay(&mut c, Undo));
        assert_eq!(c.text(), "λ界!");
        assert!(h.replay(&mut c, Undo));
        assert_eq!(c.text(), "");
        h.perform(&mut c, Action::Move(Motion::DocumentStart));
        h.perform(&mut c, Action::Edit(Edit::Delete)); // no-op
        assert!(h.replay(&mut c, Redo));
        h.perform(&mut c, Action::Edit(Edit::Insert('?')));
        assert!(!h.replay(&mut c, Redo));
        assert_eq!(c.text(), "λ界!?");
    }

    #[test]
    fn replacement_round_trips_selection_and_mixed_line_endings() {
        let mut c = Content::with_text("α\r\n東京\rfin!");
        let mut h = History::default();
        h.perform(&mut c, Action::SelectAll);
        let selection = c.cursor();
        h.perform(&mut c, Action::Edit(Edit::Paste(Arc::new("a\nb".into()))));
        let caret = c.cursor();
        h.perform(&mut c, Action::Move(Motion::DocumentStart));
        assert!(h.replay(&mut c, Undo));
        assert_eq!(c.text(), "α\r\n東京\rfin!");
        assert_eq!(c.cursor(), selection);
        assert!(h.replay(&mut c, Redo));
        assert_eq!(c.text(), "a\nb");
        assert_eq!(c.cursor(), caret);
    }

    #[test]
    fn cut_preserves_separately_inserted_cr_and_lf() {
        let mut c = Content::new();
        let mut h = History::default();
        for s in ["\r", "\n", "東京"] {
            h.perform(&mut c, Action::Edit(Edit::Paste(Arc::new(s.into()))));
        }
        h.perform(&mut c, Action::SelectAll);
        h.perform(&mut c, Action::Edit(Edit::Delete));
        assert!(h.replay(&mut c, Undo));
        assert_eq!(c.line_count(), 3);
        assert_eq!(c.text(), "\r\n東京");
        assert!(h.replay(&mut c, Redo));
        assert!(c.is_empty());
    }

    #[test]
    fn oversized_paste_is_a_history_barrier_not_a_text_limit() {
        let mut c = Content::new();
        let mut h = History::default();
        h.perform(&mut c, Action::Edit(Edit::Insert('a')));
        h.perform(
            &mut c,
            Action::Edit(Edit::Paste(Arc::new("x".repeat(MAX_BYTES)))),
        );
        assert!(!h.replay(&mut c, Undo));
        h.perform(&mut c, Action::Edit(Edit::Insert('b')));
        assert!(h.replay(&mut c, Undo));
        assert_eq!(c.text().len(), MAX_BYTES + 1);
    }
}
