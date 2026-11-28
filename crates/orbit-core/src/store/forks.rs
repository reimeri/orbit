//! Pre-submission checkpoints retain exact deltas, including rewritten tool rows
//! and replaced provider context. Reopen keeps them encoded until a fork or the
//! next checkpoint needs them; no parent lookups or command replay.
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

type Histories = (History<Entry>, History<Value>);

#[derive(Clone, Debug, Default)]
pub(crate) struct Points {
    records: im::OrdMap<usize, Arc<Point>>,
}
impl PartialEq for Points {
    fn eq(&self, other: &Self) -> bool {
        // The common save comparison is between shared snapshots. Independently
        // reopened sessions can encode the very same histories differently.
        self.records == other.records || (self.records.len() == other.records.len() && self.records.iter().all(|(i, p)| {
            other.records.get(i).is_some_and(|other_point| p.context == other_point.context
                && matches!((self.histories_at(*i), other.histories_at(*i)), (Ok(a), Ok(b)) if a == b))
        }))
    }
}
#[derive(Debug, PartialEq)]
struct Point {
    history: PointHistory,
    context: ContextState,
}
#[derive(Debug, PartialEq)]
enum PointHistory {
    Live(Histories),
    Stored(StoredDelta),
}
#[derive(Debug)]
struct StoredDelta {
    entries: String,
    input: String,
    // Materialize in prefix order so even reverse-order forks share unchanged rows.
    decoded: std::sync::OnceLock<Histories>,
}
impl PartialEq for StoredDelta {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries && self.input == other.input
    }
}
impl PointHistory {
    fn decoded(&self) -> Option<&Histories> {
        match self {
            Self::Live(histories) => Some(histories),
            Self::Stored(delta) => delta.decoded.get(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ContextState {
    effort_baseline: Option<EffortBaseline>,
    summary: String,
    summarized_entries: usize,
    account: Option<String>,
    input_tokens: u64,
}

impl Points {
    fn histories_at(&self, index: usize) -> Result<&Histories> {
        if let Some(ready) = self.records[&index].history.decoded() {
            return Ok(ready);
        }
        let previous = self
            .records
            .range(..index)
            .rev()
            .find_map(|(i, p)| Some((i + 1, p.history.decoded()?)));
        let mut histories = previous.map(|(_, h)| h);
        for (i, p) in self
            .records
            .range(previous.map_or(0, |(start, _)| start)..=index)
        {
            let PointHistory::Stored(delta) = &p.history else {
                unreachable!("started after the last decoded checkpoint")
            };
            let (mut entries, mut input) = histories.cloned().unwrap_or_default();
            restore(&mut entries, &delta.entries)
                .with_context(|| format!("Invalid transcript at fork checkpoint {i}"))?;
            restore(&mut input, &delta.input)
                .with_context(|| format!("Invalid provider input at fork checkpoint {i}"))?;
            anyhow::ensure!(entries.len() == *i, "Invalid fork boundary");
            histories = Some(delta.decoded.get_or_init(|| (entries, input)));
        }
        Ok(histories.expect("requested fork point exists"))
    }
}

impl Session {
    pub fn can_fork(&self, index: usize) -> bool {
        self.points.records.contains_key(&index)
    }
    pub(crate) fn checkpoint(&mut self) -> Result<()> {
        // Include already queued observations in model order, without transferring
        // ownership of the runtime objects which produced them.
        let mut boundary = self.clone();
        boundary.consume_pending();
        // Resolve the predecessor for the SQL delta before taking the writer lock.
        if let Some((last, _)) = self.points.records.get_max() {
            self.points.histories_at(*last)?;
        }
        self.points.records.insert(
            self.entries.len(),
            Arc::new(Point {
                history: PointHistory::Live((boundary.entries, boundary.input)),
                context: ContextState {
                    effort_baseline: self.effort_baseline.clone(),
                    summary: self.summary.clone(),
                    summarized_entries: self.summarized_entries,
                    account: self.context_account.clone(),
                    input_tokens: self.usage.input,
                },
            }),
        );
        Ok(())
    }

    pub(crate) fn fork(&self, entry: &str, settings: &Settings) -> Result<Self> {
        let index = self
            .entries
            .index_of(entry)
            .context("Message no longer exists")?;
        let point = self
            .points
            .records
            .get(&index)
            .context("No exact checkpoint exists for this message")?;
        let selected = &self.entries[index];
        let EntryKind::User { files, images, .. } = &selected.kind else {
            bail!("Only user messages can be forked");
        };
        let excerpt: String =
            title_excerpt(&selected.text, 64).unwrap_or_else(|| "Image message".into());
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
        (fork.entries, fork.input) = self.points.histories_at(index)?.clone();
        fork.effort_baseline
            .clone_from(&point.context.effort_baseline);
        fork.summary.clone_from(&point.context.summary);
        fork.summarized_entries = point.context.summarized_entries;
        fork.context_account.clone_from(&point.context.account);
        fork.usage.input = point.context.input_tokens;
        fork.points.records = self
            .points
            .records
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
        .and_then(|s| s.points.records.get_max())
        .map_or(0, |(i, _)| i + 1);
    let empty = Histories::default();
    let mut previous = next
        .points
        .records
        .range(..start)
        .next_back()
        .map(|(_, p)| p);
    for (index, p) in next.points.records.range(start..) {
        let encoded;
        let (entries, input) = match &p.history {
            PointHistory::Stored(d) => (&d.entries, &d.input),
            PointHistory::Live((entries, input)) => {
                let base = previous.map_or(&empty, |p| {
                    p.history.decoded().expect("checkpoint baseline")
                });
                encoded = (
                    serde_json::to_string(&delta(&base.0, entries))?,
                    serde_json::to_string(&delta(&base.1, input))?,
                );
                (&encoded.0, &encoded.1)
            }
        };
        db.execute(
            "INSERT INTO fork_points VALUES (?1,?2,?3,?4,?5)",
            params![
                next.id,
                *index as i64,
                serde_json::to_string(&p.context)?,
                entries,
                input
            ],
        )?;
        previous = Some(p);
    }
    Ok(())
}
pub(super) fn load(db: &Connection, s: &mut Session) -> Result<()> {
    let mut stmt = db.prepare(
        "SELECT position,state,entries,input FROM fork_points WHERE session=?1 ORDER BY position",
    )?;
    let mut rows = stmt.query([&s.id])?;
    while let Some(row) = rows.next()? {
        let index = usize::try_from(row.get::<_, i64>(0)?)?;
        anyhow::ensure!(
            index < s.entries.len() && matches!(s.entries[index].kind, EntryKind::User { .. }),
            "Invalid fork boundary"
        );
        s.points.records.insert(
            index,
            Arc::new(Point {
                history: PointHistory::Stored(StoredDelta {
                    entries: row.get(2)?,
                    input: row.get(3)?,
                    decoded: Default::default(),
                }),
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
    fn unreadable_checkpoint_preserves_the_transcript_and_blocks_dependent_operations() -> Result<()>
    {
        let root = tempfile::tempdir()?;
        let db = Store::open(root.path().into())?;
        let settings = Settings::default();
        let mut session = Session::new(root.path().into(), &settings);
        session.checkpoint()?;
        session.accept("first".into());
        session.consume_pending();
        session.checkpoint()?;
        session.accept("second".into());
        db.save_session(&session)?;
        db.db.lock().unwrap().execute(
            "UPDATE fork_points SET input='{' WHERE session=?1 AND position=1",
            [&session.id],
        )?;

        let mut reopened = db.load_session(&session.id)?;
        reopened.draft = "Still editable".into();
        db.save_session(&reopened)?;
        let mut reopened = db.load_session(&session.id)?;
        assert_eq!(reopened.entries, session.entries);
        assert_eq!(reopened.draft, "Still editable");
        assert!(reopened.can_fork(1));
        assert!(reopened.fork(&session.entries[0].id, &settings).is_ok());
        assert!(reopened.fork(&session.entries[1].id, &settings).is_err());
        assert!(reopened.checkpoint().is_err());
        assert!(!reopened.can_fork(2));
        Ok(())
    }

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
            source.entries.push(Entry::new(
                EntryKind::Reply { final_at: None },
                "Prior response",
            ));
            source
                .input
                .push(json!({"role":"assistant","content":"Prior response"}));
            source.input.push(before.clone());
            source
                .entries
                .push(Entry::new(EntryKind::Notice, "original tool status"));
            source.checkpoint()?;
            source.accept_submission(
                "first".into(),
                UserMessage {
                    files: files.clone(),
                    ..message
                },
                false,
            );
            db.save_session(&source)?;
            // The next checkpoint must delta against the *stored* predecessor,
            // even after reopening and before there is a live tail to reuse.
            let reopened = db.load_session(&source.id)?;
            assert_eq!(source, reopened);
            source = db.load_session(&source.id)?;
            let saved = source.clone();
            source.consume_pending();
            source.accept_submission("steer".into(), "steer".into(), true);
            source.consume_pending();
            source.entries[1].text = "updated tool status".into();
            source.input = vec![compact.clone()].into();
            source.context_account = Some("prior".into());
            source.summary = "past summary".into();
            source.summarized_entries = source.entries.len();
            source.checkpoint()?;
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
