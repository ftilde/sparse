use std::cell::RefCell;
use std::fmt::Write;
use std::io::stdout;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use unsegen::base::*;
use unsegen::input::{
    EditBehavior, Editable, Input, Key, OperationResult, ScrollBehavior, Scrollable,
};
use unsegen::widget::builtin::*;
use unsegen::widget::*;

use matrix_sdk::ruma::events::{room::message::MessageType, AnySyncMessageEvent};
use matrix_sdk::ruma::identifiers::{EventId, RoomId};

use crate::timeline::{EventWalkResult, EventWalkResultNewest, MessageQuery, RoomTimelineCache};
use crate::tui_app::State;

struct Rooms<'a>(&'a State, &'a TuiState);

impl Widget for Rooms<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let w = self
            .0
            .rooms()
            .values()
            .map(|s| text_width(s))
            .max()
            .unwrap_or(PositiveAxisDiff::new_unchecked(0));
        let h = self.0.rooms().len();
        Demand2D {
            width: ColDemand::exact(w),
            height: RowDemand::exact(h),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        let mut c = Cursor::new(&mut window);
        for (id, room) in self.0.rooms().iter() {
            let mut c = c.save().style_modifier();
            if Some(id) == self.1.current_room.as_ref().map(|r| &r.id) {
                c.apply_style_modifier(StyleModifier::new().invert(true));
            }
            c.writeln(room);
        }
    }
}

struct RoomsMut<'a>(&'a mut State, &'a mut TuiState);

impl Scrollable for RoomsMut<'_> {
    //TODO: we may want wrapping?
    fn scroll_backwards(&mut self) -> OperationResult {
        self.1.current_room = if let Some(current) = self.1.current_room.take() {
            let mut it = self.0.rooms().range(..current.id.clone()).rev();
            Some(
                it.next()
                    .map(|(k, _)| RoomState::at_last_message(k))
                    .unwrap_or(current),
            )
        } else {
            self.0
                .rooms()
                .keys()
                .rev()
                .next()
                .map(RoomState::at_last_message)
        };
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        self.1.current_room = if let Some(current) = self.1.current_room.take() {
            let mut it = self.0.rooms().range(current.id.clone()..);
            it.next();
            Some(
                it.next()
                    .map(|(k, _v)| RoomState::at_last_message(k))
                    .unwrap_or(current),
            )
        } else {
            self.0.rooms().keys().next().map(RoomState::at_last_message)
        };
        Ok(())
    }
}

#[derive(Copy, Clone)]
struct Tasks<'a> {
    tasks: &'a RefCell<Vec<Task>>,
    message_query: &'a RefCell<Option<MessageQueryRequest>>,
}

impl Tasks<'_> {
    fn set_message_query(&self, room: RoomId, query: MessageQuery) {
        let mut q = self.message_query.borrow_mut();
        *q = Some(MessageQueryRequest { room, kind: query });
    }

    fn add_task(&self, task: Task) {
        let mut t = self.tasks.borrow_mut();
        t.push(task);
    }
}
struct MessagesMut<'a>(&'a State, &'a mut TuiState);

impl Scrollable for MessagesMut<'_> {
    fn scroll_backwards(&mut self) -> OperationResult {
        let mut room = self.1.current_room.as_mut().ok_or(())?;
        let messages = self.0.messages().get(&room.id).ok_or(())?;
        let pos = match &room.current_message {
            MessageSelection::Newest => messages.walk_from_newest().message(),
            MessageSelection::Specific(id) => {
                let pos = messages.walk_from_known(&id).message().ok_or(())?;
                messages.previous(pos).message()
            }
        }
        .ok_or(())?;
        room.current_message = MessageSelection::Specific(messages.message(pos).event_id().clone());
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        let mut room = self.1.current_room.as_mut().ok_or(())?;
        let messages = self.0.messages().get(&room.id).ok_or(())?;
        let pos = match &room.current_message {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(id) => messages.walk_from_known(&id).message(),
        }
        .ok_or(())?;
        room.current_message = match messages.next(pos) {
            EventWalkResult::End => MessageSelection::Newest,
            EventWalkResult::Message(pos) => {
                MessageSelection::Specific(messages.message(pos).event_id().clone())
            }
            EventWalkResult::RequiresFetchFrom(_) => return Err(()),
        };
        Ok(())
    }

    fn scroll_to_end(&mut self) -> OperationResult {
        let mut room = self.1.current_room.as_mut().ok_or(())?;
        room.current_message = match &room.current_message {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(_id) => MessageSelection::Newest,
        };
        Ok(())
    }
}

