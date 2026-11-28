//! Find indexes logical display blocks, never widget trees or provider context.
use crate::{chat_selection::Position, message_window::visit_leaves};
use iced::widget::markdown::{self, Item};
use im::Vector;
use orbit_core::{
    CancellationToken,
    history::History,
    store::{Entry, EntryKind},
};
use regex::{Regex, RegexBuilder};
use std::{cell::Cell, collections::HashMap, rc::Rc, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hit {
    pub block: usize,
    pub start: usize,
    pub end: usize,
}

// Immutable match arrays per matching block: a growing reply can share every
// completed block, even if a common query matched millions of times.
#[derive(Clone, Debug)]
struct Block {
    ordinal: usize,
    hits: Arc<[Hit]>,
    end: usize,
}

fn partition<T: Clone>(items: &Vector<T>, before: impl Fn(&T) -> bool) -> usize {
    items
        .binary_search_by(|item| {
            if before(item) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        })
        .unwrap_err()
}

#[derive(Clone, Default, Debug)]
pub struct Matches(Vector<Block>);
impl Matches {
    fn len(&self) -> usize {
        self.0.back().map_or(0, |b| b.end)
    }
    fn block(&self, block: usize) -> &[Hit] {
        self.0
            .binary_search_by_key(&block, |b| b.ordinal)
            .ok()
            .map_or(&[], |i| &self.0[i].hits)
    }
    fn at(&self, n: usize) -> Hit {
        let b = &self.0[partition(&self.0, |b| b.end <= n)];
        b.hits[n - (b.end - b.hits.len())]
    }
    fn lower_bound(&self, at: Position) -> usize {
        let i = partition(&self.0, |b| b.ordinal < at.block);
        let Some(b) = self.0.get(i) else {
            return self.len();
        };
        b.end - b.hits.len()
            + if b.ordinal == at.block {
                b.hits.partition_point(|h| h.start < at.byte)
            } else {
                0
            }
    }
}

#[derive(Default, Debug, Clone)]
struct Row {
    hits: Arc<Matches>,
    // Exclusive cumulative ordinal, for logarithmic navigation.
    end: usize,
}

#[derive(Default, Debug)]
pub struct Results {
    pub source: History<Entry>,
    rows: Vector<Row>,
}

impl Results {
    pub fn len(&self) -> usize {
        self.rows.back().map_or(0, |r| r.end)
    }
    pub fn at(&self, n: usize) -> (Position, Hit) {
        let entry = partition(&self.rows, |row| row.end <= n);
        let row = &self.rows[entry];
        let hit = row.hits.at(n - (row.end - row.hits.len()));
        (
            Position {
                entry,
                block: hit.block,
                byte: hit.start,
            },
            hit,
        )
    }
    /// First visible occurrence, otherwise the closest in transcript order.
    /// Compare only the two neighbors; never wrap around the ends of history.
    pub fn nearest(&self, first: Position, last: Position) -> Option<usize> {
        if self.len() == 0 {
            return None;
        }
        let next = self.rows.get(first.entry).map_or_else(
            || self.len(),
            |row| row.end - row.hits.len() + row.hits.lower_bound(first),
        );
        if next == 0 {
            return Some(0);
        }
        if next == self.len() {
            return Some(next - 1);
        }
        let (before, hit) = self.at(next - 1);
        let end = Position {
            byte: hit.end,
            ..before
        };
        if end > first {
            return Some(next - 1);
        }
        let after = self.at(next).0;
        // Logical distances: compare messages, then blocks within one message,
        // then bytes within one block. Prefer forward on an equal distance.
        let gap = |a: Position, b: Position| {
            if a.entry != b.entry {
                (a.entry.abs_diff(b.entry), 0, 0)
            } else if a.block != b.block {
                (0, a.block.abs_diff(b.block), 0)
            } else {
                (0, 0, a.byte.abs_diff(b.byte))
            }
        };
        Some(if after <= last || gap(last, after) <= gap(end, first) {
            next
        } else {
            next - 1
        })
    }
    pub fn hits(&self, index: usize, source: &Entry) -> Option<Arc<Matches>> {
        self.rows
            .get(index)
            .filter(|r| r.hits.len() != 0 && std::ptr::eq(&self.source[index], source))
            .map(|r| r.hits.clone())
    }
}

// Historical replies keep only plain logical blocks. The streaming tail also
// retains one incremental parser and its top-level item -> block offsets.
#[derive(Default)]
pub struct Index {
    blocks: HashMap<usize, (Arc<Entry>, Vec<String>)>,
    tail: Option<Tail>,
    matcher: Option<(String, Regex)>,
}

// Prefix block offsets in the parser's top-level item order.
struct Tail {
    index: usize,
    parsed: markdown::Content,
    starts: Vec<usize>,
}

impl Index {
    fn assistant(
        &mut self,
        index: usize,
        source: &History<Entry>,
        previous: Option<&Results>,
    ) -> (&[String], usize) {
        let entry = &source[index];
        let (stored, blocks) = self
            .blocks
            .entry(index)
            .or_insert_with(|| (source.shared(index), Vec::new()));
        if !blocks.is_empty() && std::ptr::eq(stored.as_ref(), entry) {
            return (blocks, 0);
        }
        let mut changed = 0;
        let mut append = false;
        let content;
        let (items, mut starts) = if index + 1 == source.len() {
            let (tail, item) = match &mut self.tail {
                Some(tail)
                    if tail.index == index
                        && !blocks.is_empty()
                        && entry.text.starts_with(&stored.text) =>
                {
                    append = previous.is_some_and(|r| {
                        index < r.source.len() && std::ptr::eq(&r.source[index], stored.as_ref())
                    });
                    let item = tail.parsed.push_str(&entry.text[stored.text.len()..]);
                    (tail, item)
                }
                slot => (
                    slot.insert(Tail {
                        index,
                        parsed: markdown::Content::parse(&entry.text),
                        starts: vec![0],
                    }),
                    0,
                ),
            };
            changed = tail.starts[item];
            tail.starts.truncate(item + 1);
            (&tail.parsed.items()[item..], Some(&mut tail.starts))
        } else {
            content = markdown::Content::parse(&entry.text);
            (content.items(), None)
        };
        blocks.truncate(changed);
        let style = markdown::Style::from(&iced::Theme::Dark);
        let plain = |t: &markdown::Text| t.spans(style).iter().map(|s| s.text.as_ref()).collect();
        for item in items {
            visit_leaves(std::slice::from_ref(item), &mut |leaf| match leaf {
                Item::Paragraph(t) | Item::Heading(_, t) | Item::Image { alt: t, .. } => {
                    blocks.push(plain(t))
                }
                Item::CodeBlock { lines, .. } => blocks.extend(lines.iter().map(plain)),
                Item::DisplayMath(s) => blocks.push(s.clone()),
                Item::Rule => (),
                Item::Quote(_) | Item::List { .. } | Item::Table { .. } => {
                    unreachable!("visit_leaves")
                }
            });
            if let Some(starts) = &mut starts {
                starts.push(blocks.len())
            }
        }
        *stored = source.shared(index);
        (blocks, if append { changed } else { 0 })
    }

    pub fn search(
        &mut self,
        source: History<Entry>,
        previous: Option<&Results>,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Option<Results>, String> {
        if self
            .tail
            .as_ref()
            .is_some_and(|t| t.index + 1 != source.len())
        {
            self.tail = None
        }
        if self.matcher.as_ref().is_none_or(|(q, _)| q != query) {
            let re = RegexBuilder::new(&regex::escape(query))
                .case_insensitive(true)
                .build()
                .map_err(|e| e.to_string())?;
            self.matcher = Some((query.to_owned(), re));
        }
        if previous.is_none() {
            self.blocks.retain(|&i, _| {
                i < source.len() && matches!(source[i].kind, EntryKind::Reply { .. })
            });
        }
        let re = self.matcher.as_ref().unwrap().1.clone();
        let mut rows = previous.map_or_else(Vector::new, |p| p.rows.clone());
        let empty = History::default();
        let old = previous.map_or(&empty, |p| &p.source);
        // The persistent history diff skips shared subtrees. Most streaming
        // frames only replace the tail row and its cumulative count.
        let mut first = rows.len();
        for (i, entry) in old.changes(&source) {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            first = first.min(i);
            let Some(entry) = entry else {
                self.blocks.remove(&i);
                continue;
            };
            if !matches!(entry.kind, EntryKind::Reply { .. }) {
                self.blocks.remove(&i);
            }
            let mut found = Matches::default();
            let scan = |found: &mut Matches, block, text: &str| {
                let mut matches = Vec::new();
                for m in re.find_iter(text) {
                    if cancel.is_cancelled() {
                        break;
                    }
                    matches.push(Hit {
                        block,
                        start: m.start(),
                        end: m.end(),
                    });
                }
                if !matches.is_empty() {
                    found.0.push_back(Block {
                        ordinal: block,
                        end: found.len() + matches.len(),
                        hits: matches.into(),
                    })
                }
            };
            match entry.kind {
                EntryKind::Reply { .. } => {
                    let (blocks, from) = self.assistant(i, &source, previous);
                    if from > 0 {
                        let hits = &rows[i].hits.0;
                        found.0 = hits.take(partition(hits, |b| b.ordinal < from));
                    }
                    for (block, text) in (from..).zip(&blocks[from..]) {
                        if cancel.is_cancelled() {
                            return Ok(None);
                        }
                        scan(&mut found, block, text);
                    }
                }
                EntryKind::User { .. } => scan(&mut found, 0, &entry.text),
                _ => (),
            }
            let row = Row {
                hits: found.into(),
                end: 0,
            };
            if i == rows.len() {
                rows.push_back(row)
            } else {
                rows.set(i, row);
            }
        }
        if cancel.is_cancelled() {
            return Ok(None);
        }
        rows.truncate(source.len());
        let mut end = first.checked_sub(1).map_or(0, |i| rows[i].end);
        for i in first..rows.len() {
            let row = &mut rows[i];
            end += row.hits.len();
            row.end = end;
        }
        Ok(Some(Results { source, rows }))
    }
}

#[derive(Default)]
pub struct Frame {
    pub viewport: Cell<iced::Rectangle>,
    pub visible: Cell<Option<(Position, Position)>>,
    pub located: Cell<Option<(u64, iced::Rectangle, iced::Rectangle, iced::Rectangle)>>,
}

#[derive(Clone, Default)]
pub struct Paint {
    pub frame: Option<Rc<Frame>>,
    pub hits: Option<Arc<Matches>>,
    pub active: Option<Hit>,
    pub reveal: Option<u64>,
}
impl Paint {
    pub fn block(&self, block: usize) -> &[Hit] {
        self.hits.as_ref().map_or(&[], |h| h.block(block))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_prefers_visible_then_compares_both_neighbors() {
        let entries = vec![Entry::new(
            EntryKind::Reply { final_at: None },
            "hit 0123456789 hit 0123456789 hit",
        )]
        .into();
        let result = Index::default()
            .search(entries, None, "hit", &CancellationToken::new())
            .unwrap()
            .unwrap();
        let at = |byte| Position {
            entry: 0,
            block: 0,
            byte,
        };
        assert_eq!(result.nearest(at(4), at(6)), Some(0), "previous is closer");
        assert_eq!(result.nearest(at(8), at(11)), Some(1), "next is closer");
        assert_eq!(
            result.nearest(at(4), at(16)),
            Some(1),
            "visible beats just above"
        );
        assert_eq!(
            result.nearest(at(16), at(24)),
            Some(1),
            "match crosses viewport start"
        );
    }

    #[test]
    fn display_ranges_scope_and_streaming_updates() {
        let mut source: History<Entry> = vec![
            Entry::new(
                EntryKind::User {
                    questionnaire: None,
                    images: vec![],
                    files: vec![],
                    skills: vec![],
                    sent_at: 1_750_000_123,
                    steering: false,
                },
                "**Ｋ** k.",
            ),
            Entry::new(EntryKind::Reasoning, "k."),
            Entry::new(
                EntryKind::Reply { final_at: None },
                "[**K**.](https://k.example) and k.\n\n```sh\n  k.\n```",
            ),
            Entry::new(EntryKind::Notice, "k."),
        ]
        .into();
        let mut index = Index::default();
        let cancel = CancellationToken::new();
        let result = index
            .search(source.clone(), None, "k.", &cancel)
            .unwrap()
            .unwrap();
        let positions = |r: &Results| {
            (0..r.len())
                .map(|i| {
                    let (p, h) = r.at(i);
                    (p.entry, p.block, p.byte, h.end)
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            positions(&result),
            [(0, 0, 8, 10), (2, 0, 0, 4), (2, 0, 9, 11), (2, 1, 2, 4)]
        );
        source[2].text = "replacement".into();
        source.push(Entry::new(
            EntryKind::Reply { final_at: None },
            "- k.\n- tail K.",
        ));
        let updated = index
            .search(source, Some(&result), "k.", &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(
            positions(&updated),
            [(0, 0, 8, 10), (4, 0, 0, 2), (4, 1, 5, 7)]
        );
        assert_eq!(result.len(), 4, "published snapshots remain independent");
    }
    #[test]
    fn streamed_tail_reparses_old_references_without_discarding_prefix_matches() {
        let mut entries: History<Entry> = vec![Entry::new(
            EntryKind::Reply { final_at: None },
            "**first** hit\n\n[hit][alias]\n\npartial hi",
        )]
        .into();
        let mut index = Index::default();
        let cancel = CancellationToken::new();
        let result = index
            .search(entries.clone(), None, "hit", &cancel)
            .unwrap()
            .unwrap();
        entries[0].text.push('t');
        let appended = index
            .search(entries.clone(), Some(&result), "hit", &cancel)
            .unwrap()
            .unwrap();
        assert_eq!((appended.len(), appended.at(2).1.start), (3, 8));
        entries[0]
            .text
            .push_str("\n\n[alias]: https://example.org/");
        let resolved = index
            .search(entries, Some(&appended), "hit", &cancel)
            .unwrap()
            .unwrap();
        assert_eq!(resolved.len(), 3);
        assert_eq!((resolved.at(1).1.block, resolved.at(1).1.start), (1, 0));
    }

    #[test]
    #[ignore = "manual long-session search timing probe"]
    fn find_20k_entries() {
        let mut entries: History<Entry> = (0..20_000)
            .map(|i| {
                Entry::new(
                    EntryKind::Reply { final_at: None },
                    format!("Reply {i}: **build** succeeded.\n\n```rust\nlet n = {i};\n```"),
                )
            })
            .collect();
        let mut index = Index::default();
        let cancel = CancellationToken::new();
        for query in ["build succeeded", "let n ="] {
            let start = std::time::Instant::now();
            let result = index
                .search(entries.clone(), None, query, &cancel)
                .unwrap()
                .unwrap();
            assert_eq!(result.len(), entries.len());
            eprintln!("find 20k, {query}: {:?}", start.elapsed());
            entries
                .last_mut()
                .unwrap()
                .text
                .push_str("\nstreaming update");
            let start = std::time::Instant::now();
            let updated = index
                .search(entries.clone(), Some(&result), query, &cancel)
                .unwrap()
                .unwrap();
            assert_eq!(updated.len(), entries.len());
            eprintln!("find 20k, one modified reply: {:?}", start.elapsed());
        }
    }
}
