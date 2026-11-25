use super::*;
use crate::ui::worktrees::Action;

fn git(path: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(path)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=test@localhost",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn repo_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--template=", "-b", "main"]);
    git(dir.path(), &["commit", "--allow-empty", "-m", "base"]);
    dir
}

async fn click(app: &mut App, label: &str) {
    let messages = {
        let mut renderer = header_renderer();
        let mut ui = iced_runtime::UserInterface::build(
            app.view(),
            Size::new(app.window_width, app.window_height),
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        click_at(&mut ui, &mut renderer, bounds.label(label).center())
    };
    assert!(!messages.is_empty(), "{label} should be actionable");
    for m in messages {
        dispatch(app, m).await;
    }
}

#[tokio::test]
async fn sidebar_labels_visible_checkouts_even_without_a_selected_session() {
    use iced::futures::{StreamExt, stream};
    use iced_runtime::futures::subscription;

    let mut app = app();
    let dir = repo_fixture();
    let root = dir.path();
    git(root, &["branch", "-m", "trunk"]);
    git(root, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );
    let linked = root.join("linked");
    git(
        root,
        &[
            "worktree",
            "add",
            "-b",
            "topic/map",
            linked.to_str().unwrap(),
        ],
    );
    app.catalog.projects.push(Project {
        path: root.into(),
        display_name: Some("Atlas".into()),
        favorite: false,
        stashed: false,
    });
    app.catalog
        .worktree_origins
        .insert(linked.clone(), root.into());
    for path in [root, &linked] {
        app.sessions.push(SessionSummary::from(&Session::new(
            path.into(),
            &app.catalog.settings,
        )));
    }
    app.prepare_sidebar();
    assert!(app.current().is_none());
    let mut messages = stream::select_all(
        subscription::into_recipes(app.subscription())
            .into_iter()
            .map(|recipe| recipe.stream(stream::pending().boxed())),
    )
    .filter(|msg| std::future::ready(matches!(msg, Message::SidebarBranch(..))));
    tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..2 {
            let _ = app.update(messages.next().await.unwrap());
        }
    })
    .await
    .unwrap();
    let labels = displayed_text(app.sidebar(), Size::new(320., 640.));
    assert!(labels.contains("Atlas · topic/map"));
    assert!(!labels.contains("Atlas · trunk"));
}

#[tokio::test]
async fn pending_picker_stays_hidden_and_escape_cancels_the_open() {
    let mut app = app();
    let dir = repo_fixture();
    dispatch(&mut app, Message::Create(dir.path().into())).await;

    // Hold the request so rendering and dismissal don't depend on Git's speed.
    let load = app.update(Message::Worktree(Action::Popup(0.)));
    let labels =
        |app: &App| displayed_text(app.view(), Size::new(app.window_width, app.window_height));
    let pending = labels(&app);
    assert!(pending.contains("main"));
    assert!(!pending.contains("Loading worktrees") && !pending.contains("New worktree"));

    dispatch(&mut app, footer_key(keyboard::key::Named::Escape)).await;
    drain(&mut app, load).await;
    assert!(
        !labels(&app).contains("New worktree"),
        "late completion reopened the picker"
    );
}

