//! V1-style composer replacement. Drafts live in the app, never in session storage.
use crate::icons::Icon;
use crate::scrollbar::scroll;
use crate::theme::{self, DANGER, MUTED};
use iced::{
    Element, Event, Fill, Length, Rectangle, Size, Task, Theme, Vector,
    advanced::{
        Clipboard, Layout, Shell, Widget, layout, overlay, renderer,
        widget::{Operation, Tree, tree},
    },
    keyboard::{self, key::Named},
    mouse,
    widget::{self, column, container, row, scrollable, space, text, text_editor},
};
use pico_core::questionnaire::{Pending, QuestionKind};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
pub enum Action {
    Toggle,
    Select(usize),
    Move(bool),
    Edit(usize),
    EditSelected,
    Replace(bool),
    SwitchConfirmation,
    Confirm,
    Next,
    Back,
    Editor(text_editor::Action),
    Scroll(crate::growing_editor::ScrollTo),
    DeleteWord,
}
#[derive(Clone, Debug)]
pub struct Message {
    pub question: usize,
    pub action: Action,
}
struct Answer {
    selected: Option<usize>,
    editor: text_editor::Content,
    replace: Option<usize>,
    replace_focused: bool,
}
pub struct Draft {
    pub pending: Pending,
    answers: Vec<Answer>,
    pub current: usize,
    pub collapsed: bool,
    pub error: Option<String>,
}
pub struct Update {
    pub answers: Option<Vec<String>>,
    pub task: Task<Message>,
}
impl Draft {
    pub fn new(pending: Pending) -> Self {
        let answers = pending
            .request
            .questions()
            .iter()
            .map(|_| Answer {
                selected: None,
                editor: text_editor::Content::new(),
                replace: None,
                replace_focused: false,
            })
            .collect();
        Self {
            pending,
            answers,
            current: 0,
            collapsed: false,
            error: None,
        }
    }
    fn options(&self) -> &[String] {
        match &self.pending.request.questions()[self.current].kind {
            QuestionKind::Select(options) => options,
            QuestionKind::Text => &[],
        }
    }
    fn text_open(&self) -> bool {
        self.options().is_empty()
            || self.answers[self.current].selected == Some(self.options().len())
    }
    fn answered(&self, index: usize) -> bool {
        let answer = &self.answers[index];
        if let QuestionKind::Select(options) = &self.pending.request.questions()[index].kind {
            match answer.selected {
                None => return false,
                Some(selected) if selected < options.len() => return true,
                _ => {}
            }
        }
        answer
            .editor
            .lines()
            .any(|line| !line.text.trim().is_empty())
    }
    fn value(&self, index: usize) -> Option<String> {
        let a = &self.answers[index];
        if let QuestionKind::Select(options) = &self.pending.request.questions()[index].kind {
            let selected = a.selected?;
            if selected < options.len() {
                return Some(options[selected].clone());
            }
        }
        let text = a.editor.text();
        (!text.trim().is_empty()).then_some(text)
    }
    fn editor_id(&self) -> String {
        format!("questionnaire-editor-{}-{}", self.pending.id, self.current)
    }
    fn scroll_id(&self) -> String {
        format!("questionnaire-body-{}", self.pending.id)
    }
    pub fn focus(&self) -> Task<Message> {
        if self.text_open() && !self.collapsed && self.answers[self.current].replace.is_none() {
            widget::operation::focus(self.editor_id())
        } else {
            Task::none()
        }
    }
    pub fn update(&mut self, message: Message) -> Update {
        let mut result = Update {
            answers: None,
            task: Task::none(),
        };
        if message.question != self.current {
            return result;
        }
        if self.collapsed && !matches!(message.action, Action::Toggle) {
            return result;
        }
        if self.answers[self.current].replace.is_some()
            && !matches!(
                message.action,
                Action::Toggle | Action::Replace(_) | Action::SwitchConfirmation | Action::Confirm
            )
        {
            return result;
        }
        self.error = None;
        let count = self.options().len();
        let old_current = self.current;
        let mut focus = false;
        let mut reveal = false;
        match message.action {
            Action::Toggle => {
                self.collapsed = !self.collapsed;
                focus = true;
            }
            Action::Select(index) if index <= count => {
                self.answers[self.current].selected = Some(index);
                focus = true;
                reveal = true;
            }
            Action::Move(down) if count > 0 => {
                let a = &mut self.answers[self.current];
                a.selected = Some(match a.selected {
                    None => {
                        if down {
                            0
                        } else {
                            count
                        }
                    }
                    Some(index) => {
                        if down {
                            (index + 1) % (count + 1)
                        } else {
                            (index + count) % (count + 1)
                        }
                    }
                });
                focus = true;
                reveal = true;
            }
            Action::EditSelected => {
                if let Some(index) = self.answers[self.current].selected.filter(|i| *i < count) {
                    return self.update(Message {
                        question: self.current,
                        action: Action::Edit(index),
                    });
                }
            }
            Action::Edit(index) if index < count => {
                let option = self.options()[index].clone();
                let a = &mut self.answers[self.current];
                let draft = a.editor.text();
                if !draft.is_empty() && draft != option {
                    a.replace = Some(index);
                    a.replace_focused = false;
                } else {
                    a.editor = text_editor::Content::with_text(&option);
                    a.editor
                        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                    a.selected = Some(count);
                    focus = true;
                    reveal = true;
                }
            }
            Action::SwitchConfirmation => {
                let a = &mut self.answers[self.current];
                a.replace_focused = !a.replace_focused;
            }
            Action::Confirm => {
                return self.update(Message {
                    question: self.current,
                    action: Action::Replace(self.answers[self.current].replace_focused),
                });
            }
            Action::Replace(replace) => {
                if let Some(index) = self.answers[self.current].replace.take() {
                    if replace {
                        let option = self.options()[index].clone();
                        let a = &mut self.answers[self.current];
                        a.editor = text_editor::Content::with_text(&option);
                        a.editor
                            .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
                    }
                    self.answers[self.current].selected = Some(count);
                    focus = true;
                    reveal = true;
                }
            }
            Action::Next => {
                if !self.answered(self.current) {
                    self.error = Some("An answer is required before continuing.".into());
                } else if self.current + 1 < self.answers.len() {
                    self.current += 1;
                    focus = true;
                } else {
                    result.answers = self
                        .answers
                        .iter()
                        .enumerate()
                        .map(|(i, _)| self.value(i))
                        .collect();
                    if result.answers.is_none() {
                        self.error = Some("Answer every question before submitting.".into());
                    }
                }
            }
            Action::Back => {
                self.current = self.current.saturating_sub(1);
                focus = true;
            }
            Action::Editor(action)
                if self.text_open() && self.answers[self.current].replace.is_none() =>
            {
                self.answers[self.current].editor.perform(action)
            }
            Action::Scroll(scroll)
                if self.text_open() && self.answers[self.current].replace.is_none() =>
            {
                scroll.apply(&mut self.answers[self.current].editor)
            }
            Action::DeleteWord
                if self.text_open() && self.answers[self.current].replace.is_none() =>
            {
                delete_previous_word(&mut self.answers[self.current].editor)
            }
            _ => {}
        }
        let scroll = if old_current != self.current {
            widget::operation::snap_to(self.scroll_id(), scrollable::RelativeOffset::START)
        } else if reveal && self.text_open() {
            widget::operation::snap_to(self.scroll_id(), scrollable::RelativeOffset::END)
        } else if reveal {
            iced::advanced::widget::operate(Reveal {
                scroll: self.scroll_id().into(),
                target: format!(
                    "questionnaire-option-{}-{}",
                    self.pending.id,
                    self.answers[self.current]
                        .selected
                        .expect("selected option")
                )
                .into(),
                viewport: None,
                offset: None,
            })
        } else {
            Task::none()
        };
        result.task = Task::batch([scroll, if focus { self.focus() } else { Task::none() }]);
        result
    }
    pub fn view(
        &self,
        font_size: f32,
        max_height: f32,
        enabled: bool,
        take_focus: bool,
    ) -> Element<'_, Message> {
        let message = |action| Message {
            question: self.current,
            action,
        };
        let a = &self.answers[self.current];
        let header = row![
            container(text("Answer needed").size(12)).width(Fill),
            theme::button(
                container(space().width(32).height(4)).style(theme::questionnaire_handle)
            )
            .padding([10, 10])
            .on_press(message(Action::Toggle))
            .style(theme::text_button),
            container(
                text(format!("{} / {}", self.current + 1, self.answers.len()))
                    .size(12)
                    .color(MUTED)
            )
            .align_right(Fill),
        ]
        .align_y(iced::Center)
        .spacing(8);
        let mut panel = column![header].spacing(10);
        if !self.collapsed {
            let mut body = column![
                text(&self.pending.request.questions()[self.current].question).size(font_size)
            ]
            .spacing(8)
            .width(Fill);
            if let Some(index) = a.replace {
                body = body
                    .push(
                        text("Replace your existing custom answer with this option?")
                            .size(font_size),
                    )
                    .push(text(&self.options()[index]).size(font_size).color(MUTED))
                    .push(
                        text("Tab or Left/Right switches actions · Enter confirms")
                            .size(12)
                            .color(MUTED),
                    );
            } else {
                for (index, option) in self
                    .options()
                    .iter()
                    .map(String::as_str)
                    .chain((!self.options().is_empty()).then_some("Custom answer…"))
                    .enumerate()
                {
                    let selected = a.selected == Some(index);
                    let label = container(
                        row![
                            text((index + 1).to_string()).size(12).color(MUTED),
                            text(option).size(font_size)
                        ]
                        .align_y(iced::Center)
                        .spacing(10),
                    )
                    .width(Fill)
                    .padding([7, 10]);
                    let mut option_row = row![label].align_y(iced::Center);
                    if index < self.options().len() {
                        option_row = option_row.push(
                            theme::button(Icon::Pencil.label("Edit", 12.))
                                .padding([7, 10])
                                .on_press(message(Action::Edit(index)))
                                .style(theme::sidebar_control),
                        );
                    }
                    // The row owns the background, including behind Edit. The nested
                    // Edit button captures its clicks before the row can select.
                    body = body.push(
                        container(
                            theme::button(option_row)
                                .width(Fill)
                                .padding(0)
                                .on_press(message(Action::Select(index)))
                                .style(move |theme, status| {
                                    theme::option_button(theme, status, selected)
                                }),
                        )
                        .id(format!("questionnaire-option-{}-{index}", self.pending.id)),
                    );
                }
            }
            let editor_open = self.text_open() && a.replace.is_none();
            let editor_height = (max_height * 0.3).clamp(80., 160.);
            let reserved = 130. + if editor_open { editor_height + 10. } else { 0. };
            // Keep the editor and navigation stationary while long prompts/options
            // scroll independently. The editor has its own multiline scrolling.
            panel = panel.push(
                container(scroll(body).id(self.scroll_id()).height(Length::Shrink))
                    .max_height((max_height - reserved).max(60.)),
            );
            if editor_open {
                let editor = text_editor(&a.editor)
                    .style(theme::editor)
                    .id(self.editor_id())
                    .placeholder("Your answer…")
                    .size(font_size)
                    // Keep the answer field fixed-height while sharing the
                    // composer's overflow gutter and 12 px vertical padding.
                    .min_height(editor_height - 2. * crate::growing_editor::PADDING)
                    .on_action(move |action| message(Action::Editor(action)))
                    .key_binding(move |event| {
                        if matches!(event.status, text_editor::Status::Focused { .. })
                            && word_delete(&event.key, event.modifiers)
                        {
                            Some(text_editor::Binding::Custom(message(Action::DeleteWord)))
                        } else {
                            text_editor::Binding::from_key_press(event)
                        }
                    });
                panel = panel.push(crate::growing_editor::growing_editor(
                    editor,
                    &a.editor,
                    editor_height,
                    move |scroll| message(Action::Scroll(scroll)),
                    |message| match &message.action {
                        Action::Editor(text_editor::Action::Scroll { lines }) => Some(*lines),
                        _ => None,
                    },
                ));
            }
            if a.replace.is_none() {
                panel = panel.push(
                    text(if self.text_open() {
                        "Enter next · Shift+Enter newline · Shift+Tab back"
                    } else {
                        "Up/Down or 1–9, 0 select · E edit selected · Enter/Tab next"
                    })
                    .size(12)
                    .color(MUTED),
                );
            }
            if let Some(error) = &self.error {
                panel = panel.push(text(error).size(12).color(DANGER));
            }
            let controls = if a.replace.is_some() {
                row![
                    theme::button("Keep draft")
                        .on_press(message(Action::Replace(false)))
                        .style(move |theme, status| theme::option_button(
                            theme,
                            status,
                            !a.replace_focused
                        )),
                    theme::button("Replace")
                        .on_press(message(Action::Replace(true)))
                        .style(move |theme, status| theme::option_button(
                            theme,
                            status,
                            a.replace_focused
                        ))
                ]
                .spacing(8)
            } else {
                row![
                    theme::button(Icon::ArrowLeft.label("Back", 16.))
                        .on_press_maybe((self.current > 0).then(|| message(Action::Back)))
                        .style(theme::text_button),
                    space().width(Fill),
                    theme::button(if self.current + 1 == self.answers.len() {
                        Icon::Check.label("Submit", 16.)
                    } else {
                        Icon::ArrowRight.label("Next", 16.)
                    })
                    .on_press_maybe(self.answered(self.current).then(|| message(Action::Next)))
                    .style(theme::text_button)
                ]
                .spacing(8)
            };
            panel = panel.push(controls);
        }
        let content = container(panel)
            .padding(12)
            .width(Fill)
            .style(theme::questionnaire)
            .into();
        Element::new(Input {
            content,
            enabled,
            take_focus,
            collapsed: self.collapsed,
            text_open: self.text_open(),
            confirmation: a.replace.is_some(),
            question: self.current,
            id: &self.pending.id,
        })
    }
}
fn word_delete(key: &keyboard::Key, modifiers: keyboard::Modifiers) -> bool {
    modifiers.control()
        && !modifiers.alt()
        && !modifiers.logo()
        && (key == &keyboard::Key::Named(Named::Backspace)
            || key.as_ref() == keyboard::Key::Character("w"))
}
/// Shared by composer and questionnaires, using logical Unicode word boundaries.
pub fn delete_previous_word(editor: &mut text_editor::Content) {
    let cursor = editor.cursor();
    if cursor.selection.is_none() {
        let mut start = cursor.position;
        if start.column > 0 {
            let line = editor.line(start.line).expect("cursor line exists");
            start.column = line.text[..start.column]
                .split_word_bound_indices()
                .rfind(|(_, word)| !word.trim_start().is_empty())
                .map_or(0, |(index, _)| index);
        } else if start.line > 0 {
            start.line -= 1;
            start.column = editor
                .line(start.line)
                .expect("previous cursor line exists")
                .text
                .len();
        }
        editor.move_to(text_editor::Cursor {
            position: cursor.position,
            selection: Some(start),
        });
    }
    editor.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
}

