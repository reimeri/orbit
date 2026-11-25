//! Cheap snapshots and indexed edits. Tree diffs skip unchanged shared subtrees.
use im::{HashMap, OrdMap, ordmap::DiffItem};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    ops::{Index, IndexMut},
    sync::Arc,
};

pub trait Record: Clone + PartialEq {
    fn id(&self) -> Option<&str> {
        None
    }
}
impl Record for serde_json::Value {}

#[derive(Clone, Debug)]
pub struct History<T: Record> {
    rows: OrdMap<usize, Arc<T>>,
    ids: HashMap<String, usize>,
}
impl<T: Record> PartialEq for History<T> {
    fn eq(&self, other: &Self) -> bool {
        // The ID index derives from the rows. OrdMap equality skips shared subtrees.
        self.rows == other.rows
    }
}
impl<T: Record> Default for History<T> {
    fn default() -> Self {
        Self {
            rows: OrdMap::new(),
            ids: HashMap::new(),
        }
    }
}
impl<T: Record> History<T> {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub(crate) fn truncate(&mut self, len: usize) {
        while self.len() > len {
            let value = self
                .rows
                .remove(&(self.len() - 1))
                .expect("contiguous history");
            if let Some(id) = value.id() {
                self.ids.remove(id);
            }
        }
    }
    pub fn push(&mut self, value: T) {
        let index = self.len();
        if let Some(id) = value.id() {
            self.ids.insert(id.to_owned(), index);
        }
        self.rows.insert(index, Arc::new(value));
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.rows.values().map(Arc::as_ref)
    }
    pub fn range(&self, start: usize) -> impl Iterator<Item = &T> {
        self.rows.range(start..).map(|(_, v)| v.as_ref())
    }
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.ids.get(id).copied()
    }
    pub fn by_id(&self, id: &str) -> Option<&T> {
        self.index_of(id).map(|i| &self[i])
    }
    pub fn by_id_mut(&mut self, id: &str) -> Option<&mut T> {
        let index = self.index_of(id)?;
        Some(&mut self[index])
    }
    pub fn shared(&self, index: usize) -> Arc<T> {
        self.rows[&index].clone()
    }
    pub fn last_mut(&mut self) -> Option<&mut T> {
        self.len().checked_sub(1).map(|i| &mut self[i])
    }
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.rows.ptr_eq(&other.rows)
    }
    pub fn changes<'a>(&'a self, next: &'a Self) -> impl Iterator<Item = (usize, Option<&'a T>)> {
        self.rows.diff(&next.rows).map(|diff| match diff {
            DiffItem::Add(i, value)
            | DiffItem::Update {
                new: (i, value), ..
            } => (*i, Some(value.as_ref())),
            DiffItem::Remove(i, _) => (*i, None),
        })
    }
}
impl<T: Record> Index<usize> for History<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        &self.rows[&i]
    }
}
impl<T: Record> IndexMut<usize> for History<T> {
    fn index_mut(&mut self, i: usize) -> &mut T {
        Arc::make_mut(self.rows.get_mut(&i).expect("history index"))
    }
}
impl<T: Record> Extend<T> for History<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for value in iter {
            self.push(value);
        }
    }
}
impl<T: Record> FromIterator<T> for History<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut rows = Self::default();
        rows.extend(iter);
        rows
    }
}
impl<T: Record> From<Vec<T>> for History<T> {
    fn from(values: Vec<T>) -> Self {
        values.into_iter().collect()
    }
}
impl<T: Record + Serialize> Serialize for History<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}
impl<'de, T: Record + Deserialize<'de>> Deserialize<'de> for History<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Vec::<T>::deserialize(deserializer)?.into())
    }
}
