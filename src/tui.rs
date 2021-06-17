use std::io::stdout;
use std::sync::Arc;
use tokio::sync::{
    mpsc::{Receiver, Sender},
    Mutex,
};
use unsegen::base::*;
use unsegen::input::{
    EditBehavior, Editable, Input, Key, OperationResult, ScrollBehavior, Scrollable,
};
use unsegen::widget::builtin::*;
use unsegen::widget::*;

use matrix_sdk::events::room::message::MessageType;
use matrix_sdk::identifiers::RoomId;
use matrix_sdk::MilliSecondsSinceUnixEpoch;

use crate::State;

struct Rooms<'a>(&'a State, &'a TuiState);

impl Widget for Rooms<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let w = self
            .0
            .rooms
            .values()
            .map(|s| text_width(s))
            .max()
            .unwrap_or(PositiveAxisDiff::new_unchecked(0));
        let h = self.0.rooms.len();
        Demand2D {
            width: ColDemand::exact(w),
            height: RowDemand::exact(h),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        let mut c = Cursor::new(&mut window);
        for (id, room) in self.0.rooms.iter() {
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
            let mut it = self.0.rooms.range(..current.id.clone()).rev();
            Some(
                it.next()
                    .map(|(k, _)| RoomState::at_last_message(k))
                    .unwrap_or(current),
            )
        } else {
            self.0
                .rooms
                .keys()
                .rev()
                .next()
                .map(RoomState::at_last_message)
        };
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        self.1.current_room = if let Some(current) = self.1.current_room.take() {
            let mut it = self.0.rooms.range(current.id.clone()..);
            it.next();
            Some(
                it.next()
                    .map(|(k, _v)| RoomState::at_last_message(k))
                    .unwrap_or(current),
            )
        } else {
            self.0.rooms.keys().next().map(RoomState::at_last_message)
        };
        Ok(())
    }
}

struct Messages<'a>(&'a State, &'a TuiState);

impl Widget for Messages<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        Demand2D {
            width: ColDemand::at_least(Width::new(0).unwrap()),
            height: RowDemand::at_least(Height::new(0).unwrap()),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        use std::fmt::Write;
        let mut c = Cursor::new(&mut window);
        if let Some(room) = self.1.current_room.as_ref() {
            if let Some(messages) = self.0.messages.get(&room.id) {
                let msgs = if let Some(current) = room.current_message {
                    messages.range(&current..)
                } else {
                    messages.range(..)
                };
                for (_ts, msg) in msgs {
                    match &msg.content.msgtype {
                        MessageType::Text(text) => {
                            writeln!(&mut c, "{}: {}", msg.sender, text.body).unwrap();
                        }
                        o => {
                            writeln!(&mut c, "{}: Other message {:?}", msg.sender, o).unwrap();
                        }
                    }
                }
            }
        }
    }
}

struct RoomState {
    id: RoomId,
    current_message: Option<MilliSecondsSinceUnixEpoch>,
}

impl RoomState {
    fn at_last_message(id: &RoomId) -> Self {
        RoomState {
            id: id.clone(),
            current_message: None,
        }
    }
}

struct TuiState {
    msg_edit: PromptLine,
    current_room: Option<RoomState>,
}

fn tui<'a>(state: &'a State, tui_state: &'a TuiState) -> impl Widget + 'a {
    HLayout::new()
        .separator(GraphemeCluster::try_from('|').unwrap())
        .widget_weighted(Rooms(state, tui_state), 0.0)
        .widget_weighted(
            VLayout::new()
                .separator(GraphemeCluster::try_from('-').unwrap())
                .widget(Messages(state, tui_state))
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
    MoreMessages(RoomId),
}

pub async fn run_tui(
    mut events: Receiver<Event>,
    task_sink: Sender<Task>,
    state: Arc<Mutex<State>>,
) {
    let stdout = stdout();
    let mut term = Terminal::new(stdout.lock()).unwrap();
    let mut tui_state = {
        let state = state.lock().await;

        let current_room = if let Some(id) = state.rooms.keys().next() {
            Some(RoomState {
                id: id.clone(),
                current_message: state
                    .messages
                    .get(id)
                    .and_then(|msgs| msgs.keys().rev().next().cloned()),
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
    while run {
        let mut tasks = Vec::new();

        {
            let state = state.lock().await;
            let win = term.create_root_window();
            tui(&state, &tui_state).draw(win, RenderingHints::new().active(true));

            let messages_to_request: usize = 100;
            // request new messages if (potentially) necessary
            if let Some(room) = &tui_state.current_room {
                // before current
                if let Some(msgs) = state.messages.get(&room.id) {
                    let msgs = room
                        .current_message
                        .map(|m| msgs.range(..&m).rev())
                        .unwrap_or(msgs.range(..).rev());

                    let available = msgs.count();
                    if available < messages_to_request {
                        //tasks.push(Messages
                    }
                } else {
                    tasks.push(Task::MoreMessages(room.id.clone()));
                    //todo!()
                }
            }
        }
        term.present();

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
                            tasks.push(Task::Send(
                                room.id.clone(),
                                tui_state.msg_edit.get().to_owned(),
                            ));
                            tui_state.msg_edit.clear().unwrap();
                        }
                    }));
            }
        }

        // TODO: somehow we need to make sure that this does not block. at the moment it still
        // might do so because the channel has 5 elements.
        for t in tasks {
            task_sink.send(t).await.unwrap();
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

pub fn start_keyboard_thread(sink: Sender<Event>) {
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
