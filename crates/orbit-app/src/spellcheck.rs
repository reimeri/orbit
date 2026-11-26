//! Local composer spelling. Dictionary discovery, parsing and checking never
//! run on the iced thread, and only the current draft revision can paint.
use iced::{
    Task,
    advanced::graphics::text::cosmic_text::LineIter,
    task,
    widget::text_editor::{Content, Cursor, Edit, Position},
};
use pulldown_cmark::{Event, LinkType, Parser, Tag};
use std::{
    collections::BTreeMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Duration,
};
use tokio::sync::Mutex;
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub line: usize,
    pub bytes: Range<usize>,
}

impl Mark {
    pub fn replace(&self, editor: &mut Content, word: Arc<String>) -> BeforeEdit {
        let at = |column| Position {
            line: self.line,
            column,
        };
        editor.move_to(Cursor {
            position: at(self.bytes.end),
            selection: Some(at(self.bytes.start)),
        });
        let before = BeforeEdit::replacement(editor, word.len());
        editor.perform(iced::widget::text_editor::Action::Edit(Edit::Paste(word)));
        before
    }
}

/// A visible word in one particular draft revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub revision: u64,
    pub mark: Mark,
}

#[derive(Debug, Clone)]
pub struct Report {
    languages: Vec<String>,
    unavailable: Option<String>,
    marks: Vec<Mark>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Due(u64),
    Checked(u64, Result<Report, String>),
    Suggested(Target, Result<Vec<String>, String>),
}

pub struct State {
    dirs: Vec<PathBuf>,
    backend: Arc<Mutex<Backend>>,
    revision: Arc<AtomicU64>,
    task: Option<task::Handle>,
    pub languages: Option<Vec<String>>,
    pub unavailable: Option<String>,
    pub marks: Vec<Mark>,
    pub typing_at: Option<Position>,
}

fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

impl State {
    pub fn new() -> Self {
        let user = absolute_env("XDG_DATA_HOME")
            .or_else(|| absolute_env("HOME").map(|home| home.join(".local/share")))
            .map(|data| data.join("orbit/dictionaries"));
        Self::with_dirs(
            user.into_iter()
                .chain(absolute_env("ORBIT_DICTIONARY_DIR"))
                .collect(),
        )
    }

    fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            backend: Arc::default(),
            revision: Arc::default(),
            task: None,
            languages: None,
            unavailable: None,
            marks: Vec::new(),
            typing_at: None,
        }
    }

    fn cancel(&mut self) -> u64 {
        self.task = None;
        self.revision.fetch_add(1, Relaxed).wrapping_add(1)
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Relaxed)
    }

    pub fn still_marked(&self, target: &Target) -> bool {
        let mark = &target.mark;
        self.revision() == target.revision
            && self
                .marks
                .binary_search_by_key(&(mark.line, mark.bytes.start, mark.bytes.end), |m| {
                    (m.line, m.bytes.start, m.bytes.end)
                })
                .is_ok()
    }

    pub fn suggest(&self, target: Target, editor: &Content) -> Task<Message> {
        let backend = self.backend.clone();
        let current = self.revision.clone();
        let revision = target.revision;
        let word = editor.with_editor(|e| {
            e.buffer().lines[target.mark.line].text()[target.mark.bytes.clone()].to_owned()
        });
        Task::future(async move {
            let backend = backend.lock_owned().await;
            if current.load(Relaxed) != revision {
                return None;
            }
            let choices = tokio::task::spawn_blocking(move || {
                // Only a successful check with the current dictionary can publish marks.
                let (_, dictionary) = backend
                    .loaded
                    .as_ref()
                    .expect("marked word has a dictionary");
                let dictionary = dictionary.as_ref().map_err(Clone::clone)?;
                let mut choices = Vec::new();
                dictionary.suggest(&word, &mut choices);
                if choices.is_empty() && word.contains('’') {
                    dictionary.suggest(&word.replace('’', "'"), &mut choices);
                }
                Ok(choices)
            })
            .await
            .unwrap_or_else(|e| {
                Err(format!(
                    "Could not get spelling suggestions: {e}. Right-click the word to retry."
                ))
            });
            Some(Message::Suggested(target, choices))
        })
        .and_then(Task::done)
    }

    pub fn clear(&mut self) {
        self.cancel();
        self.typing_at = None;
        self.marks.clear();
    }

    pub fn schedule(&mut self, debounce: bool) -> Task<Message> {
        let revision = self.cancel();
        if debounce {
            let (task, handle) = Task::perform(
                async { tokio::time::sleep(Duration::from_millis(180)).await },
                move |()| Message::Due(revision),
            )
            .abortable();
            self.task = Some(handle.abort_on_drop());
            task
        } else {
            Task::done(Message::Due(revision))
        }
    }

    pub fn check(
        &mut self,
        revision: u64,
        editor: &Content,
        language: Option<String>,
    ) -> Task<Message> {
        if self.revision.load(Relaxed) != revision {
            return Task::none();
        }
        let text = if language.is_some() {
            snapshot(editor)
        } else {
            String::new()
        };
        let backend = self.backend.clone();
        let dirs = self.dirs.clone();
        let current = self.revision.clone();
        let (task, handle) = Task::future(async move {
            // Hold the lock through blocking work, even if this UI task is
            // superseded. Only the most recent queued request can start next.
            let mut backend = backend.lock_owned().await;
            let result = tokio::task::spawn_blocking(move || {
                let latest = || current.load(Relaxed) == revision;
                latest().then(|| backend.check(&dirs, language, &text, latest))
            })
            .await;
            match result {
                Ok(report) => report.map(|report| Message::Checked(revision, Ok(report))),
                Err(error) => Some(Message::Checked(
                    revision,
                    Err(format!(
                        "Spell checker stopped: {error}. Restart Orbit to retry."
                    )),
                )),
            }
        })
        .abortable();
        self.task = Some(handle.abort_on_drop());
        task.and_then(Task::done)
    }

    pub fn complete(&mut self, revision: u64, result: Result<Report, String>) {
        if self.revision.load(Relaxed) == revision {
            self.task = None;
            match result {
                Ok(report) => {
                    self.languages = Some(report.languages);
                    self.unavailable = report.unavailable;
                    self.marks = report.marks;
                }
                Err(error) => {
                    self.unavailable = Some(error);
                    self.marks.clear();
                }
            }
        }
    }
}

// Only for the checker: serializing the original line endings can collapse
// two editor rows (e.g. an inserted LF adjacent to an existing CR).
fn snapshot(editor: &Content) -> String {
    editor.with_editor(|editor| {
        let mut text = String::new();
        for (i, line) in editor.buffer().lines.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            text.push_str(line.text());
        }
        text
    })
}

/// Ends of the selection (or caret) and size of the document before a local edit.
pub struct BeforeEdit {
    cursor: Cursor,
    lines: usize,
    replacement: Option<(usize, usize)>,
}

impl BeforeEdit {
    pub fn capture(editor: &Content) -> Self {
        Self {
            cursor: editor.cursor(),
            lines: editor.line_count(),
            replacement: None,
        }
    }

    pub fn for_edit(editor: &Content, edit: &Edit) -> Self {
        match edit {
            Edit::Insert(c) if !c.is_control() || *c == '\t' => {
                Self::replacement(editor, c.len_utf8())
            }
            Edit::Paste(text) => Self::replacement(editor, text.len()),
            Edit::Backspace | Edit::Delete => Self::replacement(editor, 0),
            _ => Self::capture(editor),
        }
    }

    /// For a contiguous replacement the new caret is at the end of the
    /// inserted bytes, and everything after it is unchanged.
    pub fn replacement(editor: &Content, inserted: usize) -> Self {
        let mut before = Self::capture(editor);
        let len =
            editor.with_editor(|e| e.buffer().lines[before.cursor.position.line].text().len());
        before.replacement = Some((len, inserted));
        before
    }

