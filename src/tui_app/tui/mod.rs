use matrix_sdk::Client;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::stdout;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use unsegen::base::*;
use unsegen::input::{EditBehavior, Editable, Input, Key, ScrollBehavior, Scrollable};
use unsegen::widget::builtin::*;
use unsegen::widget::*;

use matrix_sdk::ruma::{
    events::{
        room::message::{MessageEventContent, MessageType},
        AnySyncMessageEvent, SyncMessageEvent,
    },
    identifiers::{EventId, RoomId},
};

use crate::timeline::MessageQuery;
use crate::tui_app::State;

use nix::sys::signal;

mod messages;
mod rooms;

const DRAW_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(16);

#[derive(Copy, Clone)]
pub struct Tasks<'a> {
    message_query: &'a RefCell<Option<MessageQueryRequest>>,
}

impl Tasks<'_> {
    fn set_message_query(&self, room: RoomId, query: MessageQuery) {
        let mut q = self.message_query.borrow_mut();
        *q = Some(MessageQueryRequest { room, kind: query });
    }
}
enum Mode {
    LineInsert,
    Normal,
    RoomFilter(LineEdit),
    RoomFilterUnread(LineEdit),
}

impl Mode {
    fn room_filter_string(&self) -> &str {
        match self {
            Mode::RoomFilter(l) | Mode::RoomFilterUnread(l) => l.get(),
            _ => "",
        }
    }
}

pub enum MessageSelection {
    Newest,
    Specific(EventId),
}

struct RoomState {
    id: RoomId,
    msg_edit: PromptLine,
    msg_edit_type: SendMessageType,
    selection: MessageSelection,
}

impl RoomState {
    fn at_last_message(id: RoomId) -> Self {
        RoomState {
            id,
            msg_edit: PromptLine::with_prompt(" > ".to_owned()),
            msg_edit_type: SendMessageType::Simple,
            selection: MessageSelection::Newest,
        }
    }
    fn send_current_message(&mut self, c: &Client) {
        let msg = self.msg_edit.get().to_owned();
        if !msg.is_empty() {
            self.msg_edit.clear().unwrap();
            let mut tmp_type = SendMessageType::Simple;
            std::mem::swap(&mut tmp_type, &mut self.msg_edit_type);
            if let Some(room) = c.get_joined_room(&self.id) {
                let id = self.id.clone();
                tokio::spawn(async move {
                    let content = match tmp_type {
                        SendMessageType::Simple => MessageEventContent::text_plain(msg),
                        SendMessageType::Reply(orig_msg) => {
                            let m = orig_msg.into_full_event(id);
                            MessageEventContent::text_reply_plain(msg, &m)
                        }
                    };
                    room.send(
                        matrix_sdk::ruma::events::AnyMessageEventContent::RoomMessage(content),
                        None,
                    )
                    .await
                    .unwrap();
                });
            } else {
                tracing::error!("can't send message, no joined room");
            }
        }
    }
}
fn send_read_receipt(c: &Client, rid: RoomId, eid: EventId) {
    if let Some(room) = c.get_joined_room(&rid) {
        tokio::spawn(async move {
            room.read_receipt(&eid).await.unwrap();
        });
    } else {
        tracing::error!("can't send read receipt, no joined room");
    }
}

pub struct TuiState {
    current_room: Option<RoomId>,
    rooms: BTreeMap<RoomId, RoomState>,
    mode: Mode,
}

impl TuiState {
    fn new(current_room: Option<RoomId>) -> Self {
        let mut s = TuiState {
            rooms: BTreeMap::new(),
            current_room: None,
            mode: Mode::Normal,
        };
        s.set_current_room(current_room);
        s
    }
    fn set_current_room(&mut self, id: Option<RoomId>) {
        if let Some(id) = &id {
            if !self.rooms.contains_key(id) {
                self.rooms
                    .insert(id.clone(), RoomState::at_last_message(id.clone()));
            }
        }
        self.current_room = id;
    }
    fn current_room_state(&self) -> Option<&RoomState> {
        self.current_room
            .as_ref()
            .map(|r| self.rooms.get(r).unwrap())
    }
    fn current_room_state_mut(&mut self) -> Option<&mut RoomState> {
        if let Some(id) = self.current_room.as_ref() {
            Some(self.rooms.get_mut(id).unwrap())
        } else {
            None
        }
    }
}

fn msg_edit<'a>(room_state: &'a RoomState, potentially_active: bool) -> impl Widget + 'a {
    let mut layout = VLayout::new();
    if let SendMessageType::Reply(orig) = &room_state.msg_edit_type {
        let c = match &orig.content.msgtype {
            MessageType::Text(t) => t.body.clone(),
            o => format!("{:?}", o),
        };
        layout = layout.widget(format!("-> {}: {:?}", orig.sender, c));
    }
    layout.widget(
        room_state
            .msg_edit
            .as_widget()
            .with_hints(move |h| h.active(h.active && potentially_active)),
    )
}

fn tui<'a>(state: &'a State, tui_state: &'a TuiState, tasks: Tasks<'a>) -> impl Widget + 'a {
    let mut layout = HLayout::new()
        .separator(GraphemeCluster::try_from('│').unwrap())
        .widget_weighted(rooms::Rooms(state, tui_state).as_widget(), 0.25);
    if let Some(current) = tui_state.current_room_state() {
        layout = layout.widget_weighted(
            VLayout::new()
                .widget(messages::Messages(state, tui_state, tasks))
                .widget(msg_edit(
                    current,
                    matches!(tui_state.mode, Mode::LineInsert),
                )),
            0.75,
        )
    }
    layout
}

