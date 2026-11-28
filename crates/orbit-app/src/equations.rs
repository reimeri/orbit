//! Display TeX only. All persistent/chat/clipboard data stays Markdown source.
use iced::{
    Element, Pixels, Task,
    widget::{container, svg, tooltip},
};
use lru::LruCache;
use std::{collections::VecDeque, num::NonZeroUsize, time::Instant};

const MAX_SOURCE: usize = 8192;
const CACHE_ENTRIES: usize = 64;

#[derive(Clone, Debug)]
pub struct Formula {
    handle: svg::Handle,
    // Dimensions in em, independent of chat font size / DPI.
    width: f32,
    height: f32,
}

fn typeset(source: &str) -> Option<Formula> {
    use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
    use ratex_types::math_style::MathStyle;
    // The Markdown block parser supplies a complete two-byte delimiter pair.
    let body = source[2..source.len() - 2].trim();
    if body.is_empty() || source.len() > MAX_SOURCE {
        return None;
    }
    let ast = ratex_parser::parser::parse(body).ok()?;
    let boxes = layout(
        &ast,
        &LayoutOptions::default().with_style(MathStyle::Display),
    );
    validate_layout(&boxes).ok()?;
    let list = to_display_list(&boxes);
    let width = list.width + 0.2;
    let height = list.height + list.depth + 0.2;
    // Bound both SVG conversion and the texture the UI will rasterize.
    if !(0.0..=128.).contains(&width) || !(0.0..=32.).contains(&height) || list.items.len() > 2048 {
        return None;
    }
    let svg = ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &ratex_svg::SvgOptions {
            embed_glyphs: true,
            font_size: 40.,
            padding: 4.,
            ..Default::default()
        },
        ratex_svg::SvgColorSyntax::Rgb,
    );
    if svg.len() > 512 * 1024 {
        return None;
    }
    Some(Formula {
        handle: svg::Handle::from_memory(svg.into_bytes()),
        width: width as f32,
        height: height as f32,
    })
}

#[derive(Clone, Debug)]
pub struct Rendered(String, Option<Formula>);

/// A viewport sensor requests only formulas that are about to scroll into view,
/// including formulas deep within a single long entry. Cache failures as well.
pub struct Cache {
    completed: std::cell::RefCell<LruCache<String, Option<Formula>>>,
    queue: VecDeque<String>,
    active: Option<String>,
    copied: Option<(String, usize, Instant)>,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            completed: LruCache::new(NonZeroUsize::new(CACHE_ENTRIES).unwrap()).into(),
            queue: VecDeque::new(),
            active: None,
            copied: None,
        }
    }
}

impl Cache {
    pub fn request(&mut self, source: String) -> Task<Rendered> {
        if self.completed.get_mut().get(&source).is_some()
            || self.active.as_ref() == Some(&source)
            || self.queue.len() >= CACHE_ENTRIES
            || self.queue.contains(&source)
        {
            return Task::none();
        }
        self.queue.push_back(source);
        self.next()
    }

    fn next(&mut self) -> Task<Rendered> {
        if self.active.is_some() {
            return Task::none();
        }
        let Some(source) = self.queue.pop_front() else {
            return Task::none();
        };
        self.active = Some(source.clone());
        Task::perform(
            async move {
                let input = source.clone();
                let result = tokio::task::spawn_blocking(move || typeset(&input)).await;
                Rendered(source, result.ok().flatten())
            },
            std::convert::identity,
        )
    }

    pub fn update(&mut self, Rendered(src, result): Rendered) -> Task<Rendered> {
        self.active = None;
        self.completed.get_mut().put(src, result);
        self.next()
    }

    #[cfg(any(test, feature = "staged-capture"))]
    pub fn rendered(&self, source: &str) -> bool {
        self.completed
            .borrow()
            .peek(source)
            .is_some_and(Option::is_some)
    }
    pub fn leave_session(&mut self) {
        self.queue.clear();
        self.copied = None;
    }

