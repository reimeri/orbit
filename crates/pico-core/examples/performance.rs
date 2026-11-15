//! Isolated release benchmark; no credentials, network, or user state.
//! cargo run --locked --release -p pico-core --example performance
use anyhow::Result;
use pico_core::{
    agent::Harness,
    store::{Entry, EntryKind, Session, SessionSummary, Settings, Store, atomic_write},
};
use serde_json::json;
#[derive(serde::Serialize)]
struct FullSnapshot<'a> {
    #[serde(flatten)]
    metadata: &'a Session,
    entries: &'a pico_core::history::History<Entry>,
    input: &'a pico_core::history::History<serde_json::Value>,
}
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
#[allow(dead_code)]
#[path = "../../pico-app/src/virtual_list.rs"]
mod virtual_list;
fn timed(label: &str, n: usize, mut f: impl FnMut() -> Result<()>) -> Result<()> {
    let start = Instant::now();
    for _ in 0..n {
        f()?;
    }
    println!(
        "{label}: {:.6} ms/op ({n} iterations)",
        start.elapsed().as_secs_f64() * 1000. / n as f64
    );
    Ok(())
}
fn main() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().into())?;
    let mut session = Session::new(dir.path().into(), &Settings::default());
    for i in 0..20000 {
        session.entries.push(Entry::new(
            if i % 3 == 0 {
                EntryKind::User {
                    images: vec![],
                    steering: false,
                }
            } else {
                EntryKind::Assistant
            },
            format!("Message {i}: {}", "asymmetric λ text ".repeat(55)),
        ));
    }
    for i in 0..2000 {
        session.input.push(
            json!({"role":"user","content":format!("Context {i}: {}","context ".repeat(60))}),
        );
    }
    store.save_session(&session)?;
    println!("Fixture: 20,000 transcript entries; 2,000 provider context items");
    timed("Baseline deep transcript/context clone", 20, || {
        black_box(session.entries.iter().cloned().collect::<Vec<_>>());
        black_box(session.input.iter().cloned().collect::<Vec<_>>());
        Ok(())
    })?;
    timed("Shared full-session snapshot", 10000, || {
        black_box(session.clone());
        Ok(())
    })?;
    timed("Baseline full JSON durable rewrite", 5, || {
        atomic_write(
            &dir.path().join("baseline.json"),
            &serde_json::to_vec_pretty(&FullSnapshot {
                metadata: &session,
                entries: &session.entries,
                input: &session.input,
            })?,
        )
    })?;
    timed("Incremental durable draft update", 100, || {
        let before = session.clone();
        session.draft.push('x');
        store.save_changes(Some(&before), &session)
    })?;
    timed("Incremental durable one-entry update", 100, || {
        let before = session.clone();
        session.entries[19999].text.push('x');
        store.save_changes(Some(&before), &session)
    })?;
    // Build metadata fixture in one transaction: setup durability is not measured.
    let mut db = rusqlite::Connection::open(store.root().join("sessions.sqlite3"))?;
    let tx = db.transaction()?;
    {
        let mut insert = tx.prepare("INSERT INTO sessions VALUES (?1,?2,?3,?4)")?;
        for i in 1..10000 {
            let mut small = Session::new(dir.path().into(), &Settings::default());
            small.title = format!("Archived session {i}");
            insert.execute(rusqlite::params![
                small.id,
                small.updated as i64,
                serde_json::to_string(&SessionSummary::from(&small))?,
                serde_json::to_string(&small)?
            ])?;
        }
    }
    tx.commit()?;
    timed("Load 10,000 sidebar summaries (no transcripts)", 10, || {
        let (_, summaries, _) = store.load()?;
        assert_eq!(summaries.len(), 10000);
        black_box(summaries);
        Ok(())
    })?;
    let (_, summaries, _) = store.load()?;
    let harness = Harness::new(store.clone(), summaries)?;
    assert!(harness.all().is_empty());
    timed("Load selected 20,000-entry session", 5, || {
        black_box(store.load_session(&session.id)?);
        Ok(())
    })?;
    let mut list = virtual_list::List::default();
    list.reset((0..20000).map(|i| (i, if i % 3 == 0 { 63. } else { 187. })));
    list.viewport = 720.;
    list.offset = list.prefix(15000);
    println!(
        "Visible/overscan rows at entry 15,000: {} of 20,000",
        list.visible().len()
    );
    timed("Indexed viewport lookup", 100000, || {
        black_box(list.visible());
        Ok(())
    })?;
    timed("Incremental height-index append", 10000, || {
        let i = list.order.len();
        list.push(i, 123.);
        Ok(())
    })?;
    // Pending timers should not cause periodic 2ms polling CPU work.
    let mode = pico_core::code_mode::CodeMode::new(
        std::sync::Arc::new(pico_core::shell::Shell::new(dir.path().into())),
        std::sync::Arc::new(|_| Ok(())),
    );
    let result = mode.exec_output(
        "// @exec: {\"yield_time_ms\": 1}\nawait new Promise(r=>setTimeout(r,1100));text('done');",
        "timer",
        &pico_core::CancellationToken::new(),
    )?;
    let result = result.summary();
    let id = result
        .lines()
        .find_map(|s| s.strip_prefix("Script running with cell ID "))
        .expect("pending timer");
    let before = cpu_ticks()?;
    std::thread::sleep(Duration::from_millis(1000));
    let after = cpu_ticks()?;
    println!(
        "Process CPU ticks during 1s awaiting JS timer: {} (Linux /proc/self/stat user+system)",
        after - before
    );
    black_box(mode.wait_output(
        &json!({"cell_id":id}).to_string(),
        &pico_core::CancellationToken::new(),
    )?);
    Ok(())
}
fn cpu_ticks() -> Result<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .expect("stat command")
        .1
        .split_whitespace()
        .collect();
    Ok(fields[11].parse::<u64>()? + fields[12].parse::<u64>()?)
}
