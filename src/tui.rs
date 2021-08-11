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

use nix::sys::signal;

const DRAW_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(16);
macro_rules! message_fetch_symbol {
    () => {
        "[...]"
    };
}

struct Rooms<'a>(&'a State, &'a TuiState);

impl<'a> Rooms<'a> {
    fn all_rooms(&self) -> impl Iterator<Item = (RoomId, String)> + 'a {
        self.0.rooms().iter().map(|(i, r)| (i.clone(), r.clone()))
    }
    fn active_rooms(&self) -> Vec<(RoomId, String)> {
        match &self.1.mode {
            Mode::RoomFilter(s) => {
                let s = s.get();
                let s_lower = s.to_lowercase();
                let mixed = s != s_lower;
                let rooms = self.all_rooms();
                if mixed {
                    rooms.filter(|(_i, r)| r.contains(s)).collect()
                } else {
                    rooms
                        .filter(|(_i, r)| r.to_lowercase().contains(&s_lower))
                        .collect()
                }
            }
            _ => self.all_rooms().collect(),
        }
    }
    fn active_contains_current(&self) -> bool {
        if let Some(current) = &self.1.current_room {
            self.active_rooms()
                .into_iter()
                .find(|(id, _)| *id == current.id)
                .is_some()
        } else {
            false
        }
    }
}

impl Widget for Rooms<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let active = self.active_rooms();
        let h = active.len();
        let w = active
            .into_iter()
            .map(|(_i, s)| text_width(&s))
            .max()
            .unwrap_or(PositiveAxisDiff::new_unchecked(0));
        let mut d = Demand2D {
            width: ColDemand::exact(w),
            height: RowDemand::exact(h),
        };
        if let Mode::RoomFilter(s) = &self.1.mode {
            let ld = s.as_widget().space_demand();
            d.height += ld.height;
            d.width = d.width.max(ld.width);
        }
        d
    }

    fn draw(&self, window: Window, hints: RenderingHints) {
        let mut window = if let Mode::RoomFilter(filter_line) = &self.1.mode {
            let s = window.get_height() - 1;
            match window.split(s.from_origin()) {
                Ok((above, below)) => {
                    filter_line.as_widget().draw(below, hints);
                    above
                }
                Err(w) => w,
            }
        } else {
            window
        };
        let mut c = Cursor::new(&mut window);
        for (id, room) in self.active_rooms() {
            let mut c = c.save().style_modifier();
            if Some(&id) == self.1.current_room.as_ref().map(|r| &r.id) {
                c.apply_style_modifier(StyleModifier::new().invert(true));
            }
            c.writeln(&room);
        }
    }
}

struct RoomsMut<'a>(&'a mut State, &'a mut TuiState);