    pub fn mark_copied(&mut self, entry_id: String, block: usize, until: Instant) {
        self.copied = Some((entry_id, block, until));
    }

    pub fn clear_copied(&mut self, until: Instant) {
        // A previous click's timeout must not shorten the current confirmation.
        if self
            .copied
            .as_ref()
            .is_some_and(|(_, _, expiry)| *expiry == until)
        {
            self.copied = None;
        }
    }

    fn copy_label(&self, at: (&str, usize)) -> &'static str {
        if self
            .copied
            .as_ref()
            .is_some_and(|(id, block, _)| (id.as_str(), *block) == at)
        {
            "Copied"
        } else {
            "Copy LaTeX"
        }
    }

    pub fn view<'a>(
        &self,
        at: (&'a str, usize),
        source: &'a str,
        size: Pixels,
        literal: Element<'a, crate::ui::Message>,
    ) -> Element<'a, crate::ui::Message> {
        match self.completed.borrow_mut().get(source) {
            Some(Some(formula)) => formula.view(source, size, at, self.copy_label(at)),
            Some(None) => literal,
            None if source.len() <= MAX_SOURCE && self.queue.len() < CACHE_ENTRIES => {
                iced::widget::sensor(literal)
                    .key_ref(source)
                    .anticipate(120.)
                    .on_show(move |_| crate::ui::Message::RequestEquation(source.to_owned()))
                    .into()
            }
            None => literal,
        }
    }
}

impl Formula {
    fn view<'a>(
        &self,
        source: &'a str,
        size: Pixels,
        (entry_id, block): (&'a str, usize),
        label: &'static str,
    ) -> Element<'a, crate::ui::Message> {
        // Rasterization happens before clipping. Limit the logical surface area
        // as well as the em bounds (256k logical pixels = 4 MiB RGBA at 2x).
        let em = (size.0 * 1.42).min((256_000. / (self.width * self.height)).sqrt());
        let glyph = svg(self.handle.clone())
            .width(self.width * em)
            .height(self.height * em)
            .style(|theme, _| svg::Style {
                color: Some(crate::theme::colors(theme).text),
            });
        let copy = crate::theme::button(glyph)
            .padding(4)
            .style(iced::widget::button::text)
            .on_press_with(move || crate::ui::Message::CopyTranscript {
                entry_id: entry_id.to_owned(),
                equation_block: Some(block),
                source: source.to_owned(),
            });
        container(crate::scrollbar::horizontal(
            tooltip(copy, label, tooltip::Position::FollowCursor)
                .style(crate::theme::footer_surface),
        ))
        .width(iced::Fill)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_selection;
    use iced::widget::markdown::{Content, Item};

    const EXAMPLE: &str = include_str!("fixtures/search-chance.md");

