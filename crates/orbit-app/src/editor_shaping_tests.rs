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
    let face = fonts.db().face(preferred).unwrap().clone();
    fonts.db_mut().remove_face(preferred);
    let buffer = shape(&mut fonts, value);
    assert!(
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .all(|glyph| glyph.glyph_id != 0 && glyph.font_id == decoy),
        "cached shaping must not retain a removed fallback face"
    );
    let restored = fonts.db_mut().push_face_info(face);
    let buffer = shape(&mut fonts, value);
    assert!(
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .all(|glyph| glyph.glyph_id != 0 && glyph.font_id == restored),
        "a previously absent fallback family must become selectable"
    );
}

#[test]
fn monospace_fallback_opens_faces_in_rank_order_only_as_needed() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct FaceBytes(Vec<u8>, Arc<AtomicBool>);
    impl AsRef<[u8]> for FaceBytes {
        fn as_ref(&self) -> &[u8] {
            self.1.store(true, Ordering::Relaxed);
            &self.0
        }
    }

    let mut db = cosmic::fontdb::Database::new();
    let bytes = include_bytes!("../../../.build-deps/iced-graphics/fonts/Iced-Icons.ttf");
    db.load_font_data(bytes.to_vec());
    let mut face = db.faces().next().unwrap().clone();
    db.remove_face(face.id);
    face.monospaced = true;
    db.set_monospace_family("Preferred");
    face.families[0].0 = "Preferred".into();
    let preferred = db.push_face_info(face.clone());
    face.families[0].0 = "Unreadable candidate".into();
    face.source = cosmic::fontdb::Source::Binary(Arc::new(Vec::<u8>::new()));
    db.push_face_info(face.clone());
    face.families[0].0 = "Readable alternative".into();
    face.weight = cosmic::Weight::LIGHT;
    let opened = Arc::new(AtomicBool::new(false));
    face.source =
        cosmic::fontdb::Source::Binary(Arc::new(FaceBytes(bytes.to_vec(), opened.clone())));
    let alternative = db.push_face_info(face);
    let mut fonts = cosmic::FontSystem::new_with_locale_and_db("en-US".into(), db);
    let attrs = cosmic::Attrs::new().family(cosmic::Family::Monospace);
    let matches = fonts.get_font_matches(&attrs);
    let families = [&attrs.family];
    let mut candidates = cosmic::fallback::FontFallbackIter::new(
        &mut fonts,
        &matches,
        &families,
        &[],
        "word",
        attrs.weight,
    );
    assert_eq!(candidates.next().unwrap().id(), preferred);
    assert!(
        !opened.load(Ordering::Relaxed),
        "ranking read an unused face"
    );
    assert_eq!(candidates.next().unwrap().id(), alternative);
    assert!(opened.load(Ordering::Relaxed));
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
            fonts.shape_run_cache = cosmic::ShapeRunCache::default();
            let cold = layout(&mut fonts);
            assert_eq!(
                warm, cold,
                "cache must preserve direction, clusters and attributes: {value}"
            );
        }
    }
}

// Compare the complete paragraph, not just the currently visible viewport.
// Fresh construction plus an empty run cache is the independent reference.
fn assert_matches_fresh_line(fonts: &mut cosmic::FontSystem, line: &mut cosmic::BufferLine) {
    for width in [113., 347.] {
        line.reset_layout();
        let actual = format!(
            "{:?}",
            line.layout(fonts, 16., Some(width), cosmic::Wrap::WordOrGlyph, None, 4)
        );
        let mut fresh = cosmic::BufferLine::new(
            line.text(),
            line.ending(),
            line.attrs_list().clone(),
            cosmic::Shaping::Advanced,
        );
        fresh.set_align(line.align());
        fonts.shape_run_cache = cosmic::ShapeRunCache::default();
        let expected = format!(
            "{:?}",
            fresh.layout(fonts, 16., Some(width), cosmic::Wrap::WordOrGlyph, None, 4)
        );
        assert_eq!(actual, expected, "edited paragraph must match fresh layout");
    }
}