struct TuiEvent<'a>(&'a crate::timeline::Event);

impl TuiEvent<'_> {
    fn header(&self) -> Option<String> {
        match self.0 {
            crate::timeline::Event::Message(e) => match e {
                AnySyncMessageEvent::RoomMessage(msg) => Some(format!("{}: ", msg.sender)),
                AnySyncMessageEvent::RoomEncrypted(msg) => Some(format!("{}: ", msg.sender)),
                _o => None,
            },
            _o => None,
        }
    }
    fn header_size(&self) -> ColDemand {
        ColDemand::exact(
            self.header()
                .map(|s| text_width(&s))
                .unwrap_or(Width::new(0).unwrap()),
        )
    }
    fn content(&self) -> String {
        match self.0 {
            crate::timeline::Event::Message(e) => match e {
                AnySyncMessageEvent::RoomMessage(msg) => match &msg.content.msgtype {
                    MessageType::Text(text) => {
                        format!("{}: {:?}, {}", msg.sender, msg.event_id, text.body)
                    }
                    o => {
                        format!("{}: Other message {:?}", msg.sender, o)
                    }
                },
                AnySyncMessageEvent::RoomEncrypted(msg) => {
                    format!(
                        "{}: {:?}, *Unable to decrypt message*",
                        msg.sender, msg.event_id
                    )
                }
                o => {
                    format!("Other event {:?}", o)
                }
            },
            o => {
                format!("Other event {:?}", o)
            }
        }
    }
    fn content_size(&self) -> Demand2D {
        self.content().space_demand()
    }
}

impl Widget for TuiEvent<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let h = self.header_size();
        let c = self.content_size();
        Demand2D {
            width: h + c.width,
            height: c.height,
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        // Apply initial background style to whole window
        window.clear();

        let mut c = Cursor::new(&mut window);
        if let Some(header) = self.header() {
            c.write(&header);
            let start = c.get_col();
            c.set_line_start_column(start);
        }
        c.set_wrapping_mode(WrappingMode::Wrap);

        let _ = write!(c, "{}", self.content());
    }
}

struct Messages<'a>(&'a State, &'a TuiState, Tasks<'a>);

