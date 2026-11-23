//! Window top-level Markdown items inside the existing transcript scrollable.
//! Lists, tables, quotes and fenced blocks retain their native internal layout.
use crate::{chat_selection::Viewer, keyed_column::keyed_column, ui::Message, virtual_list::List};
use iced::{
    Element, Padding,
    widget::{
        container,
        markdown::{self, Item},
        space,
    },
};
use std::{cell::Cell, collections::HashMap};

const MIN_ITEMS: usize = 64;
const OVERSCAN: f32 = 160.;

// Walk in the same order as markdown::item: table headers first, then cells
// in row-major order. Never build offscreen widgets to find URLs or ordinals.
pub fn visit_leaves(items: &[Item], f: &mut impl FnMut(&Item)) {
    for item in items {
        match item {
            Item::Quote(items) => visit_leaves(items, f),
            Item::List { bullets, .. } => {
                for bullet in bullets {
                    let (markdown::Bullet::Point { items } | markdown::Bullet::Task { items, .. }) =
                        bullet;
                    visit_leaves(items, f);
                }
            }
            Item::Table { columns, rows } => {
                for c in columns {
                    visit_leaves(&c.header, f);
                }
                for r in rows {
                    for cell in r.cells().iter().take(columns.len()) {
                        visit_leaves(cell, f);
                    }
                }
            }
            _ => f(item),
        }
    }
}

pub type Heights = HashMap<usize, f32>;
pub struct Window {
    list: List,
    // Prefix offsets in the full selection/copy traversal, including code lines.
    starts: Vec<usize>,
    pub known: Heights,
    // The inner and outer probes sample the SAME redraw, inside-out. Publish
    // their measurements together, so only one layer corrects the scroll.
    pending: Cell<Option<Geometry>>,
}

#[derive(Clone, Debug)]
pub struct Geometry {
    offset: f32,
    viewport: f32,
    measured: Vec<(usize, f32)>,
}

impl Geometry {
    pub fn save_heights(self, known: &mut Heights) {
        known.extend(self.measured);
    }
}

impl Window {
    pub fn sync(window: &mut Option<Self>, items: &[Item], from: usize, size: f32) {
        if items.len() < MIN_ITEMS {
            *window = None;
            return;
        }
        let window = window.get_or_insert_with(|| Self {
            list: List::with_overscan(OVERSCAN),
            starts: vec![0],
            known: Heights::new(),
            pending: Cell::new(None),
        });
        let from = from.min(window.list.order.len());
        window.starts.truncate(from + 1);
        let mut next = window.starts[from];
        if items.len() < window.list.order.len() {
            window.known.retain(|&i, _| i < items.len());
        }
        window.list.replace_tail(
            from,
            items[from..].iter().enumerate().map(|(j, item)| {
                let start = next;
                visit_leaves(std::slice::from_ref(item), &mut |leaf| {
                    next += match leaf {
                        Item::Rule => 0,
                        Item::CodeBlock { lines, .. } => lines.len(),
                        _ => 1,
                    };
                });
                window.starts.push(next);
                let lines = match item {
                    Item::Table { rows, .. } => 1 + rows.len(),
                    _ => (next - start).max(1),
                };
                // About one line of spacing per top-level item. Measurements
                // replace these estimates at the actual font size and width.
                let line = iced::widget::text::LineHeight::default()
                    .to_absolute(size.into())
                    .0;
                (from + j, line * lines as f32 + size)
            }),
        );
    }

    pub fn restore(&mut self, known: Heights, offset: f32, viewport: f32) {
        self.list.offset = 0.;
        for (&i, &h) in &known {
            self.list.measure(i, h);
        }
        self.known = known;
        self.list.offset = offset;
        self.list.viewport = viewport;
    }

    pub fn visible<'a>(&self, items: &'a [Item]) -> &'a [Item] {
        &items[self.list.visible()]
    }

    pub fn view<'a>(
        &'a self,
        items: &'a [Item],
        settings: markdown::Settings,
        viewer: &Viewer<'a>,
    ) -> Element<'a, Message> {
        let mut range = self.list.visible();
        if viewer.find.reveal.is_some()
            && let Some(hit) = viewer.find.active
        {
            let item = self.starts.partition_point(|&n| n <= hit.block) - 1;
            if !range.contains(&item) {
                range = self
                    .list
                    .visible_at(self.list.prefix(item), self.list.viewport);
            }
        }
        viewer.start_block(self.starts[range.start]);
        let mut rows = vec![(None, space().height(self.list.prefix(range.start)).into())];
        for i in range.clone() {
            let gap = if i + 1 == items.len() {
                0.
            } else {
                settings.spacing.0
            };
            rows.push((
                Some(i),
                container(markdown::item(viewer, settings, &items[i], i))
                    .padding(Padding::default().bottom(gap))
                    .into(),
            ));
        }
        rows.push((
            Some(items.len()),
            space()
                .height((self.list.total() - self.list.prefix(range.end)).max(0.))
                .into(),
        ));
        crate::transcript_viewport::observe_layout(keyed_column(rows), move |layout, viewport| {
            let offset = viewport.y - layout.bounds().y;
            let measured: Vec<_> = range
                .clone()
                .zip(layout.children().skip(1))
                .map(|(i, child)| (i, child.bounds().height.max(1.)))
                .filter(|(i, height)| (self.list.height(*i) - height).abs() >= 0.5)
                .collect();
            self.pending.set(
                (self.list.visible_at(offset, viewport.height) != range || !measured.is_empty())
                    .then_some(Geometry {
                        offset,
                        viewport: viewport.height,
                        measured,
                    }),
            );
            None
        })
    }

    pub fn observe_row<'a>(
        &'a self,
        row: Element<'a, Message>,
        expected: f32,
        report: impl Fn(f32, Option<Geometry>) -> Message + 'a,
    ) -> Element<'a, Message> {
        crate::transcript_viewport::observe_layout(row, move |layout, _| {
            let height = layout.bounds().height;
            let geometry = self.pending.take();
            (geometry.is_some() || (height - expected).abs() >= 0.5)
                .then(|| report(height, geometry))
        })
    }

    pub fn update(&mut self, geometry: Geometry) -> f32 {
        self.list.viewport = geometry.viewport;
        self.list.offset = geometry.offset;
        for (i, height) in geometry.measured {
            self.known.insert(i, height);
            self.list.measure(i, height);
        }
        self.list.offset - geometry.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_suffix_and_late_reference_report_the_first_reparsed_item() {
        let mut content = markdown::Content::parse("First\n\n[guide][later]\n\nThird\n\nTail");
        let tail = content.items().len() - 1;
        assert_eq!(content.push_str(" grows"), tail);
        assert_eq!(
            content.push_str("\n\n[later]: https://example.org/guide\n"),
            1
        );
        let Item::Paragraph(guide) = &content.items()[1] else {
            panic!("link paragraph")
        };
        assert!(
            guide
                .spans(markdown::Style::from(&iced::Theme::Dark))
                .iter()
                .any(|span| span.link.as_deref() == Some("https://example.org/guide"))
        );
    }
}