// Reveal the actual laid-out row rather than estimating heights of wrapped options.
struct Reveal {
    scroll: iced::advanced::widget::Id,
    target: iced::advanced::widget::Id,
    viewport: Option<(Rectangle, Vector)>,
    offset: Option<f32>,
}
impl Operation<Message> for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Message>)) {
        operate(self);
    }
    fn scrollable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        _: Rectangle,
        translation: Vector,
        _: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.viewport = Some((bounds, translation));
        }
    }
    fn container(&mut self, id: Option<&iced::advanced::widget::Id>, bounds: Rectangle) {
        if id == Some(&self.target)
            && let Some((viewport, translation)) = self.viewport
        {
            let top = bounds.y - translation.y;
            self.offset = if top < viewport.y || bounds.height > viewport.height {
                Some(bounds.y - viewport.y)
            } else if top + bounds.height > viewport.y + viewport.height {
                Some(bounds.y + bounds.height - viewport.y - viewport.height)
            } else {
                None
            };
        }
    }
    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Message> {
        use iced::advanced::widget::operation::{Outcome, scrollable};
        match self.offset {
            Some(y) => Outcome::Chain(Box::new(scrollable::scroll_to(
                self.scroll.clone(),
                scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ))),
            None => Outcome::None,
        }
    }
}