    pub fn retain_untouched(self, marks: &mut Vec<Mark>, editor: &Content) {
        let caret = self.cursor.position.line;
        let anchor = self.cursor.selection.unwrap_or(self.cursor.position).line;
        let position = editor.cursor().position;
        let after = position.line;
        let new_lines = editor.line_count();
        // Account for both selection replacement and Delete/Backspace joining
        // adjacent lines. Only the unchanged suffix needs its indices shifted.
        let prefix = caret.min(anchor).min(after);
        let suffix = (self.lines - 1 - caret.max(anchor)).min(new_lines - 1 - after);
        let first = marks.partition_point(|m| m.line < prefix);
        let end = marks.partition_point(|m| m.line < self.lines - suffix);
        if first == end && new_lines == self.lines {
            return;
        }
        if let Some((old_len, inserted)) = self.replacement
            && new_lines == self.lines
            && caret == anchor
            && caret == after
        {
            editor.with_editor(|e| {
                let line = e.buffer().lines[after].text();
                let start = position.column - inserted;
                let old_end = old_len - (line.len() - position.column);
                if inserted == 0 && start == old_end {
                    return;
                }

                // Expand to whole words: inserting at a word's edge may
                // extend it, and deleting a separator can join two words.
                let left = crossing_word(line, start).start;
                let right = old_end + (crossing_word(line, position.column).end - position.column);
                let row = &mut marks[first..end];
                let keep = row.partition_point(|m| m.bytes.end <= left);
                let shift = row.partition_point(|m| m.bytes.start < right);
                for m in &mut row[shift..] {
                    m.bytes = (m.bytes.start - old_end + position.column)
                        ..(m.bytes.end - old_end + position.column);
                }
                marks.drain(first + keep..first + shift);
            });
            return;
        }
        marks.drain(first..end);
        if new_lines != self.lines {
            for mark in &mut marks[first..] {
                mark.line = mark.line + new_lines - self.lines;
            }
        }
    }
}

/// Unicode word span straddling a byte offset, or an empty range at a boundary.
/// Only the adjacent non-whitespace token needs segmenting, not the entire line.
fn crossing_word(line: &str, at: usize) -> Range<usize> {
    let (left, right) = line.split_at(at);
    let token_char = |c: char| !c.is_whitespace();
    if left.ends_with(token_char) && right.starts_with(token_char) {
        let start = left.trim_end_matches(token_char).len();
        let end = line.len() - right.trim_start_matches(token_char).len();
        for (i, word) in line[start..end].split_word_bound_indices() {
            let span = start + i..start + i + word.len();
            if span.end > at {
                if span.start < at {
                    return span;
                }
                break;
            }
        }
    }
    at..at
}

#[derive(Default)]
struct Backend {
    paths: Option<BTreeMap<String, PathBuf>>,
    loaded: Option<(String, Result<spellbook::Dictionary, String>)>,
}

impl Backend {
    fn check(
        &mut self,
        dirs: &[PathBuf],
        language: Option<String>,
        text: &str,
        latest: impl Fn() -> bool,
    ) -> Report {
        let paths = self.paths.get_or_insert_with(|| discover(dirs));
        let mut report = Report {
            languages: paths.keys().cloned().collect(),
            unavailable: None,
            marks: Vec::new(),
        };
        if let Some(language) = language {
            if self.loaded.as_ref().map(|(tag, _)| tag) != Some(&language) {
                self.loaded = Some((language.clone(), load(paths, &language)));
            }
            match &self.loaded.as_ref().expect("selected dictionary loaded").1 {
                Ok(dictionary) if latest() => {
                    report.marks = check_words(text, latest, |word| {
                        dictionary.check(word)
                            || (word.contains('’') && dictionary.check(&word.replace('’', "'")))
                    });
                }
                Ok(_) => {}
                Err(error) => report.unavailable = Some(error.clone()),
            }
        }
        report
    }
}

fn discover(dirs: &[PathBuf]) -> BTreeMap<String, PathBuf> {
    let mut pairs = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "aff")
                && path.is_file()
                && path.with_extension("dic").is_file()
                && let Some(language) = path.file_stem().and_then(|stem| stem.to_str())
            {
                pairs.entry(language.to_owned()).or_insert(path);
            }
        }
    }
    pairs
}

