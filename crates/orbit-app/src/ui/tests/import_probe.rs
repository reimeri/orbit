//! Local scheduling probe, not a frame-presentation benchmark. Run alone in release mode.
use super::*;
use iced::futures::{FutureExt, StreamExt, stream::SelectAll};
use std::time::Instant;

fn pss_kib() -> usize {
    let rollup = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
    rollup
        .lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test]
#[ignore = "local two-import latency and PSS measurement"]
async fn two_4k_import_probe() {
    let mut app = app();
    let root = app.smoke_dir.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let destination = app.selected.clone().unwrap();
    stored_session(&mut app, root.clone()).await;
    let owner = app.selected.clone().unwrap();

    let paths = [7_u32, 19].map(|mut seed| {
        let path = root.join(format!("noise-{seed}.jpg"));
        image::RgbImage::from_fn(3840, 2160, |_, _| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let [_, r, g, b] = seed.to_le_bytes();
            image::Rgb([r, g, b])
        })
        .save(&path)
        .unwrap();
        path
    });
    let baseline = pss_kib();
    let (done, stop) = std::sync::mpsc::channel::<()>();
    let sampler = std::thread::spawn(move || {
        let mut peak = baseline;
        loop {
            peak = peak.max(pss_kib());
            if !matches!(
                stop.recv_timeout(Duration::from_millis(5)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                return peak;
            }
        }
    });
    let start = Instant::now();
    let mut tasks = SelectAll::new();
    for path in paths {
        if let Some(stream) = iced_runtime::task::into_stream(app.update(Message::Event(
            iced::Event::Window(window::Event::FileDropped(path)),
        ))) {
            tasks.push(stream);
        }
    }
    // Start polling the first real import before requesting the switch.
    let mut first_action = tasks.next().now_or_never().flatten();
    let switch = Instant::now();
    tasks.push(
        iced_runtime::task::into_stream(app.update(Message::Select(destination.clone()))).unwrap(),
    );
    let mut ready = None;
    let mut prepared = vec![];
    while !app.imports.is_empty() || ready.is_none() {
        let action = match first_action.take() {
            Some(action) => action,
            None => tasks.next().await.expect("pending import or selection"),
        };
        if let iced_runtime::Action::Output(message) = action {
            let selected = matches!(message, Message::Selected(_));
            if matches!(message, Message::AttachmentLoaded(_)) {
                prepared.push(start.elapsed());
            }
            let task = app.update(message);
            if selected {
                ready = Some(switch.elapsed());
                assert_eq!(app.selected.as_ref(), Some(&destination));
            }
            if let Some(stream) = iced_runtime::task::into_stream(task) {
                tasks.push(stream);
            }
        }
    }
    let ready = ready.unwrap();
    let published = start.elapsed();
    done.send(()).unwrap();
    let peak = sampler.join().unwrap();
    let saved = app
        .harness
        .as_ref()
        .unwrap()
        .store
        .load_session(&owner)
        .unwrap();
    assert!(app.attachments.is_empty());
    assert_eq!(saved.draft_images.len(), 2);
    assert!(
        saved
            .draft_images
            .iter()
            .all(|m| (m.width, m.height) == (3840, 2160))
    );
    eprintln!(
        "selected={ready:?} prepared={prepared:?} both_published={published:?} baseline_pss={baseline} KiB peak_pss={peak} KiB"
    );
}
