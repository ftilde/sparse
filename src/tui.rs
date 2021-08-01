use std::cell::RefCell;
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

use crate::timeline::{EventWalkResult, EventWalkResultNewest, MessageQuery};
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
    fn add_message_query(&self, room: RoomId, query: MessageQuery) {
        let mut q = self.message_query.borrow_mut();
        *q = Some(MessageQueryRequest { room, kind: query });
    }

    fn add_task(&self, task: Task) {
        let mut t = self.tasks.borrow_mut();
        t.push(task);
    }
}

struct Messages<'a>(&'a State, &'a TuiState, Tasks<'a>);

fn format_event(e: &crate::timeline::Event) -> String {
    match e {
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

impl Widget for Messages<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        Demand2D {
            width: ColDemand::at_least(Width::new(0).unwrap()),
            height: RowDemand::at_least(Height::new(0).unwrap()),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        use std::fmt::Write;
        let height = window.get_height();
        let mut c = Cursor::new(&mut window);
        c.move_to_y((height - 1).from_origin());

        tracing::warn!("1");
        if let Some(room) = self.1.current_room.as_ref() {
            tracing::warn!("2");
            if let Some(messages) = self.0.messages().get(&room.id) {
                let mut msg = match &room.current_message {
                    MessageSelection::Newest => match messages.walk_from_newest() {
                        EventWalkResultNewest::Message(m) => EventWalkResult::Message(m),
                        EventWalkResultNewest::End => EventWalkResult::End,
                        EventWalkResultNewest::RequiresFetch => {
                            tracing::warn!("fetch newest");
                            write!(&mut c, "[...]").unwrap();
                            self.2
                                .add_message_query(room.id.clone(), MessageQuery::Newest);
                            EventWalkResult::End
                        }
                    },
                    MessageSelection::Specific(id) => messages.walk_from_known(&id),
                };

                tracing::warn!("start loop");
                loop {
                    tracing::warn!("msg={:?}", msg);
                    msg = match msg {
                        EventWalkResult::Message(id) => {
                            let msg = messages.message(id);
                            let (_, row) = c.get_position();
                            if row < 0 {
                                break;
                            }
                            let text = format_event(&msg);
                            //TODO: what about line wrapping due to small window size?
                            let wraps = text.chars().filter(|c| *c == '\n').count() as i32;
                            c.move_to_y(row - wraps);
                            c.write(&text);
                            c.move_to(AxisIndex::new(0), row - wraps - 1);
                            messages.previous(id)
                        }
                        EventWalkResult::End => {
                            break;
                        }
                        EventWalkResult::RequiresFetchFrom(_tok) => {
                            write!(&mut c, "[...]").unwrap();
                            self.2
                                .add_message_query(room.id.clone(), MessageQuery::BeforeCache);
                            break;
                        }
                    };
                }
                //TODO
                //let msgs = if let Some(current) = room.current_message {
                //    messages.range(&current..)
                //} else {
                //    messages.range(..)
                //};
                //for (_ts, msg) in msgs {
                //}
            } else {
                write!(&mut c, "[...]").unwrap();
                let query = match &room.current_message {
                    MessageSelection::Newest => MessageQuery::Newest,
                    MessageSelection::Specific(_id) => MessageQuery::BeforeCache,
                };
                self.2.add_message_query(room.id.clone(), query);
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