impl RoomsMut<'_> {
    fn as_rooms<'b>(&'b self) -> Rooms<'b> {
        Rooms(self.0, self.1)
    }
}
impl Scrollable for RoomsMut<'_> {
    //TODO: we may want wrapping?
    fn scroll_backwards(&mut self) -> OperationResult {
        self.1.current_room = if let Some(current) = self.1.current_room.take() {
            let mut it = self
                .as_rooms()
                .active_rooms()
                .into_iter()
                .rev()
                .skip_while(|(id, _)| id != &current.id);
            it.next();
            Some(
                it.next()
                    .or(self.as_rooms().active_rooms().into_iter().rev().next())
                    .map(|(k, _)| RoomState::at_last_message(&k))
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
            let mut it = self
                .as_rooms()
                .active_rooms()
                .into_iter()
                .skip_while(|(id, _)| id != &current.id);
            it.next();
            Some(
                it.next()
                    .or(self.as_rooms().active_rooms().into_iter().next())
                    .map(|(k, _)| RoomState::at_last_message(&k))
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

struct TuiEvent<'a>(&'a crate::timeline::Event, Width);

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

    fn draw_with_cursor<T: unsegen::base::CursorTarget>(&self, c: &mut Cursor<T>) {
        if let Some(header) = self.header() {
            c.write(&header);
            let start = c.get_col();
            c.set_line_start_column(start);
        }
        c.set_wrapping_mode(WrappingMode::Wrap);

        match self.0 {
            crate::timeline::Event::Message(e) => match e {
                AnySyncMessageEvent::RoomMessage(msg) => match &msg.content.msgtype {
                    MessageType::Text(text) => c.write(&text.body),
                    o => {
                        let _ = write!(c, "Other message {:?}", o);
                    }
                },
                AnySyncMessageEvent::RoomEncrypted(_msg) => {
                    c.write("*Unable to decrypt message*");
                }
                o => {
                    let _ = write!(c, "Other event {:?}", o);
                }
            },
            o => {
                let _ = write!(c, "Other event {:?}", o);
            }
        }
    }
}

impl Widget for TuiEvent<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let mut est = unsegen::base::window::ExtentEstimationWindow::with_width(self.1);
        let mut c = Cursor::new(&mut est);
        self.draw_with_cursor(&mut c);
        Demand2D {
            width: ColDemand::exact(est.extent_x()),
            height: RowDemand::exact(est.extent_y()),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        // Apply initial background style to whole window
        window.clear();

        let mut c = Cursor::new(&mut window);
        self.draw_with_cursor(&mut c);
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
                    let evt = TuiEvent(messages.message(id), window.get_width());
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
                    write!(&mut c, message_fetch_symbol!()).unwrap();
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
        let msg_id = match messages.walk_from_newest() {
            EventWalkResultNewest::Message(m) => m,
            EventWalkResultNewest::End => return,
            EventWalkResultNewest::RequiresFetch(latest) => {
                self.2
                    .set_message_query(room.id.clone(), MessageQuery::Newest);

                let split = (window.get_height() - 1).from_origin();
                let (above, mut below) = match window.split(split) {
                    Ok((above, below)) => (Some(above), below),
                    Err(below) => (None, below),
                };
                let mut c = Cursor::new(&mut below);
                write!(&mut c, message_fetch_symbol!()).unwrap();

                if let Some(above) = above {
                    window = above;
                } else {
                    return;
                }
                if let Some(latest) = latest {
                    latest
                } else {
                    return;
                }
            }
        };
        self.draw_up_from(
            window,
            hints,
            EventWalkResult::Message(msg_id),
            room,
            messages,
        );
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
                    collected_height += TuiEvent(messages.message(id), window.get_width())
                        .space_demand()
                        .height
                        .min;
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
                    let evt = TuiEvent(messages.message(id), window.get_width());
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
                    write!(&mut c, message_fetch_symbol!()).unwrap();
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
                write!(&mut c, message_fetch_symbol!()).unwrap();
                let query = match &room.current_message {
                    MessageSelection::Newest => MessageQuery::Newest,
                    MessageSelection::Specific(_id) => MessageQuery::BeforeCache,
                };
                self.2.set_message_query(room.id.clone(), query);
            }
        }
    }
}

enum Mode {
    LineInsert,
    Normal,
    RoomFilter(LineEdit),
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
    mode: Mode,
}

impl TuiState {
    fn send_current_message(&mut self) -> Option<Task> {
        if let Some(room) = &self.current_room {
            let msg = self.msg_edit.get().to_owned();
            if !msg.is_empty() {
                self.msg_edit.clear().unwrap();
                return Some(Task::Send(room.id.clone(), msg));
            }
        }
        None
    }
}

fn tui<'a>(state: &'a State, tui_state: &'a TuiState, tasks: Tasks<'a>) -> impl Widget + 'a {
    HLayout::new()
        .separator(GraphemeCluster::try_from('|').unwrap())
        .widget_weighted(Rooms(state, tui_state), 0.0)
        .widget_weighted(
            VLayout::new()
                .separator(GraphemeCluster::try_from('-').unwrap())
                .widget(Messages(state, tui_state, tasks))
                .widget(tui_state.msg_edit.as_widget().with_hints(move |h| {
                    h.active(h.active && matches!(tui_state.mode, Mode::LineInsert))
                })),
            1.0,
        )
}