#[tokio::test]
async fn footer_selects_without_git_changes_and_quick_create_can_retry_only_the_handoff() {
    let mut app = app();
    let dir = repo_fixture();
    let main = dir.path();
    let ext = dir.path().join("linked");
    git(
        main,
        &["worktree", "add", "-b", "contours", ext.to_str().unwrap()],
    );
    git(&ext, &["commit", "--allow-empty", "-m", "other base"]);
    std::fs::write(ext.join("notes"), "do not touch").unwrap();
    app.catalog.projects.push(Project {
        path: ext.clone(),
        display_name: Some("Contour Studio".into()),
        favorite: false,
        stashed: false,
    });
    let before = git(main, &["worktree", "list", "--porcelain"]);
    dispatch(&mut app, Message::Create(main.into())).await;
    type_message(&mut app, "compare the labels").await;
    let first = app.current().unwrap().id.clone();
    click(&mut app, "main").await;
    assert!(!app.worktrees.panel);
    click(&mut app, "contours").await;
    assert_ne!(app.current().unwrap().id, first);
    assert_eq!(app.current().unwrap().project, ext);
    assert_eq!(app.editor.text(), "compare the labels");
    assert_eq!(git(main, &["worktree", "list", "--porcelain"]), before);
    assert_eq!(std::fs::read(ext.join("notes")).unwrap(), b"do not touch");

    click(&mut app, "contours").await;
    let store = app.harness.as_ref().unwrap().store.clone();
    let db = rusqlite::Connection::open(store.root().join("sessions.sqlite3")).unwrap();
    sqlite::fail_writes(&db, "new_session_draft", "handoff fault");
    click(&mut app, "New worktree").await;
    assert_eq!(app.current().unwrap().project, ext);
    assert_eq!(app.editor.text(), "compare the labels");
    assert!(app.error.as_ref().unwrap().contains("handoff fault"));
    let created = git(main, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        created
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count(),
        3
    );
    sqlite::allow_writes(&db, "new_session_draft");
    click(&mut app, "Retry starting session").await;
    let path = &app.current().unwrap().project;
    assert!(path.starts_with(store.root().join("worktrees")));
    assert_eq!(
        git(path, &["rev-parse", "HEAD"]),
        git(main, &["rev-parse", "HEAD"])
    );
    assert_eq!(git(main, &["worktree", "list", "--porcelain"]), created);
    assert_eq!(
        store.load_new_draft().unwrap().message,
        "compare the labels"
    );
    assert_eq!(store.load_new_draft().unwrap().project.as_ref(), Some(path));

    // Reopen from a different project using only the persisted association.
    let branch = path.file_name().unwrap().to_str().unwrap().to_owned();
    app.catalog = store.load().unwrap().0;
    dispatch(&mut app, Message::Create(main.into())).await;
    click(&mut app, "main").await;
    click(&mut app, &branch).await;
    let labels = displayed_text(app.center(), Size::new(900., 700.));
    assert!(labels.contains("Contour Studio"));
    assert!(labels.contains("CS"));
}

#[tokio::test]
async fn panel_selects_a_base_without_moving_the_original_chat_or_its_draft() {
    let mut app = app();
    let dir = repo_fixture();
    let path = dir.path();
    git(path, &["branch", "candidate"]);
    git(path, &["commit", "--allow-empty", "-m", "advance main"]);
    stored_session(&mut app, path.into()).await;
    let session = app.current().unwrap().id.clone();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&session, |s| {
        s.entries
            .push(Entry::new(EntryKind::Assistant, "Original chat"));
    })
    .unwrap();
    app.refresh();
    type_message(&mut app, "keep here").await;

    dispatch(&mut app, Message::Worktree(Action::Popup(0.))).await;
    assert!(app.worktrees.popup.is_none());
    dispatch(&mut app, Message::Worktree(Action::Panel)).await;
    let before = git(path, &["worktree", "list", "--porcelain"]);
    click(&mut app, "New worktree").await;
    assert!(app.worktrees.bases);
    dispatch(&mut app, Message::Worktree(Action::Bases)).await;
    assert_eq!(git(path, &["worktree", "list", "--porcelain"]), before);
    click(&mut app, "New worktree").await;
    click(&mut app, "candidate").await;
    let current = app.current().unwrap();
    assert_ne!(current.id, session);
    assert_eq!(
        git(&current.project, &["rev-parse", "HEAD"]),
        git(path, &["rev-parse", "candidate"])
    );
    assert!(app.editor.is_empty());
    let saved = h.store.load_session(&session).unwrap();
    assert_eq!(saved.project, path);
    assert_eq!(saved.draft, "keep here");
    assert_eq!(saved.entries.len(), 1);
}

#[tokio::test]
async fn removing_an_idle_checkout_keeps_the_chat_readable_but_prevents_send() {
    let mut app = app();
    let repo = repo_fixture();
    let main = repo.path();
    let store = app.harness.as_ref().unwrap().store.clone();
    let worktrees = crate::worktrees::Repo::read(main, store.root())
        .await
        .unwrap();
    let path = worktrees.create("main".as_ref()).await.unwrap();
    stored_session(&mut app, path.clone()).await;
    let id = app.current().unwrap().id.clone();
    let h = app.harness.as_ref().unwrap().clone();
    h.edit(&id, |s| {
        s.entries
            .push(Entry::new(EntryKind::Assistant, "Saved transcript"));
    })
    .unwrap();
    type_message(&mut app, "keep this draft").await;
    dispatch(&mut app, Message::Create(main.into())).await;
    dispatch(&mut app, Message::Worktree(Action::Panel)).await;
    dispatch(&mut app, Message::Worktree(Action::Row(path.clone()))).await;
    click(&mut app, "Remove worktree…").await;
    assert!(path.is_dir(), "confirmation must precede removal");
    click(&mut app, "Remove").await;
    assert!(!path.exists());
    dispatch(&mut app, Message::Select(id.clone())).await;
    assert_eq!(app.editor.text(), "keep this draft");
    send_current(&mut app).await;
    assert!(app.error.as_ref().unwrap().contains("worktree"));
    let saved = store.load_session(&id).unwrap();
    assert_eq!(saved.draft, "keep this draft");
    assert_eq!(saved.entries.len(), 1);
    assert!(!path.exists());
}

