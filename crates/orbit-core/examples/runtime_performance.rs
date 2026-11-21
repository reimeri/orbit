//! Offline probes for the app performance audit. Run in release, outside the UI.
use anyhow::{Result, ensure};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use orbit_core::{CancellationToken, code_mode::CodeMode, media, shell::Shell};
use serde_json::json;
use std::{hint::black_box, io::Cursor, sync::Arc, time::Instant};

fn measure(mut f: impl FnMut() -> Result<()>, label: &str, n: usize) -> Result<()> {
    for i in 0..=n {
        let start = Instant::now();
        f()?;
        if i > 0 {
            println!("{label},{i},{:.3}", start.elapsed().as_secs_f64() * 1000.);
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mode = CodeMode::new(
        Arc::new(Shell::new(dir.path().into())),
        Arc::new(|_| Ok(())),
    );
    let cancel = CancellationToken::new();
    println!("case,sample,ms");
    for (label, source) in [
        ("cell", "text('ok')"),
        (
            "cell+shell",
            "text(await tools.exec_command({cmd:'printf ok',shell:'/bin/sh',login:false}))",
        ),
    ] {
        measure(
            || {
                let out = mode.exec_output(source, "audit", &cancel)?;
                ensure!(!out.failed && out.summary().contains("ok"));
                Ok(())
            },
            label,
            9,
        )?;
    }

    for (w, h) in [(1280, 720), (3840, 2160)] {
        // Reproducible photographic-noise stressor, not a screenshot compression model.
        let mut seed = 7_u32;
        let pixels = RgbImage::from_fn(w, h, |_, _| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let [_, r, g, b] = seed.to_le_bytes();
            Rgb([r, g, b])
        });
        let mut jpeg = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(pixels).write_to(&mut jpeg, ImageFormat::Jpeg)?;
        let mut destinations = Vec::new();
        let mut media = None;
        measure(
            || {
                let dest = tempfile::tempdir_in(&dir)?;
                media = Some(media::MediaStore::new(dest.path().into()).image(jpeg.get_ref())?);
                destinations.push(dest);
                Ok(())
            },
            &format!("import-{w}"),
            5,
        )?;
        let image = media.unwrap();
        let prepared = image.prepared.as_ref().unwrap().metadata()?.len();
        eprintln!("{w}x{h}: jpeg={} prepared={prepared}", jpeg.get_ref().len());
        measure(
            || {
                let mut frame = json!({"input":[{"role":"user","content":[{"type":"orbit_media","media":image}]}]});
                media::project(&mut frame, false)?;
                let wire = serde_json::to_string(&frame)?;
                ensure!(wire.len() > prepared as usize);
                black_box(wire);
                Ok(())
            },
            &format!("project+serialize-{w}"),
            5,
        )?;
    }
    Ok(())
}