impl Messages<'_> {
    fn draw_up_from<'b>(
        &self,
        mut window: Window,
        hints: RenderingHints,
        mut msg: EventWalkResult<'b>,
        room: &RoomState,
        messages: &'b RoomTimelineCache,
    ) {
        loop {
            msg = match msg {
                EventWalkResult::Message(id) => {
                    let evt = TuiEvent(messages.message(id));
                    let h = evt.space_demand().height.min;
                    let window_height = window.get_height();
                    let (above, below) = match window.split((window_height - h).from_origin()) {
                        Ok(pair) => pair,
                        Err(_) => {
                            break;
                        }
                    };

                    evt.draw(below, hints);
                    window = above;
                    messages.previous(id)
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetchFrom(_tok) => {
                    let mut c = Cursor::new(&mut window);
                    write!(&mut c, "[...]").unwrap();
                    self.2
                        .set_message_query(room.id.clone(), MessageQuery::BeforeCache);
                    break;
                }
            };
        }
    }
    fn draw_newest(
        &self,
        mut window: Window,
        hints: RenderingHints,
        room: &RoomState,
        messages: &RoomTimelineCache,
    ) {
        let msg = match messages.walk_from_newest() {
            EventWalkResultNewest::Message(m) => EventWalkResult::Message(m),
            EventWalkResultNewest::End => return,
            EventWalkResultNewest::RequiresFetch => {
                tracing::warn!("fetch newest");
                let mut c = Cursor::new(&mut window);
                write!(&mut c, "[...]").unwrap();
                self.2
                    .set_message_query(room.id.clone(), MessageQuery::Newest);
                return;
            }
        };
        self.draw_up_from(window, hints, msg, room, messages);
    }
    fn draw_selected(
        &self,
        window: Window,
        hints: RenderingHints,
        selected_msg: &EventId,
        room: &RoomState,
        messages: &RoomTimelineCache,
    ) {
        let start_msg = messages.walk_from_known(selected_msg);
        let mut msg = start_msg.clone();
        let mut collected_height = Height::new(0).unwrap();
        let window_height = window.get_height();
        loop {
            match msg {
                EventWalkResult::Message(id) => {
                    collected_height += TuiEvent(messages.message(id)).space_demand().height.min;
                    msg = messages.next(id);
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetchFrom(_tok) => {
                    collected_height += Height::new(1).unwrap();
                    break;
                }
            }
            if collected_height > window_height / 2 {
                break;
            }
        }
        let (above_selected, below_selected) =
            match window.split((window_height - collected_height).from_origin()) {
                Ok((above, below)) => (Some(above), below),
                Err(w) => (None, w),
            };
        if let (Some(above), Some(evt)) = (
            above_selected,
            start_msg.message().map(|id| messages.previous(id)),
        ) {
            self.draw_up_from(above, hints, evt, room, messages);
        }
        let mut window = below_selected;
        let mut msg = start_msg;
        let mut drawing_selected = true;
        loop {
            msg = match msg {
                EventWalkResult::Message(id) => {
                    let evt = TuiEvent(messages.message(id));
                    let h = evt.space_demand().height.min;
                    let (mut current, below) = match window.split(h.from_origin()) {
                        Ok(pair) => pair,
                        Err(_) => {
                            break;
                        }
                    };

                    if drawing_selected {
                        current.set_default_style(
                            StyleModifier::new().invert(true).apply_to_default(),
                        );
                        drawing_selected = false;
                    }
                    evt.draw(current, hints);
                    window = below;
                    messages.next(id)
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetchFrom(_tok) => {
                    let mut c = Cursor::new(&mut window);
                    write!(&mut c, "[...]").unwrap();
                    self.2
                        .set_message_query(room.id.clone(), MessageQuery::AfterCache);
                    break;
                }
            };
        }
    }
}

impl Widget for Messages<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        Demand2D {
            width: ColDemand::at_least(Width::new(0).unwrap()),
            height: RowDemand::at_least(Height::new(0).unwrap()),
        }
    }

    fn draw(&self, mut window: Window, hints: RenderingHints) {
        if let Some(room) = self.1.current_room.as_ref() {
            if let Some(messages) = self.0.messages().get(&room.id) {
                match &room.current_message {
                    MessageSelection::Newest => self.draw_newest(window, hints, room, messages),
                    MessageSelection::Specific(id) => {
                        self.draw_selected(window, hints, id, room, messages)
                    }
                }
            } else {
                let mut c = Cursor::new(&mut window);
                write!(&mut c, "[...]").unwrap();
                let query = match &room.current_message {
                    MessageSelection::Newest => MessageQuery::Newest,
                    MessageSelection::Specific(_id) => MessageQuery::BeforeCache,
                };
                self.2.set_message_query(room.id.clone(), query);
            }
        }
    }
}

enum MessageSelection {
    Newest,
    Specific(EventId),
}

struct RoomState {
    id: RoomId,
    current_message: MessageSelection,
}

impl RoomState {
    fn at_last_message(id: &RoomId) -> Self {
        RoomState {
            id: id.clone(),
            current_message: MessageSelection::Newest,
        }
    }
}

struct TuiState {
    msg_edit: PromptLine,
    current_room: Option<RoomState>,
}

fn tui<'a>(state: &'a State, tui_state: &'a TuiState, tasks: Tasks<'a>) -> impl Widget + 'a {
    HLayout::new()
        .separator(GraphemeCluster::try_from('|').unwrap())
        .widget_weighted(Rooms(state, tui_state), 0.0)
        .widget_weighted(
            VLayout::new()
                .separator(GraphemeCluster::try_from('-').unwrap())
                .widget(Messages(state, tui_state, tasks))
                .widget(tui_state.msg_edit.as_widget()),
            1.0,
        )
}

