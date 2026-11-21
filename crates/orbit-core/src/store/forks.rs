//! Pre-submission snapshots share unchanged history nodes in memory. SQLite stores
//! only the changes since the previous fork point, including rewritten tool rows
//! and replaced provider context. Neither replay nor parent lookups are needed.
use super::*;
use serde::de::DeserializeOwned;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOrigin {
    pub family: String,
    pub parent: String,
    pub entry: String,
    pub excerpt: String,
    pub inherited: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Point {
    entries: History<Entry>,
    input: History<Value>,
    context: ContextState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ContextState {
    effort_baseline: Option<EffortBaseline>,
    summary: String,
    summarized_entries: usize,
    account: Option<String>,
    input_tokens: u64,
}

impl Session {
    pub fn can_fork(&self, index: usize) -> bool {
        self.points.contains_key(&index)
    }
    pub(crate) fn checkpoint(&mut self) {
        // Include already queued observations in model order, without transferring
        // ownership of the runtime objects which produced them.
        let mut boundary = self.clone();
        boundary.consume_pending();
        self.points.insert(
            self.entries.len(),
            Arc::new(Point {
                entries: boundary.entries,
                input: boundary.input,
                context: ContextState {
                    effort_baseline: self.effort_baseline.clone(),
                    summary: self.summary.clone(),
                    summarized_entries: self.summarized_entries,
                    account: self.context_account.clone(),
                    input_tokens: self.usage.input,
                },
            }),
        );
    }

    pub(crate) fn fork(&self, entry: &str, settings: &Settings) -> Result<Self> {
        let index = self
            .entries
            .index_of(entry)
            .context("Message no longer exists")?;
        let point = self
            .points
            .get(&index)
            .context("No exact checkpoint exists for this message")?;
        let selected = &self.entries[index];
        let EntryKind::User { files, images, .. } = &selected.kind else {
            bail!("Only user messages can be forked");
        };
        let excerpt: String = selected
            .text
            .lines()
            .find(|s| !s.trim().is_empty())
            .unwrap_or("Image message")
            .chars()
            .take(64)
            .collect();
        let mut fork = Session::new(self.project.clone(), settings);
        fork.origin = Some(ForkOrigin {
            family: self.origin.as_ref().map_or(&self.id, |o| &o.family).clone(),
            parent: self.id.clone(),
            entry: entry.to_owned(),
            excerpt: excerpt.clone(),
            inherited: index,
        });
        fork.title = format!("Fork · {excerpt}");
        fork.model.clone_from(&self.model);
        fork.effort.clone_from(&self.effort);
        fork.fast = self.fast;
        fork.entries = point.entries.clone();
        fork.input = point.input.clone();
        fork.effort_baseline
            .clone_from(&point.context.effort_baseline);
        fork.summary.clone_from(&point.context.summary);
        fork.summarized_entries = point.context.summarized_entries;
        fork.context_account.clone_from(&point.context.account);
        fork.usage.input = point.context.input_tokens;
        fork.points = self
            .points
            .range(..index)
            .map(|(i, p)| (*i, p.clone()))
            .collect();
        fork.draft.clone_from(&selected.text);
        fork.draft_images.clone_from(images);
        fork.draft_files.clone_from(files);
        Ok(fork)
    }
}

#[derive(Serialize, Deserialize)]
struct Delta<T> {
    len: usize,
    rows: Vec<(usize, T)>,
}
fn delta<'a, T: Record>(prev: &'a History<T>, next: &'a History<T>) -> Delta<&'a T> {
    Delta {
        len: next.len(),
        rows: prev
            .changes(next)
            .filter_map(|(i, v)| v.map(|v| (i, v)))
            .collect(),
    }
}
fn restore<T: Record + DeserializeOwned>(rows: &mut History<T>, json: &str) -> Result<()> {
    let delta: Delta<T> = serde_json::from_str(json)?;
    rows.truncate(delta.len);
    for (i, v) in delta.rows {
        if i == rows.len() {
            rows.push(v);
        } else {
            anyhow::ensure!(
                i < rows.len() && v.id() == rows[i].id(),
                "Invalid fork history position or identity"
            );
            rows[i] = v;
        }
    }
    anyhow::ensure!(rows.len() == delta.len, "Incomplete fork history");
    Ok(())
}

pub(super) fn save(db: &Connection, before: Option<&Session>, next: &Session) -> Result<()> {
    if before.is_none() {
        if let Some(o) = &next.origin {
            db.execute(
                "INSERT OR IGNORE INTO fork_media SELECT ?1,owner FROM fork_media WHERE session=?2",
                params![next.id, o.parent],
            )?;
            db.execute(
                "INSERT OR IGNORE INTO fork_media VALUES (?1,?2)",
                params![next.id, o.parent],
            )?;
        }
        db.execute("DELETE FROM fork_points WHERE session=?1", [&next.id])?;
    }
    let start = before
        .and_then(|s| s.points.get_max())
        .map_or(0, |(i, _)| i + 1);
    let mut prev = before.and_then(|s| s.points.get_max()).map(|(_, p)| p);
    let entries = History::default();
    let input = History::default();
    for (index, p) in next.points.range(start..) {
        db.execute(
            "INSERT INTO fork_points VALUES (?1,?2,?3,?4,?5)",
            params![
                next.id,
                *index as i64,
                serde_json::to_string(&p.context)?,
                serde_json::to_string(&delta(prev.map_or(&entries, |p| &p.entries), &p.entries))?,
                serde_json::to_string(&delta(prev.map_or(&input, |p| &p.input), &p.input))?
            ],
        )?;
        prev = Some(p);
    }
    Ok(())
}
pub(super) fn load(db: &Connection, s: &mut Session) -> Result<()> {
    let mut stmt = db.prepare(
        "SELECT position,state,entries,input FROM fork_points WHERE session=?1 ORDER BY position",
    )?;
    let mut rows = stmt.query([&s.id])?;
    let mut entries = History::default();
    let mut input = History::default();
    while let Some(row) = rows.next()? {
        let index = usize::try_from(row.get::<_, i64>(0)?)?;
        restore(&mut entries, &row.get::<_, String>(2)?)?;
        restore(&mut input, &row.get::<_, String>(3)?)?;
        anyhow::ensure!(
            entries.len() == index
                && index < s.entries.len()
                && matches!(s.entries[index].kind, EntryKind::User { .. }),
            "Invalid fork boundary"
        );
        s.points.insert(
            index,
            Arc::new(Point {
                entries: entries.clone(),
                input: input.clone(),
                context: serde_json::from_str(&row.get::<_, String>(1)?)?,
            }),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn forks_reopen_at_exact_boundaries_after_rewrites_and_parent_deletion() -> Result<()> {
        let root = tempfile::tempdir()?;
        let settings = Settings::default();
        let mut source = Session::new(root.path().into(), &settings);
        let before = json!({"type":"custom_tool_call_output","call_id":"r","output":"before"});
        let compact = json!({"type":"compaction","encrypted_content":"prior account"});
        let message: UserMessage = "Try @notes.txt".into();
        fs::write(root.path().join("notes.txt"), "old bytes")?;
        let files =
            crate::file_tags::capture(root.path(), &root.path().join("files"), &message.text)?;
        {
            let db = Store::open(root.path().into())?;
            // A legacy prompt and a steering prompt must never acquire a point by inference.
            source.accept("legacy".into());
            source.consume_pending();
            source.input.push(before.clone());
            source
                .entries
                .push(Entry::new(EntryKind::Notice, "original tool status"));
            source.checkpoint();
            source.accept_submission(
                "first".into(),
                UserMessage {
                    files: files.clone(),
                    ..message
                },
                false,
            );
            db.save_session(&source)?;
            let saved = source.clone();
            source.consume_pending();
            source.accept_submission("steer".into(), "steer".into(), true);
            source.consume_pending();
            source.entries[1].text = "updated tool status".into();
            source.input = vec![compact.clone()].into();
            source.context_account = Some("prior".into());
            source.summary = "past summary".into();
            source.summarized_entries = source.entries.len();
            source.checkpoint();
            source.accept_submission("second".into(), "different".into(), false);
            source.consume_pending();
            source.input = vec![json!({"content":"future"})].into();
            source.summary = "future summary".into();
            db.save_changes(Some(&saved), &source)?;
        }
        let db = Store::open(root.path().into())?;
        let source = db.load_session(&source.id)?;
        assert!(source.fork(&source.entries[0].id, &settings).is_err());
        assert!(source.fork("steer", &settings).is_err());
        let fork = source.fork("second", &settings)?;
        db.save_session(&fork)?;
        assert!(
            db.delete_sessions(&[source.id.clone()].into())
                .unwrap()
                .is_empty()
        );
        let fork = db.load_session(&fork.id)?;
        assert_eq!(fork.input.len(), 1);
        assert_eq!(fork.input[0], compact);
        assert_eq!(fork.context_account.as_deref(), Some("prior"));
        assert_eq!(fork.summary, "past summary");
        assert_eq!(fork.entries[1].text, "updated tool status");
        assert!(fork.entries.by_id("second").is_none());
        assert!(fork.pending.is_empty() && !fork.interrupted);
        assert_eq!(fork.draft, "different");
        let nested = fork.fork("first", &settings)?;
        assert_eq!(nested.origin.as_ref().unwrap().family, source.id);
        assert_eq!(nested.input.len(), 2);
        assert_eq!(nested.input[1], before);
        assert_eq!(nested.entries[1].text, "original tool status");
        assert_eq!(nested.draft, "Try @notes.txt");
        fs::remove_file(root.path().join("notes.txt"))?;
        fs::write(root.path().join("extra.txt"), "added")?;
        let reused = crate::file_tags::capture_reusing(
            &nested.draft_files,
            root.path(),
            &root.path().join("files"),
            "Compare @extra.txt and @notes.txt",
        )?;
        assert_eq!(reused.len(), 2);
        assert_eq!(reused[0].inline.as_deref(), Some("added"));
        assert_eq!(reused[1], files[0]);
        db.save_session(&nested)?;
        assert!(db.delete_sessions(&[fork.id.clone()].into())?.is_empty());
        let unused: std::collections::HashSet<_> = db
            .delete_sessions(&[nested.id.clone()].into())?
            .into_iter()
            .collect();
        assert_eq!(unused, [source.id, fork.id, nested.id].into());
        Ok(())
    }
}