fn load(
    pairs: &BTreeMap<String, PathBuf>,
    language: &str,
) -> Result<spellbook::Dictionary, String> {
    let aff = pairs.get(language).ok_or_else(|| {
        format!("No dictionary for {language}. Select an installed language or install its dictionary and restart Orbit.")
    })?;
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|error| {
            format!(
                "Cannot read {}: {error}. Install a UTF-8 dictionary pair and restart Orbit.",
                path.display()
            )
        })
    };
    let rules = read(aff)?;
    let words = read(&aff.with_extension("dic"))?;
    spellbook::Dictionary::new(&rules, &words).map_err(|error| {
        format!(
            "Cannot load {language}: {error}. Replace the dictionary and restart Orbit, or choose another language."
        )
    })
}

fn check_words(text: &str, latest: impl Fn() -> bool, correct: impl Fn(&str) -> bool) -> Vec<Mark> {
    // Keep source byte offsets and line breaks, but blank markup and other
    // non-prose. In particular a link label is Text; its destination isn't.
    // Mask first so a quoted @reference can still span emphasis or escapes.
    let mut prose = text.as_bytes().to_vec();
    let blank = |bytes: &mut [u8]| {
        for byte in bytes {
            if !matches!(byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    };
    let mut copied = 0;
    let mut skip_until = 0;
    for (event, range) in Parser::new(text).into_offset_iter() {
        if !latest() {
            return Vec::new();
        }
        match event {
            Event::Start(
                Tag::CodeBlock(_)
                | Tag::Link {
                    link_type: LinkType::Autolink | LinkType::Email,
                    ..
                },
            ) => skip_until = range.end,
            Event::Text(value) if range.start >= skip_until && *value == text[range.clone()] => {
                blank(&mut prose[copied..range.start]);
                copied = range.end;
            }
            _ => {}
        }
    }
    blank(&mut prose[copied..]);
    let prose = std::str::from_utf8(&prose).expect("only full Markdown source ranges were masked");
    let mut marks = Vec::new();
    for (line, (bytes, _)) in LineIter::new(prose).enumerate() {
        let content = &prose[bytes.clone()];
        let mut start = 0;
        for tag in
            orbit_core::file_tags::mention_ranges(&text[bytes], |at| content.as_bytes()[at] == b'@')
                .chain(std::iter::once(content.len()..content.len()))
        {
            let mut offset = start;
            for piece in content[start..tag.start].split_inclusive(char::is_whitespace) {
                if !latest() {
                    return Vec::new();
                }
                let token = piece.trim_end();
                for (at, word) in prose_words(token) {
                    if word.len() <= spellbook::MAX_WORD_LEN
                        && word.chars().any(char::is_alphabetic)
                        && !code_like(word)
                        && !correct(word)
                    {
                        let at = offset + at;
                        marks.push(Mark {
                            line,
                            bytes: at..at + word.len(),
                        });
                    }
                }
                offset += piece.len();
            }
            start = tag.end;
        }
    }
    marks
}

fn code_like(word: &str) -> bool {
    let mut letters = word.chars().filter(|c| c.is_alphabetic());
    let first = letters.next().expect("contains a letter");
    let mut seen_lower = first.is_lowercase();
    let mut all_upper = first.is_uppercase();
    let mut several = false;
    for c in letters {
        several = true;
        if seen_lower && c.is_uppercase() {
            return true; // camelCase, PascalCase
        }
        seen_lower |= c.is_lowercase();
        all_upper &= c.is_uppercase();
    }
    (several && all_upper) || word.chars().any(|c| c.is_numeric() || c == '_')
}

/// URLs can contain punctuation, but ordinary adjacent prose (e.g.
/// "typo,src/file.rs") should not inherit a neighbour's path classification.
fn prose_words(token: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    token
        .split_inclusive([',', ';', '!', '(', ')', '[', ']', '{', '}', '"', '…'])
        .scan(false, move |url, piece| {
            let start = offset;
            offset += piece.len();
            *url |= piece.contains("://");
            let technical = *url
                || piece.contains(['/', '\\', '@'])
                || piece.contains("::")
                || piece.starts_with("--");
            Some(
                piece
                    .split_word_bound_indices()
                    .filter_map(move |(at, word)| {
                        // Unicode word boundaries keep internal dots in "file.ext", but
                        // split prose around a run of dots (an ASCII ellipsis).
                        (!technical && !word.contains('.')).then_some((start + at, word))
                    }),
            )
        })
        .flatten()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn pair(dir: &Path, language: &str, words: &str) -> State {
        fs::write(dir.join(format!("{language}.aff")), "SET UTF-8\n").unwrap();
        fs::write(dir.join(format!("{language}.dic")), words).unwrap();
        State::with_dirs(vec![dir.to_owned()])
    }

    #[test]
    fn checks_unicode_prose_without_marking_code_links_paths_or_file_tags() {
        let text = "A misspëll café don't rock’n’roll. camelCase XML2 snake_case src/main.rs `fuzzz`\r\n\
                    ~~~rust\nmisst in fence\n~~~\r\
                    See https://example.test no@host.test @\"quoted file.rs\" @plain `x` finall.\n\
                    More misst @\"unfinished file";
        let marks = check_words(text, || true, |word| matches!(word, "A" | "See" | "More"));
        let lines: Vec<_> = LineIter::new(text).map(|(line, _)| &text[line]).collect();
        let marked: Vec<_> = marks
            .iter()
            .map(|mark| (mark.line, &lines[mark.line][mark.bytes.clone()]))
            .collect();
        assert_eq!(
            marked,
            [
                (0, "misspëll"),
                (0, "café"),
                (0, "don't"),
                (0, "rock’n’roll"),
                (4, "finall"),
                (5, "misst")
            ]
        );
    }

    #[test]
    fn checks_emphasis_link_labels_and_prose_next_to_technical_spans() {
        let text = r#"_mispëlt_ [wrng](guidde "titlle") oops...typo,src/file.rs @"path \"with\" _some_ words" finál"#;
        let marked: Vec<_> = check_words(text, || true, |_| false)
            .into_iter()
            .map(|m| &text[m.bytes])
            .collect();
        assert_eq!(marked, ["mispëlt", "wrng", "oops", "typo", "finál"]);
    }

    #[test]
    fn same_line_edits_keep_unaffected_underlines_before_recheck() {
        use iced::widget::text_editor::Action;

        for (text, column, anchor, edit, expected) in [
            // Insert several bytes inside one word. Its punctuation-separated
            // neighbour and the word after it must still have exact byte spans.
            (
                "wrng,mispelt othr",
                7,
                None,
                Edit::Insert('界'),
                &["wrng", "othr"][..],
            ),
            // Deleting only the separator changes both neighbouring words.
            ("wrng café othr", 4, None, Edit::Delete, &["othr"]),
            // Splitting a word must not leave either half underlined.
            (
                "wrng café othr",
                7,
                None,
                Edit::Insert(' '),
                &["wrng", "othr"],
            ),
            // A separator after a marked word does not change it.
            ("wrng othr", 4, None, Edit::Insert(' '), &["wrng", "othr"]),
            // Replace a backward selection, leaving a shorter UTF-8 line.
            (
                "wrng café othr",
                5,
                Some(10),
                Edit::Paste(Arc::new("ok".into())),
                &["wrng", "othr"],
            ),
        ] {
            let mut editor = Content::with_text(text);
            let mut marks = check_words(text, || true, |_| false);
            editor.move_to(Cursor {
                position: Position { line: 0, column },
                selection: anchor.map(|column| Position { line: 0, column }),
            });
            let before = BeforeEdit::for_edit(&editor, &edit);
            editor.perform(Action::Edit(edit));
            before.retain_untouched(&mut marks, &editor);
            let line = snapshot(&editor);
            let retained: Vec<_> = marks.iter().map(|m| &line[m.bytes.clone()]).collect();
            assert_eq!(retained, expected, "{text} -> {line}");
        }
    }

    #[test]
    fn local_edits_preserve_unaffected_marks_and_native_line_numbers() {
        use iced::widget::text_editor::{Action, Edit};
        let mut editor = Content::with_text("first\rsecond\nthird");
        let mut marks = check_words(&snapshot(&editor), || true, |_| false);
        let move_to = |editor: &mut Content, line, column| {
            editor.move_to(Cursor {
                position: Position { line, column },
                selection: None,
            });
        };
        move_to(&mut editor, 0, 5);
        let before = BeforeEdit::for_edit(&editor, &Edit::Enter);
        editor.perform(Action::Edit(Edit::Enter));
        before.retain_untouched(&mut marks, &editor);
        assert_eq!(snapshot(&editor), "first\n\nsecond\nthird");
        let rechecked = check_words(&snapshot(&editor), || true, |_| false);
        assert_eq!(marks, rechecked[1..]);
        assert_eq!((marks[0].line, marks[1].line), (2, 3));

        // Replacing a backwards selection changes the number of rows: the
        // first and last untouched rows must stay marked during debounce.
        marks = rechecked;
        editor.move_to(Cursor {
            position: Position { line: 1, column: 0 },
            selection: Some(Position { line: 2, column: 3 }),
        });
        let before = BeforeEdit::for_edit(&editor, &Edit::Insert('x'));
        editor.perform(Action::Edit(Edit::Insert('x')));
        before.retain_untouched(&mut marks, &editor);
        assert_eq!(snapshot(&editor), "first\nxond\nthird");
        assert_eq!(
            marks,
            [
                Mark {
                    line: 0,
                    bytes: 0..5
                },
                Mark {
                    line: 2,
                    bytes: 0..5
                }
            ]
        );
    }

    #[test]
    fn missing_dictionary_does_not_fallback_and_restart_sees_a_new_pair() {
        let dir = tempfile::tempdir().unwrap();
        pair(dir.path(), "en_US", "1\nhello\n");
        fs::write(dir.path().join("fr_FR.aff"), "SET UTF-8\n").unwrap();
        let dirs = [dir.path().to_owned()];
        let mut backend = Backend::default();
        let missing = backend.check(&dirs, Some("fr_FR".into()), "bonjour", || true);
        assert_eq!(missing.languages, ["en_US"]);
        assert!(missing.marks.is_empty());
        assert!(missing.unavailable.unwrap().contains("fr_FR"));

        pair(dir.path(), "fr_FR", "1\nbonjour\n");
        let cached = backend.check(&dirs, Some("fr_FR".into()), "", || true);
        assert_eq!(cached.languages, ["en_US"]);
        let updated =
            Backend::default().check(&dirs, Some("fr_FR".into()), "bonjour hello", || true);
        assert_eq!(updated.languages, ["en_US", "fr_FR"]);
        assert!(updated.unavailable.is_none());
        assert_eq!(
            updated.marks,
            [Mark {
                line: 0,
                bytes: 8..13
            }]
        );
    }

    #[test]
    fn loads_the_real_nix_dictionary() {
        let Some(dir) = absolute_env("ORBIT_TEST_DICTIONARY_DIR") else {
            return;
        };
        let result = Backend::default().check(
            &[dir],
            Some("en_US".into()),
            "Working don’t use mispelledd.",
            || true,
        );
        assert_eq!(result.unavailable, None);
        assert_eq!(
            result.marks,
            [Mark {
                line: 0,
                bytes: 20..30
            }]
        );
    }

    #[tokio::test]
    async fn in_flight_result_cannot_reappear_after_draft_invalidation() {
        use iced::futures::StreamExt as _;

        let dir = tempfile::tempdir().unwrap();
        pair(dir.path(), "en_US", "1\nhello\n");
        let mut state = State::with_dirs(vec![dir.path().to_owned()]);
        let editor = Content::with_text("hello mispelledd");
        let revision = state.cancel();
        let task = state.check(revision, &editor, Some("en_US".into()));
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(Message::Checked(revision, Ok(report)))) =
            stream.next().await
        else {
            panic!("expected a checked draft");
        };
        assert_eq!(
            report.marks,
            [Mark {
                line: 0,
                bytes: 6..16
            }]
        );
        state.clear();
        state.complete(revision, Ok(report));
        assert!(state.marks.is_empty());
        assert!(state.languages.is_none());
    }
}
