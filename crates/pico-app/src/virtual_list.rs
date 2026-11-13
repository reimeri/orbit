//! Variable-height viewport index. Unseen rows use estimates; measured rows update
//! a Fenwick tree in O(log n), keeping the visible anchor stable during reflow.
use std::{collections::HashMap, ops::Range};
#[derive(Default)]
pub struct List {
    pub order: Vec<usize>,
    positions: HashMap<usize, usize>,
    heights: Vec<f32>,
    tree: Vec<f32>,
    pub offset: f32,
    pub viewport: f32,
}
impl List {
    pub fn reset(&mut self, rows: impl IntoIterator<Item = (usize, f32)>) {
        let anchor_index = self.at(self.offset);
        let anchor = self
            .order
            .get(anchor_index)
            .map(|&id| (id, self.offset - self.prefix(anchor_index)));
        let known: HashMap<_, _> = self
            .order
            .iter()
            .copied()
            .zip(self.heights.iter().copied())
            .collect();
        self.order.clear();
        self.heights.clear();
        self.positions.clear();
        for (id, estimate) in rows {
            self.positions.insert(id, self.order.len());
            self.order.push(id);
            self.heights
                .push(known.get(&id).copied().unwrap_or(estimate));
        }
        self.tree = vec![0.; self.order.len() + 1];
        for i in 1..self.tree.len() {
            self.tree[i] += self.heights[i - 1];
            let parent = i + (i & i.wrapping_neg());
            if parent < self.tree.len() {
                self.tree[parent] += self.tree[i];
            }
        }
        if let Some((id, inside)) = anchor
            && let Some(&index) = self.positions.get(&id)
        {
            self.offset = self.prefix(index) + inside;
        }
        self.offset = self.offset.min((self.total() - self.viewport).max(0.));
    }
    pub fn push(&mut self, id: usize, height: f32) {
        let index = self.order.len();
        let i = index + 1;
        let value = height + self.prefix(index) - self.prefix(i - (i & i.wrapping_neg()));
        if self.tree.is_empty() {
            self.tree.push(0.);
        }
        self.positions.insert(id, index);
        self.order.push(id);
        self.heights.push(height);
        self.tree.push(value);
    }
    fn add(&mut self, index: usize, delta: f32) {
        let mut i = index + 1;
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i & i.wrapping_neg();
        }
    }
    pub fn prefix(&self, end: usize) -> f32 {
        let mut i = end;
        let mut sum = 0.;
        while i > 0 {
            sum += self.tree[i];
            i -= i & i.wrapping_neg();
        }
        sum
    }
    pub fn total(&self) -> f32 {
        self.prefix(self.order.len())
    }
    fn at(&self, offset: f32) -> usize {
        let mut index = 0;
        let mut sum = 0.;
        let mut bit = self.order.len().next_power_of_two();
        while bit > 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= offset {
                index = next;
                sum += self.tree[next];
            }
            bit >>= 1;
        }
        index.min(self.order.len())
    }
    pub fn visible(&self) -> Range<usize> {
        let height = self.viewport.max(600.);
        let start = self.at((self.offset - height).max(0.));
        let end = (self.at(self.offset + 2. * height) + 1).min(self.order.len());
        start..end
    }
    /// Returns true when preserving the viewport anchor requires a scroll correction.
    pub fn measure(&mut self, id: usize, height: f32) -> bool {
        let Some(&index) = self.positions.get(&id) else {
            return false;
        };
        let height = height.max(1.);
        let delta = height - self.heights[index];
        if delta.abs() < 0.5 {
            return false;
        }
        let before = self.prefix(index + 1) <= self.offset;
        self.heights[index] = height;
        self.add(index, delta);
        if before {
            self.offset = (self.offset + delta).max(0.);
        }
        before
    }
    pub fn end(&mut self) {
        self.offset = (self.total() - self.viewport.max(600.)).max(0.);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn asymmetric_heights_bound_visible_work_and_preserve_anchor() {
        let mut list = List::default();
        list.reset((0..20000).map(|i| (i, if i % 3 == 0 { 41. } else { 173. })));
        list.viewport = 700.;
        list.offset = list.prefix(15000) + 11.;
        let original = list.offset;
        assert!(list.visible().len() < 60);
        assert!(list.measure(20, 273.));
        assert_eq!(list.offset, original + 100.);
        assert!(list.visible().contains(&15000));
        assert!(!list.measure(15002, 55.));
        list.end();
        assert_eq!(list.visible().end, 20000);
    }
}