#[test]
fn retained_words_match_fresh_across_grapheme_bidi_and_tab_changes() {
    let (mut fonts, _, _) = font_fixture();
    let attrs = cosmic::AttrsList::new(&cosmic::Attrs::new().metadata(7));
    for (initial, additions) in [
        ("prefix fi λ界 words e", ["\u{301}", " x", "y"]),
        ("אב fi لَا\t(12) words ", ["ב", " \tfi", " אב"]),
        ("(123) \t", ["אב", " fi", "\u{301}"]),
        ("a \u{2067}אב (12)\t", ["\u{2069} L", "\t", "界"]),
    ] {
        let mut expected = initial.to_owned();
        let mut line = cosmic::BufferLine::new(
            &expected,
            cosmic::LineEnding::None,
            attrs.clone(),
            cosmic::Shaping::Advanced,
        );
        assert_matches_fresh_line(&mut fonts, &mut line);
        for addition in additions {
            line.append(&cosmic::BufferLine::new(
                addition,
                cosmic::LineEnding::None,
                attrs.clone(),
                cosmic::Shaping::Advanced,
            ));
            expected.push_str(addition);
            assert_eq!(line.text(), expected);
            assert_matches_fresh_line(&mut fonts, &mut line);
        }
        // Remove the tail, then append before shaping: the earliest unchanged
        // prefix must survive both mutations, not just the most recent append.
        expected.pop();
        let _removed = line.split_off(expected.len());
        line.append(&cosmic::BufferLine::new(
            " end",
            cosmic::LineEnding::None,
            attrs.clone(),
            cosmic::Shaping::Advanced,
        ));
        expected.push_str(" end");
        assert_eq!(line.text(), expected);
        assert_matches_fresh_line(&mut fonts, &mut line);
    }
    // The replacement occupies the same byte range as the old word. Advancing
    // the reusable prefix on the second append would paint "old" as "new".
    let mut line = cosmic::BufferLine::new(
        "keep old tail ",
        cosmic::LineEnding::None,
        attrs.clone(),
        cosmic::Shaping::Advanced,
    );
    assert_matches_fresh_line(&mut fonts, &mut line);
    let _removed = line.split_off(5);
    for text in ["new ", "tail "] {
        line.append(&cosmic::BufferLine::new(
            text,
            cosmic::LineEnding::None,
            attrs.clone(),
            cosmic::Shaping::Advanced,
        ));
    }
    assert_eq!(line.text(), "keep new tail ");
    assert_matches_fresh_line(&mut fonts, &mut line);
}

#[test]
fn retained_words_invalidate_on_attribute_reset_and_preserve_split_join_attributes() {
    let (mut fonts, preferred, _) = font_fixture();
    let mut attrs = cosmic::AttrsList::new(&cosmic::Attrs::new().metadata(11));
    attrs.add_span(
        7..9,
        &cosmic::Attrs::new()
            .weight(cosmic::Weight::BOLD)
            .metadata(17),
    );
    let mut line = cosmic::BufferLine::new(
        "before fi after ",
        cosmic::LineEnding::None,
        attrs.clone(),
        cosmic::Shaping::Advanced,
    );
    assert_matches_fresh_line(&mut fonts, &mut line);
    let other_attrs = cosmic::AttrsList::new(&cosmic::Attrs::new().metadata(29));
    line.append(&cosmic::BufferLine::new(
        "tail λ\t",
        cosmic::LineEnding::None,
        other_attrs.clone(),
        cosmic::Shaping::Advanced,
    ));
    let suffix = line.split_off(10);
    line.append(&cosmic::BufferLine::new(
        "new ",
        cosmic::LineEnding::None,
        other_attrs.clone(),
        cosmic::Shaping::Advanced,
    ));
    line.append(&suffix);
    assert_eq!(line.text(), "before fi new after tail λ\t");
    assert_matches_fresh_line(&mut fonts, &mut line);

    let suffix = line.split_off(line.text().len() - 1);
    line.set_attrs_list(other_attrs.clone());
    line.append(&suffix);
    assert_matches_fresh_line(&mut fonts, &mut line);

    // reset_new must not preserve a pending text-only reuse allowance, even
    // when the new words happen to have the same byte ranges as the old ones.
    line.append(&suffix);
    line.reset_new(
        "xxxxxx yy لَا ",
        cosmic::LineEnding::None,
        attrs.clone(),
        cosmic::Shaping::Advanced,
    );
    assert_matches_fresh_line(&mut fonts, &mut line);
    line.append(&cosmic::BufferLine::new(
        " end",
        cosmic::LineEnding::None,
        attrs,
        cosmic::Shaping::Advanced,
    ));
    // Buffer owners must invalidate shaping when fonts change (iced does this
    // on its font-system revision). That reset also revokes prefix reuse.
    fonts.db_mut().remove_face(preferred);
    line.reset_shaping();
    assert_matches_fresh_line(&mut fonts, &mut line);
}
