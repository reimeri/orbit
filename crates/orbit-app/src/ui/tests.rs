mod attention;
mod chat_messages;
mod composer;
mod drafts;
mod filing;
mod footer;
mod fork_actions;
mod images;
mod import_probe;
mod long_reply;
mod panels_and_menus;
mod projects;
mod questionnaires;
mod session_lifecycle;
mod session_navigation;
mod settings_panel;
mod sidebar;
mod tool_activity;
mod transcript_layout;
mod worktree_handoff;

use super::*;
use orbit_core::agent::ConversationStatus;

mod sqlite {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../orbit-core/src/test_support/sqlite.rs"
    ));
}

pub(super) fn app() -> App {
    App::new(crate::Launch::Smoke, Arc::new(AtomicBool::new(false))).0
}

async fn stored_session(app: &mut App, path: PathBuf) {
    let session = Session::new(path, &app.catalog.settings);
    let id = session.id.clone();
    let h = app.harness.as_ref().unwrap();
    h.store.save_session(&session).unwrap();
    h.insert(session);
    dispatch(app, Message::Select(id)).await;
}
pub(super) async fn dispatch(app: &mut App, message: Message) {
    let task = app.update(message);
    drain(app, task).await;
}
async fn drain(app: &mut App, task: Task<Message>) {
    use iced::futures::StreamExt;
    let mut image_renderer = None;
    let mut pending = std::collections::VecDeque::from([task]);
    while let Some(task) = pending.pop_front() {
        if let Some(mut stream) = iced_runtime::task::into_stream(task) {
            while let Some(action) = stream.next().await {
                match action {
                    iced_runtime::Action::Output(message) => pending.push_back(app.update(message)),
                    iced_runtime::Action::Image(iced_runtime::image::Action::Allocate(
                        handle,
                        sender,
                    )) => {
                        use iced::advanced::image::Renderer as _;
                        let renderer =
                            image_renderer.get_or_insert_with(|| crate::test_renderer::new(16.));
                        sender.send(renderer.load_image(&handle)).unwrap();
                    }
                    _ => {}
                }
            }
        }
    }
}
async fn copied_text(task: Task<Message>) -> String {
    use iced::futures::StreamExt;
    let mut stream = iced_runtime::task::into_stream(task).expect("copy action");
    let mut copied = None;
    while let Some(action) = stream.next().await {
        if let iced_runtime::Action::Clipboard(iced_runtime::clipboard::Action::Write {
            target,
            contents,
        }) = action
        {
            assert_eq!(target, iced::advanced::clipboard::Kind::Standard);
            assert!(
                copied.replace(contents).is_none(),
                "one clipboard write per copy"
            );
        }
    }
    copied.expect("clipboard write")
}

async fn type_message(app: &mut App, message: &str) {
    dispatch(
        app,
        Message::Editor(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new(message.to_owned()),
        ))),
    )
    .await;
}

fn header_renderer() -> iced::Renderer {
    crate::test_renderer::new(16.)
}

fn displayed_text(view: Element<'_, Message>, size: iced::Size) -> String {
    let mut renderer = header_renderer();
    let mut ui = iced_runtime::UserInterface::build(
        view,
        size,
        iced_runtime::user_interface::Cache::new(),
        &mut renderer,
    );
    let mut bounds = UiBounds::default();
    ui.operate(&renderer, &mut bounds);
    bounds
        .labels
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>()
        .concat()
}
fn entry_text(app: &App, index: usize) -> String {
    displayed_text(
        app.entry_view(&app.current().unwrap().entries[index], index),
        iced::Size::new(900., 1350.),
    )
}

fn chat_text(app: &App) -> String {
    displayed_text(app.center(), iced::Size::new(930., 1350.))
}

fn click_at(
    ui: &mut iced_runtime::UserInterface<'_, Message, Theme, iced::Renderer>,
    renderer: &mut iced::Renderer,
    point: iced::Point,
) -> Vec<Message> {
    let mut messages = Vec::new();
    ui.update(
        &[
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ],
        mouse::Cursor::Available(point),
        renderer,
        &mut iced::advanced::clipboard::Null,
        &mut messages,
    );
    messages
}

#[derive(Default)]
struct UiBounds {
    find_input: Option<iced::Rectangle>,
    composer: Option<iced::Rectangle>,
    focused: Option<iced::advanced::widget::Id>,
    labels: Vec<(String, iced::Rectangle)>,
    containers: Vec<iced::Rectangle>,
    scrolls: Vec<(iced::advanced::widget::Id, iced::Rectangle)>,
}
impl iced::advanced::widget::Operation for UiBounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation)) {
        operate(self);
    }
    fn container(&mut self, _: Option<&iced::advanced::widget::Id>, bounds: iced::Rectangle) {
        self.containers.push(bounds);
    }
    fn focusable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if id == Some(&iced::advanced::widget::Id::new("conversation-find")) {
            self.find_input = Some(bounds);
        }
        if id == Some(&iced::advanced::widget::Id::new("composer")) {
            self.composer = Some(bounds);
        }
        if state.is_focused() {
            self.focused = id.cloned();
        }
    }
    fn scrollable(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        _: iced::Rectangle,
        _: iced::Vector,
        _: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if let Some(id) = id {
            self.scrolls.push((id.clone(), bounds));
        }
    }
    fn text(
        &mut self,
        _: Option<&iced::advanced::widget::Id>,
        bounds: iced::Rectangle,
        value: &str,
    ) {
        self.labels.push((value.into(), bounds));
    }
}
impl UiBounds {
    fn leftmost_control(&self) -> iced::Point {
        self.containers
            .iter()
            .min_by(|a, b| a.x.total_cmp(&b.x).then(a.width.total_cmp(&b.width)))
            .unwrap()
            .center()
    }
    // Locate icon-only controls by their relationship to the visible layout,
    // not by font glyphs or implementation-specific SVG paths.
    fn rightmost_control_at(&self, y: f32) -> iced::Rectangle {
        *self
            .containers
            .iter()
            .filter(|bounds| (bounds.center_y() - y).abs() < 0.5)
            .max_by(|a, b| {
                a.x.total_cmp(&b.x)
                    .then_with(|| b.width.total_cmp(&a.width))
            })
            .expect("control at row center")
    }
    fn scroll(&self, name: &'static str) -> iced::Rectangle {
        let id = iced::advanced::widget::Id::new(name);
        self.scrolls
            .iter()
            .find(|(key, _)| *key == id)
            .unwrap_or_else(|| panic!("missing scrollable {name}"))
            .1
    }
    fn label(&self, value: &str) -> iced::Rectangle {
        self.labels
            .iter()
            .find(|(text, _)| text == value)
            .unwrap_or_else(|| panic!("missing label {value}"))
            .1
    }
}

fn footer_key(key: keyboard::key::Named) -> Message {
    Message::Event(iced::Event::Keyboard(keyboard::Event::KeyPressed {
        key: keyboard::Key::Named(key),
        modified_key: keyboard::Key::Named(key),
        physical_key: keyboard::key::Physical::Code(keyboard::key::Code::Enter),
        location: keyboard::Location::Standard,
        modifiers: keyboard::Modifiers::empty(),
        text: None,
        repeat: false,
    }))
}
