//! Offline scene for the marketing screenshot. Commands are transcript data only.
use super::*;
use iced::futures::FutureExt;

impl App {
    pub(super) fn stage_marketing_session(&mut self) {
        let project = PathBuf::from("/workspace/atlas");
        let harness = self.harness.as_ref().expect("isolated capture storage");
        let mut session = Session::new(project.clone(), &self.settings);
        session.title = "Keep map tiles warm".into();
        for (title, minutes_ago) in [
            ("Tune the contour palette", 24),
            ("Label trail junctions", 85),
            ("Import elevation tiles", 180),
        ] {
            let mut earlier = Session::new(project.clone(), &self.settings);
            earlier.title = title.into();
            earlier.updated = session.updated - minutes_ago * 60;
            harness.insert(earlier);
        }

        let mut waiting = Session::new(project.clone(), &self.settings);
        waiting.title = "Choose a distance unit".into();
        waiting.updated = session.updated - 5 * 60;
        let waiting = harness.insert(waiting);
        waiting.running.store(true, Ordering::Release);
        let mut question = Box::pin(async move {
            waiting.questionnaires.as_ref().expect("main session").ask(
                    "distance-unit",
                    r#"{"questions":[{"id":"unit","kind":"select","question":"Which unit should trail distances use by default?","options":["Kilometers","Miles"]}]}"#,
                    &CancellationToken::new(),
                ).await.expect("valid staged question")
        });
        // Register the pending question before the first sidebar layout, so the
        // screenshot cannot catch the row moving from Working to Needs Attention.
        assert!(question.as_mut().now_or_never().is_none());
        self.tasks
            .push(Task::perform(question, |_| Message::Changed));

        let command = |cmd: &str, output: &str, complete: bool| {
            Entry::new(
                EntryKind::Tool {
                    name: "exec_command".into(),
                    input: serde_json::json!({ "cmd": cmd }).to_string(),
                    output: output.into(),
                    complete,
                    failed: false,
                    command_status: None,
                    cell_id: None,
                    parent: None,
                    media: vec![],
                    web: None,
                },
                "",
            )
        };
        let inspect = command(
            "rg -n 'TileCache' src/map",
            "src/map/cache.rs:18:pub struct TileCache {",
            true,
        );
        let test = command(
            "cargo test --test tile_cache",
            "running 3 tests\ntest evicts_the_oldest_tile ... ok\ntest keeps_visible_tiles ... ok\ntest reuses_tiles_after_pan ... ok",
            true,
        );
        self.groups.extend([inspect.id.clone(), test.id.clone()]);
        self.expanded.insert(test.id.clone());
        session.entries = vec![
            Entry::new(EntryKind::User { skills: vec![], files: vec![], images: vec![], sent_at: None, steering: false }, "Panning back to a trail reloads the same map tiles. Can we reuse them without growing memory usage?"),
            Entry::new(EntryKind::Assistant, "I’ll trace how tiles leave the viewport, then add a test for panning away and back."),
            inspect,
            Entry::new(EntryKind::Assistant, "The renderer was dropping tiles as soon as they left the screen. I’ve added a **bounded tile cache**: visible tiles stay pinned, and the oldest offscreen tile makes room for the next one."),
            test,
            command("cargo bench --bench map_pan", "", false),
        ].into();
        self.git.insert(
            project,
            crate::git::Info::Changes {
                branch: "tile-cache".into(),
                added: 48,
                removed: 12,
            },
        );
        self.selected = Some(session.id.clone());
        harness
            .insert(session)
            .running
            .store(true, Ordering::Release);
        self.refresh();
    }
}