#[derive(Debug)]
pub enum Event {
    Update,
    Input(Input),
}

#[derive(Debug)]
pub enum Task {
    Send(RoomId, String),
}

#[derive(Clone)]
pub struct MessageQueryRequest {
    pub room: RoomId,
    pub kind: MessageQuery,
}

pub async fn run_tui(
    mut events: mpsc::Receiver<Event>,
    task_sink: mpsc::Sender<Task>,
    message_query_sink: watch::Sender<Option<MessageQueryRequest>>,
    state: Arc<Mutex<State>>,
) {
    let stdout = stdout();
    let mut term = Terminal::new(stdout.lock()).unwrap();
    let mut tui_state = {
        let state = state.lock().await;

        let current_room = if let Some(id) = state.rooms().keys().next() {
            Some(RoomState {
                id: id.clone(),
                current_message: MessageSelection::Newest,
            })
        } else {
            None
        };
        TuiState {
            msg_edit: PromptLine::with_prompt(" > ".to_owned()),
            current_room,
        }
    };

    let mut run = true;

    let task_vec = RefCell::new(Vec::new());
    let message_query = RefCell::new(None);

    let tasks = Tasks {
        tasks: &task_vec,
        message_query: &message_query,
    };
    while run {
        {
            let state = state.lock().await;
            let win = term.create_root_window();
            tui(&state, &tui_state, tasks).draw(win, RenderingHints::new().active(true));
        }
        term.present();

        // TODO: somehow we need to make sure that this does not block. at the moment it still
        // might do so because the channel has 5 elements.
        for t in tasks.tasks.borrow_mut().drain(..) {
            task_sink.send(t).await.unwrap();
        }
        if let Some(query) = tasks.message_query.borrow_mut().take() {
            if message_query_sink.send(Some(query)).is_err() {
                return;
            }
        }

        match events.recv().await.unwrap() {
            Event::Update => {}
            Event::Input(i) => {
                let mut state = state.lock().await;
                i.chain((Key::Ctrl('q'), || run = false))
                    .chain(
                        ScrollBehavior::new(&mut RoomsMut(&mut state, &mut tui_state))
                            .forwards_on(Key::Ctrl('n'))
                            .backwards_on(Key::Ctrl('p')),
                    )
                    .chain(
                        ScrollBehavior::new(&mut MessagesMut(&state, &mut tui_state))
                            .forwards_on(Key::Down)
                            .backwards_on(Key::Up)
                            .to_end_on(Key::Ctrl('g')),
                    )
                    .chain(
                        EditBehavior::new(&mut tui_state.msg_edit)
                            .delete_forwards_on(Key::Delete)
                            .delete_backwards_on(Key::Backspace)
                            .clear_on(Key::Ctrl('c')),
                    )
                    .chain((Key::Char('\n'), || {
                        if let Some(room) = &tui_state.current_room {
                            let msg = tui_state.msg_edit.get().to_owned();
                            if !msg.is_empty() {
                                tasks.add_task(Task::Send(room.id.clone(), msg));
                                tui_state.msg_edit.clear().unwrap();
                            }
                        }
                    }));
            }
        }
    }
}
// This ain't working because of stdin lock. we would need support in unsegen/termion
//pub async fn run_keyboard_loop(sink: Sender<Event>) {
//    let stdin = tokio::io::stdin();
//    for e in Input::read_all(stdin) {
//        if let Err(_) = sink.send(Event::Input(e.expect("event"))).await {
//            break;
//        }
//    }
//}

pub fn start_keyboard_thread(sink: mpsc::Sender<Event>) {
    let _ = std::thread::Builder::new()
        .name("input".to_owned())
        .spawn(move || {
            let stdin = ::std::io::stdin();
            let stdin = stdin.lock();
            for e in Input::read_all(stdin) {
                sink.blocking_send(Event::Input(e.expect("event"))).unwrap();
            }
        });
}