#[derive(Debug)]
pub enum Event {
    Update,
    Input(Input),
    Signal(signal::Signal),
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
            mode: Mode::Normal,
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

        let mut first = true;
        loop {
            let event: Option<Event> = if first {
                first = false;
                events.recv().await
            } else {
                if let Ok(event) = tokio::time::timeout_at(
                    tokio::time::Instant::now() + DRAW_TIMEOUT,
                    events.recv(),
                )
                .await
                {
                    event
                } else {
                    break;
                }
            };
            match event.unwrap() {
                Event::Update => {}
                Event::Signal(signal::Signal::SIGWINCH) => { /* Just redraw the window */ }
                Event::Signal(signal::Signal::SIGTSTP) => {
                    if let Err(e) = term.handle_sigtstp() {
                        tracing::warn!("Unable to handle SIGTSTP: {}", e);
                    }
                }
                Event::Signal(signal::Signal::SIGTERM) => run = false,
                Event::Signal(s) => {
                    tracing::warn!("Unhandled signal {}", s);
                }
                Event::Input(input) => {
                    let sig_behavior = unsegen_signals::SignalBehavior::new()
                        .on_default::<unsegen_signals::SIGTSTP>();
                    let input = input.chain(sig_behavior);

                    let input = input.chain((Key::Esc, || tui_state.mode = Mode::Normal));

                    let mut state = state.lock().await;
                    match &mut tui_state.mode {
                        Mode::Normal => input
                            .chain((Key::Char('q'), || run = false))
                            .chain((Key::Char('i'), || tui_state.mode = Mode::LineInsert))
                            .chain((Key::Char('o'), || {
                                tui_state.mode = Mode::RoomFilter(LineEdit::new())
                            }))
                            .chain(
                                ScrollBehavior::new(&mut RoomsMut(&mut state, &mut tui_state))
                                    .forwards_on(Key::Char('n'))
                                    .backwards_on(Key::Char('p')),
                            )
                            .chain(
                                ScrollBehavior::new(&mut MessagesMut(&state, &mut tui_state))
                                    .forwards_on(Key::Char('j'))
                                    .backwards_on(Key::Char('k'))
                                    .to_end_on(Key::Ctrl('g')),
                            )
                            .chain((Key::Char('\n'), || {
                                tui_state.send_current_message().map(|t| tasks.add_task(t));
                            })),
                        Mode::LineInsert => input
                            .chain(
                                EditBehavior::new(&mut tui_state.msg_edit)
                                    .delete_forwards_on(Key::Delete)
                                    .delete_backwards_on(Key::Backspace)
                                    .clear_on(Key::Ctrl('c')),
                            )
                            .chain((Key::Char('\n'), || {
                                tui_state.send_current_message().map(|t| tasks.add_task(t));
                            })),
                        Mode::RoomFilter(lineedit) => input
                            .chain(
                                EditBehavior::new(lineedit)
                                    .delete_forwards_on(Key::Delete)
                                    .delete_backwards_on(Key::Backspace),
                            )
                            .chain(
                                ScrollBehavior::new(&mut RoomsMut(&mut state, &mut tui_state))
                                    .forwards_on(Key::Ctrl('n'))
                                    .backwards_on(Key::Ctrl('p')),
                            )
                            .chain((Key::Char('\n'), || {
                                let mut r = RoomsMut(&mut state, &mut tui_state);
                                if !r.as_rooms().active_contains_current() {
                                    let _ = r.scroll_forwards(); // Select first
                                }
                                tui_state.mode = Mode::Normal;
                            })),
                    };
                }
            }
        }
    }
}
