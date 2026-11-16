//! Regression coverage for the pinned editor/font patches, using real shaping.
use iced::advanced::graphics::text::{self, cosmic_text as cosmic};

fn font_fixture() -> (cosmic::FontSystem, cosmic::fontdb::ID, cosmic::fontdb::ID) {
    let mut global = text::font_system().write().unwrap();
    let db = global.raw().db();
    let sans = db
        .query(&cosmic::fontdb::Query {
            families: &[cosmic::Family::Name("DejaVu Sans")],
            ..Default::default()
        })
        .expect("Nix supplies DejaVu Sans");
    let sans = db.face(sans).unwrap().clone();
    drop(global);
    let mut db = cosmic::fontdb::Database::new();
    // The default face intentionally lacks Arabic. No host CJK installation is
    // needed to reproduce the non-exact fallback-weight regression.
    db.load_font_data(
        include_bytes!("../../../.build-deps/iced-graphics/fonts/Iced-Icons.ttf").to_vec(),
    );
    let icons = db.faces().next().unwrap().families[0].0.clone();
    db.set_sans_serif_family(icons);
    let mut decoy = sans.clone();
    decoy.families[0].0 = "Unrelated exact-weight face".into();
    let decoy = db.push_face_info(decoy);
    let mut preferred = sans;
    preferred.families[0].0 = "Noto Sans Arabic".into();
    preferred.weight = cosmic::Weight(350);
    let preferred = db.push_face_info(preferred);
    (
        cosmic::FontSystem::new_with_locale_and_db("en-US".into(), db),
        preferred,
        decoy,
    )
}

fn shape(fonts: &mut cosmic::FontSystem, value: &str) -> cosmic::Buffer {
    let mut buffer = cosmic::Buffer::new(fonts, cosmic::Metrics::new(16., 24.));
    buffer.set_size(fonts, Some(180.), Some(600.));
    buffer.set_text(
        fonts,
        value,
        &cosmic::Attrs::new(),
        cosmic::Shaping::Advanced,
        None,
    );
    buffer
}

#[test]
fn preferred_fallback_accepts_nearby_weight_and_font_changes_invalidate_shaping() {
    let (mut fonts, preferred, decoy) = font_fixture();
    let value = "لَا"; // Lam-alef with a combining mark: preserve clustered RTL shaping.
    let buffer = shape(&mut fonts, value);
    let glyphs: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs).collect();
    assert!(!glyphs.is_empty());
    assert!(
        glyphs
            .iter()
            .all(|glyph| glyph.glyph_id != 0 && glyph.font_id == preferred)
    );
    fonts.db_mut().remove_face(preferred);
    let buffer = shape(&mut fonts, value);
    assert!(
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .all(|glyph| glyph.glyph_id != 0 && glyph.font_id == decoy),
        "cached shaping must not retain a removed fallback face"
    );
}

#[test]
fn cached_shaping_matches_fresh_for_bidi_clusters_attributes_and_wraps() {
    let (mut fonts, _, _) = font_fixture();
    for value in [
        "(123) fi λ",
        "אב (123) fi",
        "لَا (123)",
        "λ fi\t(123) e\u{301}",
    ] {
        for weight in [cosmic::Weight::NORMAL, cosmic::Weight::BOLD] {
            let attrs = cosmic::Attrs::new().weight(weight).metadata(17);
            let layout = |fonts: &mut cosmic::FontSystem| {
                let mut buffer = shape(fonts, value);
                buffer.set_text(fonts, value, &attrs, cosmic::Shaping::Advanced, None);
                buffer.set_size(fonts, Some(60.), Some(600.));
                buffer
                    .layout_runs()
                    .map(|run| {
                        (
                            run.line_y,
                            run.line_w,
                            run.glyphs
                                .iter()
                                .map(|g| {
                                    (
                                        g.start, g.end, g.font_id, g.glyph_id, g.x, g.y, g.w,
                                        g.level, g.metadata,
                                    )
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let warm = layout(&mut fonts);
            // Independent fresh shaping is the reference, not a stored snapshot.
            fonts.shape_run_cache = cosmic::ShapeRunCache::default();
            let cold = layout(&mut fonts);
            assert_eq!(
                warm, cold,
                "cache must preserve direction, clusters and attributes: {value}"
            );
        }
    }
}