#[derive(Debug)]
pub enum Event {
    Update,
    Input(Input),
    Signal(signal::Signal),
}

#[derive(Debug)]
pub enum SendMessageType {
    Simple,
    Reply(SyncMessageEvent<MessageEventContent>),
}

#[derive(Clone)]
pub struct MessageQueryRequest {
    pub room: RoomId,
    pub kind: MessageQuery,
}

pub async fn run_tui(
    mut events: mpsc::Receiver<Event>,
    message_query_sink: watch::Sender<Option<MessageQueryRequest>>,
    state: Arc<Mutex<State>>,
    client: Client,
) {
    let stdout = stdout();
    let mut term = Terminal::new(stdout.lock()).unwrap();
    let mut tui_state = {
        let state = state.lock().await;
        TuiState::new(state.rooms.keys().next().cloned())
    };

    let mut run = true;

    let message_query = RefCell::new(None);

    let tasks = Tasks {
        message_query: &message_query,
    };
    while run {
        {
            let state = state.lock().await;
            let win = term.create_root_window();
            tui(&state, &tui_state, tasks).draw(win, RenderingHints::new().active(true));
        }
        term.present();

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

                    let input = input.chain((Key::Esc, || {
                        if let Mode::Normal = tui_state.mode {
                            tui_state
                                .current_room_state_mut()
                                .map(|tui_room| tui_room.selection = MessageSelection::Newest);
                        } else {
                            tui_state.mode = Mode::Normal
                        }
                    }));

                    let mut state = state.lock().await;
                    match &mut tui_state.mode {
                        Mode::Normal => input
                            .chain((Key::Char('q'), || run = false))
                            .chain((Key::Char('i'), || tui_state.mode = Mode::LineInsert))
                            .chain((Key::Char('o'), || {
                                tui_state.mode = Mode::RoomFilter(LineEdit::new())
                            }))
                            .chain((Key::Char('O'), || {
                                tui_state.mode = Mode::RoomFilterUnread(LineEdit::new())
                            }))
                            .chain((Key::Char('r'), || {
                                if let Some(id) = &tui_state.current_room {
                                    if let Some(room) = state.rooms.get(id) {
                                        let tui_room = tui_state.current_room_state_mut().unwrap();
                                        if let MessageSelection::Specific(eid) = &tui_room.selection
                                        {
                                            if let Some(crate::timeline::Event::Message(
                                                AnySyncMessageEvent::RoomMessage(msg),
                                            )) = room.messages.message_from_id(eid)
                                            {
                                                tui_room.msg_edit_type =
                                                    SendMessageType::Reply(msg.clone());
                                                tui_room.selection = MessageSelection::Newest;
                                            }
                                        }
                                    }
                                }
                                tui_state.mode = Mode::LineInsert;
                            }))
                            .chain(
                                ScrollBehavior::new(&mut rooms::RoomsMut(
                                    &mut state,
                                    &mut tui_state,
                                ))
                                .forwards_on(Key::Char('n'))
                                .backwards_on(Key::Char('p')),
                            )
                            .chain(
                                ScrollBehavior::new(&mut messages::MessagesMut(
                                    &state,
                                    &mut tui_state,
                                ))
                                .forwards_on(Key::Char('j'))
                                .backwards_on(Key::Char('k'))
                                .to_end_on(Key::Ctrl('g')),
                            )
                            .chain((Key::Char('\n'), || {
                                tui_state
                                    .current_room_state_mut()
                                    .map(|r| r.send_current_message(&client));
                            })),
                        Mode::LineInsert => {
                            if let Some(room) = tui_state.current_room_state_mut() {
                                input
                                    .chain(
                                        EditBehavior::new(&mut room.msg_edit)
                                            .delete_forwards_on(Key::Delete)
                                            .delete_backwards_on(Key::Backspace)
                                            .clear_on(Key::Ctrl('c')),
                                    )
                                    .chain((Key::Char('\n'), || {
                                        room.send_current_message(&client);
                                    }))
                            } else {
                                input
                            }
                        }
                        Mode::RoomFilter(lineedit) | Mode::RoomFilterUnread(lineedit) => input
                            .chain(
                                EditBehavior::new(lineedit)
                                    .delete_forwards_on(Key::Delete)
                                    .delete_backwards_on(Key::Backspace),
                            )
                            .chain(
                                ScrollBehavior::new(&mut rooms::RoomsMut(
                                    &mut state,
                                    &mut tui_state,
                                ))
                                .forwards_on(Key::Ctrl('n'))
                                .backwards_on(Key::Ctrl('p')),
                            )
                            .chain((Key::Char('\n'), || {
                                let mut r = rooms::RoomsMut(&mut state, &mut tui_state);
                                if !r.as_rooms().active_contains_current() {
                                    let _ = r.scroll_forwards(); // Select first
                                }
                                tui_state.mode = Mode::Normal;
                            })),
                    };

                    if let Some(id) = &tui_state.current_room {
                        if let Some(room) = state.rooms.get_mut(id) {
                            if let Some(read_event_id) = room.mark_newest_event_as_read() {
                                send_read_receipt(&client, id.clone(), read_event_id);
                            }
                        }
                    }
                }
            }
        }
    }
}