#[tokio::test]
async fn picker_pins_main_and_scrolls_four_rows_in_creation_not_commit_order() {
    let mut app = app();
    let repo = repo_fixture();
    let root = repo.path();
    for (index, name) in ["older", "b", "c", "d", "e", "latest"].iter().enumerate() {
        let path = root.join(name);
        let output = std::process::Command::new("git")
            .current_dir(root)
            .env(
                "GIT_COMMITTER_DATE",
                format!("2020-01-0{}T12:00:00Z", index + 1),
            )
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "-b",
                name,
                path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    git(
        &root.join("older"),
        &["commit", "--allow-empty", "-m", "new commit on old branch"],
    );
    dispatch(&mut app, Message::Create(root.into())).await;
    dispatch(&mut app, Message::Worktree(Action::Popup(0.))).await;
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.worktree_popup(240.).unwrap(),
        Size::new(240., 600.),
        Default::default(),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let bounds_of = |name: &str| bounds.label(name);
    let scroll = bounds
        .scrolls
        .iter()
        .find(|(id, _)| *id == iced::advanced::widget::Id::new("worktree-options"))
        .unwrap()
        .1;
    assert!(bounds_of("main").center_y() < scroll.y);
    // Four complete rows fit, with or without gaps between rows.
    for name in ["latest", "e", "d", "c"] {
        let label = bounds_of(name);
        assert!(label.y >= scroll.y && label.y + label.height <= scroll.y + scroll.height);
    }
    assert!(bounds_of("latest").y < bounds_of("e").y);
    assert!(bounds_of("b").y >= scroll.y + scroll.height);
    assert!(bounds_of("older").y > bounds_of("b").y);
    assert!(bounds_of("New worktree").center_y() > scroll.y + scroll.height);
}

#[tokio::test]
async fn panel_checkout_details_align_and_the_creation_button_leaves_the_gutter_clear() {
    let mut app = app();
    let repo = repo_fixture();
    dispatch(&mut app, Message::Create(repo.path().into())).await;
    dispatch(&mut app, Message::Worktree(Action::Panel)).await;
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        app.worktrees_panel(),
        Size::new(app.panel_width(), 500.),
        Default::default(),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    let heading_x = bounds
        .labels
        .iter()
        .find(|(label, _)| label.starts_with("Checkouts"))
        .unwrap()
        .1
        .x;
    let gutter = iced::Point::new(
        app.panel_width() - theme::SCROLLBAR_EDGE_INSET - crate::scrollbar::GUTTER / 2.,
        bounds.label("New worktree").center_y(),
    );
    assert!(click_at(&mut ui, &mut renderer, gutter).is_empty());
    for label in [
        "main",
        repo.path().to_str().unwrap(),
        "Primary · This session",
    ] {
        assert_eq!(bounds.label(label).x, heading_x, "{label}");
    }
}

#[tokio::test]
async fn worktree_picker_waits_for_the_draft_attachment_import() {
    let mut app = app();
    let repo = repo_fixture();
    dispatch(&mut app, Message::Create(repo.path().into())).await;
    let path = repo.path().join("attachment.png");
    std::fs::write(&path, include_bytes!("../../fixtures/media.png")).unwrap();
    let import = app.update(Message::Event(iced::Event::Window(
        window::Event::FileDropped(path),
    )));
    dispatch(&mut app, Message::Worktree(Action::Popup(0.))).await;

    let mut renderer = header_renderer();
    let mut check_choices = |app: &App, enabled| {
        let mut ui = iced_runtime::UserInterface::build(
            app.worktree_popup(240.).unwrap(),
            Size::new(240., 600.),
            Default::default(),
            &mut renderer,
        );
        let mut bounds = UiBounds::default();
        ui.operate(&renderer, &mut bounds);
        for label in ["main", "New worktree"] {
            let messages = click_at(&mut ui, &mut renderer, bounds.label(label).center());
            assert_eq!(
                matches!(messages.as_slice(), [Message::Worktree(Action::Run(_))]),
                enabled,
                "{label}"
            );
        }
    };
    check_choices(&app, false);
    drain(&mut app, import).await;
    assert_eq!(app.attachments.len(), 1);
    check_choices(&app, true);
}