// Widget-local focus avoids global keyboard handlers stealing input from chat,
// search or another panel. Child text editors retain their standard mouse/IME behavior.
struct Input<'a> {
    content: Element<'a, Message>,
    enabled: bool,
    take_focus: bool,
    collapsed: bool,
    text_open: bool,
    confirmation: bool,
    question: usize,
    id: &'a str,
}
struct Focus {
    id: String,
    focused: bool,
}
impl Widget<Message, Theme, iced::Renderer> for Input<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Focus {
            id: self.id.into(),
            focused: self.take_focus,
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<Focus>();
        if state.id != self.id {
            *state = Focus {
                id: self.id.into(),
                focused: self.take_focus,
            };
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let focus = tree.state.downcast_mut::<Focus>();
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(_)) => {
                focus.focused = cursor.is_over(layout.bounds())
            }
            Event::Window(iced::window::Event::Unfocused) => focus.focused = false,
            _ => {}
        }
        if matches!(event, Event::Keyboard(_) | Event::InputMethod(_)) {
            if !self.enabled || self.collapsed || !focus.focused {
                return;
            }
            if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
                let plain = !modifiers.control() && !modifiers.alt() && !modifiers.logo();
                let action = if self.confirmation {
                    match key.as_ref() {
                        keyboard::Key::Named(Named::Tab | Named::ArrowLeft | Named::ArrowRight) => {
                            Some(Action::SwitchConfirmation)
                        }
                        keyboard::Key::Named(Named::Enter) => Some(Action::Confirm),
                        _ => None,
                    }
                } else if plain {
                    match key.as_ref() {
                        keyboard::Key::Named(Named::Tab) => Some(if modifiers.shift() {
                            Action::Back
                        } else {
                            Action::Next
                        }),
                        keyboard::Key::Named(Named::Enter) if !modifiers.shift() => {
                            Some(Action::Next)
                        }
                        keyboard::Key::Named(Named::ArrowDown) if !self.text_open => {
                            Some(Action::Move(true))
                        }
                        keyboard::Key::Named(Named::ArrowUp) if !self.text_open => {
                            Some(Action::Move(false))
                        }
                        keyboard::Key::Character("e" | "E") if !self.text_open => {
                            Some(Action::EditSelected)
                        }
                        keyboard::Key::Character(digit) if !self.text_open => digit
                            .parse::<usize>()
                            .ok()
                            .filter(|n| *n <= 9)
                            .map(|n| Action::Select(if n == 0 { 9 } else { n - 1 })),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(action) = action {
                    shell.publish(Message {
                        question: self.question,
                        action,
                    });
                    shell.capture_event();
                    return;
                }
            }
            if self.confirmation {
                shell.capture_event();
                return;
            }
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::operation::focusable;
    use iced_runtime::{UserInterface, user_interface};
    use std::sync::Arc;

    fn draft() -> Draft {
        Draft::new(Pending { id: "fixture".into(), request: Arc::new(pico_core::questionnaire::Request::parse(r#"{"questions":[{"id":"platform","question":"Choose a platform","kind":"select","options":["A website","A mobile app","A command-line tool"]},{"id":"constraints","question":"Describe constraints","kind":"text"}]}"#).unwrap()) })
    }
    fn apply(draft: &mut Draft, action: Action) -> Update {
        draft.update(Message {
            question: draft.current,
            action,
        })
    }
    fn paste(draft: &mut Draft, text: &str) {
        apply(
            draft,
            Action::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new(text.into()),
            ))),
        );
    }
    fn key(key: keyboard::Key, modifiers: keyboard::Modifiers, text: Option<&str>) -> Event {
        Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
    }
    fn events(draft: &mut Draft, enabled: bool, events: &[Event], outside: bool) -> Vec<Message> {
        let mut renderer = crate::test_renderer::new(14.);
        let messages = {
            let mut ui = UserInterface::build(
                draft.view(14., 360., enabled, true),
                Size::new(600., 500.),
                user_interface::Cache::new(),
                &mut renderer,
            );
            ui.operate(&renderer, &mut focusable::focus(draft.editor_id().into()));
            if outside {
                ui.update(
                    &[Event::Mouse(mouse::Event::ButtonPressed(
                        mouse::Button::Left,
                    ))],
                    mouse::Cursor::Available(iced::Point::new(599., 499.)),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut Vec::new(),
                );
            }
            let mut messages = vec![];
            ui.update(
                events,
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            messages
        };
        for message in &messages {
            draft.update(message.clone());
        }
        messages
    }

    #[test]
    fn questionnaire_option_hover_and_edit_share_surface_but_not_clicks() {
        use iced::advanced::Renderer as _;
        struct OptionBounds(Option<Rectangle>);
        impl Operation for OptionBounds {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
                if id == Some(&widget::Id::from("questionnaire-option-fixture-0")) {
                    self.0 = Some(bounds);
                }
            }
        }

        let draft = draft();
        let mut renderer = crate::test_renderer::new(14.);
        let mut ui = UserInterface::build(
            draft.view(14., 360., true, true),
            Size::new(600., 500.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let mut bounds = OptionBounds(None);
        ui.operate(&renderer, &mut bounds);
        let bounds = bounds.0.expect("first option is laid out");
        let label = iced::Point::new(bounds.x + 60., bounds.center_y());
        let edit = iced::Point::new(bounds.x + bounds.width - 20., bounds.center_y());
        let theme = theme::app();
        let hover_background = theme::secondary(&theme, widget::button::Status::Hovered)
            .background
            .unwrap();

        for (cursor, background, edit_color) in [
            (mouse::Cursor::Unavailable, theme::BACKGROUND.into(), MUTED),
            (mouse::Cursor::Available(label), hover_background, MUTED),
            (
                mouse::Cursor::Available(edit),
                hover_background,
                theme::TEXT,
            ),
        ] {
            let mut messages = vec![];
            ui.update(
                &[Event::Window(iced::window::Event::RedrawRequested(
                    iced::time::Instant::now(),
                ))],
                cursor,
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            renderer.reset(Rectangle::with_size(Size::new(600., 500.)));
            ui.draw(
                &mut renderer,
                &theme,
                &renderer::Style {
                    text_color: theme::TEXT,
                },
                cursor,
            );
            let pixels = crate::test_renderer::screenshot(
                &mut renderer,
                Size::new(600, 500),
                1.,
                iced::Color::BLACK,
            );
            let iced::Background::Color(background) = background else {
                panic!("fixture theme must have a solid option background")
            };
            // Sample padding below the text, not the renderer's quad decomposition.
            for point in [label, edit] {
                let offset = (((bounds.y + bounds.height - 4.).floor() as usize) * 600
                    + point.x.floor() as usize)
                    * 4;
                assert_eq!(&pixels[offset..offset + 4], &background.into_rgba8());
            }
            let expected = edit_color.into_rgba8();
            let closest = (bounds.y.ceil() as usize..(bounds.y + bounds.height).floor() as usize)
                .flat_map(|y| {
                    ((bounds.x + bounds.width - 45.).ceil() as usize
                        ..(bounds.x + bounds.width).floor() as usize)
                        .map(move |x| (y * 600 + x) * 4)
                })
                .map(|offset| {
                    (0..3)
                        .map(|c| pixels[offset + c].abs_diff(expected[c]))
                        .max()
                        .unwrap()
                })
                .min()
                .unwrap();
            assert!(
                closest < 30,
                "Edit must paint its state-dependent foreground color"
            );
        }

        for (point, editing) in [(label, false), (edit, true)] {
            let mut messages = vec![];
            ui.update(
                &[
                    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut messages,
            );
            assert_eq!(messages.len(), 1, "Edit must not also select the option");
            assert_eq!(messages[0].question, 0);
            assert!(matches!(
                (&messages[0].action, editing),
                (Action::Edit(0), true) | (Action::Select(0), false)
            ));
        }
    }

    #[derive(Default)]
    struct EditorBounds {
        id: String,
        bounds: Option<iced::Rectangle>,
    }
    impl iced::advanced::widget::Operation for EditorBounds {
        fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
            visit(self);
        }
        fn focusable(
            &mut self,
            id: Option<&iced::advanced::widget::Id>,
            bounds: iced::Rectangle,
            _: &mut dyn iced::advanced::widget::operation::Focusable,
        ) {
            if id.is_some_and(|id| id == &iced::advanced::widget::Id::from(self.id.clone())) {
                self.bounds = Some(bounds);
            }
        }
    }

    #[test]
    fn questionnaire_answer_scrollbar_handles_overflow_without_editing_the_answer() {
        let mut draft = draft();
        apply(&mut draft, Action::Select(3));
        let mut renderer = crate::test_renderer::new(14.);
        let theme = theme::app();
        let mut cache = user_interface::Cache::new();

        // Exercise both custom select answers and text-only questions, including
        // wrapped overflow and shrinking again with retained widget state.
        for question in [0, 1] {
            draft.current = question;
            for (value, overflowing) in [
                ("Short answer".to_owned(), false),
                ("A line of text λ\n".repeat(40), true),
                (
                    "A long wrapped answer with Unicode λ and words. ".repeat(100),
                    true,
                ),
                ("Short again".to_owned(), false),
            ] {
                draft.answers[question].editor = text_editor::Content::with_text(&value);
                let cursor = draft.answers[question].editor.cursor();
                let mut ui = UserInterface::build(
                    draft.view(14., 600., true, true),
                    Size::new(600., 700.),
                    cache,
                    &mut renderer,
                );
                ui.draw(
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: theme::TEXT,
                    },
                    mouse::Cursor::Unavailable,
                );
                let pixels = crate::test_renderer::screenshot(
                    &mut renderer,
                    Size::new(600, 700),
                    1.,
                    theme::BACKGROUND,
                );
                let mut editor = EditorBounds {
                    id: draft.editor_id(),
                    ..EditorBounds::default()
                };
                ui.operate(&renderer, &mut editor);
                let editor = editor.bounds.unwrap();
                let x = (editor.x + editor.width
                    - theme::SCROLLBAR_EDGE_INSET
                    - theme::SCROLLBAR_WIDTH / 2.)
                    .floor() as usize;
                let top = (editor.y + crate::growing_editor::PADDING).ceil() as usize + 1;
                let bottom = (editor.y + editor.height - crate::growing_editor::PADDING).floor()
                    as usize
                    - 1;
                let pixel = |y: usize| &pixels[(y * 600 + x) * 4..(y * 600 + x + 1) * 4];
                let background = pixel(bottom);
                let painted: Vec<_> = (top..bottom).filter(|&y| pixel(y) != background).collect();
                assert_eq!(
                    !painted.is_empty(),
                    overflowing,
                    "question {question}, answer length {}",
                    value.len()
                );
                let mut messages = Vec::new();
                if overflowing {
                    // Use the painted thumb, not renderer-private quad lists, to
                    // click the track below it without moving the caret.
                    let thumb_bottom = *painted.last().unwrap() as f32 + 1.;
                    let point =
                        iced::Point::new(x as f32 + 0.5, (thumb_bottom + bottom as f32) / 2.);
                    ui.update(
                        &[
                            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                        ],
                        mouse::Cursor::Available(point),
                        &mut renderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut messages,
                    );
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0].question, question);
                    assert!(matches!(messages[0].action, Action::Scroll(_)));
                }
                cache = ui.into_cache();
                for message in messages {
                    draft.update(message);
                }
                assert_eq!(draft.current, question);
                assert_eq!(draft.answers[question].editor.cursor(), cursor);
                if overflowing {
                    draft.answers[question].editor.with_editor(|editor| {
                        let scroll = editor.buffer().scroll();
                        assert!(scroll.line > 0 || scroll.vertical > 0.);
                    });
                }
                assert_eq!(draft.value(question).as_deref(), Some(value.as_str()));
            }
        }
    }

    #[test]
    fn questionnaire_seeded_editor_has_visible_glyphs() {
        let mut draft = draft();
        apply(&mut draft, Action::Edit(1));
        let mut renderer = crate::test_renderer::new(14.);
        let mut ui = UserInterface::build(
            draft.view(14., 360., true, true),
            Size::new(600., 500.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        ui.draw(
            &mut renderer,
            &theme::app(),
            &iced::advanced::renderer::Style {
                text_color: theme::TEXT,
            },
            mouse::Cursor::Unavailable,
        );
        let mut editor = EditorBounds {
            id: draft.editor_id(),
            ..EditorBounds::default()
        };
        ui.operate(&renderer, &mut editor);
        let bounds = editor.bounds.expect("custom editor is mounted");
        let pixels = crate::test_renderer::screenshot(
            &mut renderer,
            Size::new(600, 500),
            1.,
            theme::BACKGROUND,
        );
        let foreground = theme::TEXT.into_rgba8();
        let padding = crate::growing_editor::PADDING;
        let columns: Vec<_> = ((bounds.x + padding).ceil() as usize
            ..(bounds.x + bounds.width - padding).floor() as usize)
            .filter(|&x| {
                ((bounds.y + padding).ceil() as usize
                    ..(bounds.y + bounds.height - padding).floor() as usize)
                    .any(|y| {
                        (0..3).all(|c| pixels[(y * 600 + x) * 4 + c].abs_diff(foreground[c]) < 30)
                    })
            })
            .collect();
        assert!(
            columns.len() > 14,
            "seeded text must paint visible glyphs inside the editor, not just a caret"
        );
        assert_eq!(draft.value(0).as_deref(), Some("A mobile app"));
        assert_eq!(
            draft.answers[0].editor.cursor().position.column,
            "A mobile app".len()
        );
    }

    #[test]
    fn questionnaire_navigation_edit_confirmation_and_unbounded_answers() {
        let mut d = draft();
        assert!(!d.answered(0));
        apply(&mut d, Action::Next);
        assert_eq!(d.current, 0);
        assert!(d.error.is_some());
        apply(&mut d, Action::Select(1));
        apply(&mut d, Action::EditSelected);
        assert_eq!(d.value(0).as_deref(), Some("A mobile app"));
        assert_eq!(d.current, 0, "Edit must not advance");
        paste(&mut d, " with offline mode");
        apply(&mut d, Action::Edit(0));
        assert!(d.answers[0].replace.is_some());
        events(
            &mut d,
            true,
            &[
                key(
                    keyboard::Key::Character("x".into()),
                    keyboard::Modifiers::empty(),
                    Some("x"),
                ),
                key(
                    keyboard::Key::Named(Named::Enter),
                    keyboard::Modifiers::empty(),
                    None,
                ),
            ],
            false,
        );
        assert_eq!(
            d.value(0).as_deref(),
            Some("A mobile app with offline mode"),
            "Keep draft is the default and typing must not edit it"
        );
        apply(&mut d, Action::Edit(2));
        apply(&mut d, Action::SwitchConfirmation);
        apply(&mut d, Action::Confirm);
        assert_eq!(d.value(0).as_deref(), Some("A command-line tool"));
        assert_eq!(d.options()[1], "A mobile app", "options are immutable");
        apply(&mut d, Action::Next);
        let long = "λ界\\n".repeat(20_000);
        paste(&mut d, &long);
        apply(&mut d, Action::Back);
        assert_eq!(d.value(0).as_deref(), Some("A command-line tool"));
        apply(&mut d, Action::Toggle);
        assert!(d.collapsed);
        assert!(
            events(
                &mut d,
                true,
                &[key(
                    keyboard::Key::Named(Named::Enter),
                    keyboard::Modifiers::empty(),
                    None
                )],
                false
            )
            .is_empty()
        );
        apply(&mut d, Action::Toggle);
        apply(&mut d, Action::Next);
        assert_eq!(
            apply(&mut d, Action::Next).answers.unwrap(),
            ["A command-line tool".to_owned(), long]
        );
    }

    #[test]
    fn questionnaire_keyboard_routes_options_text_and_focus_without_double_advancing() {
        // A newly arriving panel must not take typing from an open Settings field.
        let d = draft();
        let mut renderer = crate::test_renderer::new(14.);
        let mut ui = UserInterface::build(
            d.view(14., 360., true, false),
            Size::new(600., 500.),
            user_interface::Cache::new(),
            &mut renderer,
        );
        let select = key(
            keyboard::Key::Character("2".into()),
            keyboard::Modifiers::empty(),
            Some("2"),
        );
        let mut messages = vec![];
        ui.update(
            std::slice::from_ref(&select),
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty());
        ui.update(
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left,
            ))],
            mouse::Cursor::Available(iced::Point::new(20., 20.)),
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        ui.update(
            &[select],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut iced::advanced::clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            messages.as_slice(),
            [Message {
                action: Action::Select(1),
                ..
            }]
        ));
        drop(ui);
        let mut d = draft();
        events(
            &mut d,
            true,
            &[key(
                keyboard::Key::Character("2".into()),
                keyboard::Modifiers::empty(),
                Some("2"),
            )],
            false,
        );
        assert_eq!(d.value(0).as_deref(), Some("A mobile app"));
        events(
            &mut d,
            true,
            &[key(
                keyboard::Key::Character("e".into()),
                keyboard::Modifiers::empty(),
                Some("e"),
            )],
            false,
        );
        assert!(d.text_open());
        events(
            &mut d,
            true,
            &[key(
                keyboard::Key::Character("2".into()),
                keyboard::Modifiers::empty(),
                Some("2"),
            )],
            false,
        );
        assert_eq!(d.value(0).as_deref(), Some("A mobile app2"));
        events(
            &mut d,
            true,
            &[key(
                keyboard::Key::Named(Named::Enter),
                keyboard::Modifiers::SHIFT,
                None,
            )],
            false,
        );
        assert_eq!(d.value(0).as_deref(), Some("A mobile app2\n"));
        let next = key(
            keyboard::Key::Named(Named::Enter),
            keyboard::Modifiers::empty(),
            None,
        );
        assert!(events(&mut d, false, std::slice::from_ref(&next), false).is_empty());
        assert!(events(&mut d, true, std::slice::from_ref(&next), true).is_empty());
        assert_eq!(events(&mut d, true, &[next], false).len(), 1);
        assert_eq!(d.current, 1);
        events(
            &mut d,
            true,
            &[key(
                keyboard::Key::Named(Named::Tab),
                keyboard::Modifiers::SHIFT,
                None,
            )],
            false,
        );
        assert_eq!(d.current, 0);
    }

    #[test]
    fn questionnaire_both_word_delete_shortcuts_preserve_suffix_selection_and_unicode() {
        for key_name in [
            keyboard::Key::Named(Named::Backspace),
            keyboard::Key::Character("w".into()),
        ] {
            for (value, line, column, selection, expected) in [
                ("prefix foo/bar tail", 0, 14, None, "prefix foo/ tail"),
                ("one\nsecond", 1, 0, None, "onesecond"),
                ("héllo λ界 suffix", 0, 9, Some((0, 7)), "héllo 界 suffix"),
                ("left selected right", 0, 13, Some((0, 5)), "left  right"),
                ("", 0, 0, None, ""),
            ] {
                let mut d = draft();
                apply(&mut d, Action::Select(3));
                d.answers[0].editor = text_editor::Content::with_text(value);
                d.answers[0].editor.move_to(text_editor::Cursor {
                    position: text_editor::Position { line, column },
                    selection: selection
                        .map(|(line, column)| text_editor::Position { line, column }),
                });
                let messages = events(
                    &mut d,
                    true,
                    &[key(key_name.clone(), keyboard::Modifiers::CTRL, None)],
                    false,
                );
                assert!(
                    matches!(
                        messages.as_slice(),
                        [Message {
                            action: Action::DeleteWord,
                            ..
                        }]
                    ),
                    "{messages:?}"
                );
                assert_eq!(
                    d.answers[0].editor.text(),
                    expected,
                    "{key_name:?}: {value}"
                );
            }
        }
    }
}