    fn formulas(content: &Content) -> Vec<&str> {
        fn visit<'a>(items: &'a [Item], out: &mut Vec<&'a str>) {
            for item in items {
                match item {
                    Item::DisplayMath(src) => out.push(src),
                    Item::Quote(items) => visit(items, out),
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        visit(content.items(), &mut out);
        out
    }

    #[test]
    fn streamed_display_blocks_preserve_raw_tex_and_do_not_consume_prose() {
        let example = EXAMPLE.trim_end();
        let mut content = Content::parse("Before\n");
        for c in example[..example.len() - 1].chars() {
            content.push_str(c.encode_utf8(&mut [0; 4]));
        }
        assert!(formulas(&content).is_empty());
        content.push_str("]\nAfter\n$$x^2$$\n");
        assert_eq!(formulas(&content), [example, "$$x^2$$"]);
        let viewer = chat_selection::Viewer::new(Default::default(), 0);
        let _ = iced::widget::markdown::view_with(content.items(), iced::Theme::Dark, &viewer);
        let blocks = viewer.blocks.into_inner();
        assert_eq!(
            blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>(),
            ["Before", example, "After", "$$x^2$$"]
        );
        assert!(typeset(example).is_some());
    }

    #[test]
    fn delimiters_respect_markdown_blocks_and_leave_inline_math_alone() {
        let content = Content::parse(
            r#"Inline $x$ or $$x$$ or `\[x\]`, and $10.
```tex
\[
x^2
\]
```

    $$indented code$$

> $$
> \begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}
> $$

\[unfinished: \text{10\%}
"#,
        );
        let formulas = formulas(&content);
        assert_eq!(
            formulas,
            [r"$$
\begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}
$$"]
        );
        assert!(matches!(content.items().last(), Some(Item::Paragraph(_))));
        let viewer = chat_selection::Viewer::new(Default::default(), 0);
        let _ = iced::widget::markdown::view_with(content.items(), iced::Theme::Dark, &viewer);
        assert_eq!(
            viewer.blocks.borrow().last().unwrap().text,
            r"\[unfinished: \text{10\%}"
        );
    }

    #[test]
    fn unfinished_equation_does_not_swallow_the_next_paragraph() {
        let mut content = Content::parse("$$x\n");
        content.push_str("\n# Later\n\n$$y$$");
        assert!(matches!(
            content.items(),
            [Item::Paragraph(_), Item::Heading(..), Item::DisplayMath(_)]
        ));
    }

    #[test]
    fn copy_feedback_is_local_and_a_second_click_renews_it() {
        let mut cache = Cache::default();
        let first = Instant::now();
        let next = first + std::time::Duration::from_millis(500);
        cache.mark_copied("entry".into(), 2, first);
        cache.mark_copied("entry".into(), 2, next);
        cache.clear_copied(first);
        assert_eq!(cache.copy_label(("entry", 2)), "Copied");
        assert_eq!(cache.copy_label(("entry", 1)), "Copy LaTeX");
        assert_eq!(cache.copy_label(("other", 2)), "Copy LaTeX");
        cache.clear_copied(next);
        assert_eq!(cache.copy_label(("entry", 2)), "Copy LaTeX");
    }

    #[test]
    fn typeset_svg_paints_visible_glyphs() {
        use iced::{Color, Size, advanced::renderer, mouse};
        use iced_runtime::{UserInterface, user_interface};
        let source = EXAMPLE.trim_end();
        let formula = typeset(source).unwrap();
        let size = Size::new(800., 120.);
        let mut renderer = crate::test_renderer::new(16.);
        let mut ui = UserInterface::build(
            formula.view(source, Pixels(16.), ("entry", 0), "Copy LaTeX"),
            size,
            user_interface::Cache::new(),
            &mut renderer,
        );
        ui.draw(
            &mut renderer,
            &iced::Theme::Dark,
            &renderer::Style {
                text_color: Color::WHITE,
            },
            mouse::Cursor::Unavailable,
        );
        let pixels =
            crate::test_renderer::screenshot(&mut renderer, Size::new(800, 120), 1., Color::BLACK);
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 100 && p[1] > 100 && p[2] > 100)
                .count()
                > 200,
            "outlined formula must actually paint on the chat background"
        );
    }

    #[tokio::test]
    async fn worker_deduplicates_requests_and_handles_unsupported_input() {
        use iced::futures::StreamExt;
        async fn finish(task: Task<Rendered>) -> Rendered {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(result) = action {
                    return result;
                }
            }
            panic!("missing equation result");
        }
        let mut cache = Cache::default();
        let first = EXAMPLE.trim_end().to_owned();
        let work = cache.request(first.clone());
        let bad = r"$$\orbitUnknownMacro{x}$$";
        assert!(iced_runtime::task::into_stream(cache.request(bad.into())).is_none());
        assert!(iced_runtime::task::into_stream(cache.request(first.clone())).is_none());
        let work = cache.update(finish(work).await);
        let _ = cache.update(finish(work).await);
        assert!(cache.rendered(&first));
        assert!(!cache.rendered(bad));
        assert!(iced_runtime::task::into_stream(cache.request(bad.into())).is_none());
    }
}
