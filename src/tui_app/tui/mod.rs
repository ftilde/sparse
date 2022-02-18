use matrix_sdk::Client;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::io::stdout;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use unsegen::base::*;
use unsegen::input::{
    EditBehavior, Editable, Input, Key, OperationResult, ScrollBehavior, Scrollable,
};
use unsegen::widget::builtin::*;
use unsegen::widget::*;

use matrix_sdk::ruma::{
    events::{room::message::RoomMessageEventContent, SyncMessageEvent},
    identifiers::{EventId, RoomId},
};

use crate::config::{Config, KeyMapFunctionResult, Keys};
use crate::timeline::MessageQuery;
use crate::tui_app::tui::actions::CommandEnvironment;
use crate::tui_app::State;

use nix::sys::signal;

pub mod actions;
pub mod messages;
pub mod rooms;

const DRAW_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(16);

#[derive(Copy, Clone)]
pub struct Tasks<'a> {
    message_query: &'a RefCell<Option<MessageQueryRequest>>,
}

impl Tasks<'_> {
    fn set_message_query(&self, room: Box<RoomId>, query: MessageQuery) {
        let mut q = self.message_query.borrow_mut();
        *q = Some(MessageQueryRequest { room, kind: query });
    }
}

pub enum MessageSelection {
    Newest,
    Specific(Box<EventId>),
}

struct RoomState {
    id: Box<RoomId>,
    pub msg_edit: TextEdit,
    msg_edit_type: SendMessageType,
    selection: MessageSelection,
}

impl RoomState {
    fn at_last_message(id: Box<RoomId>) -> Self {
        RoomState {
            id,
            msg_edit: TextEdit::new(),
            msg_edit_type: SendMessageType::Simple,
            selection: MessageSelection::Newest,
        }
    }
    pub fn send_current_message(&mut self, c: &Client) -> OperationResult {
        let msg = self.msg_edit.get().to_owned();
        if !msg.is_empty() {
            self.msg_edit.clear().unwrap();
            let mut tmp_type = SendMessageType::Simple;
            std::mem::swap(&mut tmp_type, &mut self.msg_edit_type);
            if let Some(room) = c.get_joined_room(&self.id) {
                tokio::spawn(async move {
                    let content = match tmp_type {
                        SendMessageType::Simple => RoomMessageEventContent::text_plain(msg),
                        SendMessageType::Reply(orig_msg) => {
                            RoomMessageEventContent::text_reply_plain(msg, &orig_msg)
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
            Ok(())
        } else {
            Err(())
        }
    }
}
fn send_read_receipt(c: &Client, rid: &RoomId, eid: Box<EventId>) {
    if let Some(room) = c.get_joined_room(rid) {
        tokio::spawn(async move {
            room.read_receipt(&eid).await.unwrap();
        });
    } else {
        tracing::error!("can't send read receipt, no joined room");
    }
}

#[derive(Default)]
pub struct RoomSelectionHistory {
    selections: Vec<Box<RoomId>>, // Ordered from least to most recent access via `select`
    current: usize,
}

impl RoomSelectionHistory {
    fn current(&self) -> Option<&RoomId> {
        self.selections.get(self.current).map(|c| &**c)
    }

    fn deselect(&mut self) {
        self.current = self.selections.len();
    }
    fn select(&mut self, id: &RoomId) {
        let index = self
            .selections
            .iter()
            .enumerate()
            .find(|(_, c)| *c == id)
            .map(|(i, _)| i);
        let new_newest = if let Some(index) = index {
            self.selections.remove(index)
        } else {
            id.to_owned()
        };

        self.selections.push(new_newest);
        self.current = self.selections.len() - 1;
    }
}

impl Scrollable for RoomSelectionHistory {
    fn scroll_backwards(&mut self) -> OperationResult {
        self.current = self.current.checked_sub(1).ok_or(())?;
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        let new = self.current + 1;
        if new < self.selections.len() {
            self.current = new;
            Ok(())
        } else {
            Err(())
        }
    }
}

pub struct TuiState {
    room_selection: RoomSelectionHistory,
    rooms: BTreeMap<Box<RoomId>, RoomState>,
    mode: Mode,
    room_filter_line: LineEdit,
    command_line: PromptLine,
    previous_keys: Keys,
    last_error_message: Option<String>,
}

fn key_action_behavior<'a>(
    c: &'a mut actions::CommandContext<'a>,
) -> impl unsegen::input::Behavior + 'a {
    move |input: Input| -> Option<Input> {
        let mut new_keys = Keys(Vec::new());
        if let unsegen::input::Event::Key(k) = input.event {
            c.tui_state.previous_keys.0.push(k);
        } else {
            return Some(input);
        }
        std::mem::swap(&mut new_keys, &mut c.tui_state.previous_keys);
        match c.config.keymaps.find_action(&c.tui_state.mode, &new_keys) {
            KeyMapFunctionResult::IsPrefix(_) => {
                c.tui_state.previous_keys = new_keys;
                None
            }
            KeyMapFunctionResult::Found(action) => {
                use crate::tui_app::tui::actions::ActionResult;
                match c.command_environment.run_action(action, c) {
                    Ok(ActionResult::Ok | ActionResult::Noop) => {}
                    Ok(ActionResult::Error(e)) => {
                        c.tui_state.last_error_message = Some(e);
                    }
                    Err(e) => {
                        c.tui_state.last_error_message = Some(format!("{}", e));
                    }
                }
                None
            }
            KeyMapFunctionResult::NotFound | KeyMapFunctionResult::FoundPrefix(_) => Some(input),
        }
    }
}

impl TuiState {
    fn new(current_room: Option<&RoomId>) -> Self {
        let mut s = TuiState {
            rooms: BTreeMap::new(),
            room_selection: RoomSelectionHistory::default(),
            mode: Mode::default(),
            room_filter_line: LineEdit::new(),
            command_line: PromptLine::with_prompt(":".to_owned()),
            previous_keys: Keys(Vec::new()),
            last_error_message: None,
        };
        s.set_current_room(current_room);
        s
    }
    fn enter_mode(&mut self, mode: Mode) -> OperationResult {
        let new = mode.builtin_mode();
        let previous = self.mode.builtin_mode();
        if !matches!(new, BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread)
            && matches!(
                previous,
                BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread
            )
        {
            let _ = self.room_filter_line.clear();
        }
        if !matches!(new, BuiltinMode::Command) && matches!(previous, BuiltinMode::Command) {
            let _ = self.command_line.clear();
        }
        if self.mode != mode {
            self.mode = mode;
            Ok(())
        } else {
            Err(())
        }
    }
    fn set_current_room(&mut self, id: Option<&RoomId>) {
        if let Some(id) = id {
            if !self.rooms.contains_key(id) {
                self.rooms
                    .insert(id.to_owned(), RoomState::at_last_message(id.to_owned()));
            }
            self.room_selection.select(id);
        } else {
            self.room_selection.deselect();
        }
    }
    fn current_room_state(&self) -> Option<&RoomState> {
        self.room_selection
            .current()
            .map(|r| self.rooms.get(r).unwrap())
    }
    fn current_room_state_mut(&mut self) -> Option<&mut RoomState> {
        if let Some(id) = self.room_selection.current() {
            Some(self.rooms.get_mut(id).unwrap())
        } else {
            None
        }
    }
}

struct Foo<F: Fn(Window, RenderingHints)>(ColDemand, RowDemand, F);

impl<F: Fn(Window, RenderingHints)> Widget for Foo<F> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        unsegen::widget::Demand2D {
            width: self.0,
            height: self.1,
        }
    }

    fn draw(&self, window: Window, hints: RenderingHints) {
        (self.2)(window, hints)
    }
}

fn msg_edit<'a>(
    tui_room_state: &'a RoomState,
    room_state: &'a crate::tui_app::RoomState,
    potentially_active: bool,
) -> impl Widget + 'a {
    let mut layout = VLayout::new();
    if let SendMessageType::Reply(orig) = &tui_room_state.msg_edit_type {
        layout = layout.widget(Foo(
            ColDemand::at_least(1),
            RowDemand::exact(1),
            move |mut w, _| messages::draw_event_preview(orig, room_state, &mut w),
        ));
    }
    layout.widget(
        HLayout::new().widget("> ").widget(
            tui_room_state
                .msg_edit
                .as_widget()
                .cursor_blink_on(StyleModifier::new().underline(true))
                .cursor_inactive(StyleModifier::new().invert(BoolModifyMode::Toggle))
                .with_hints(move |h| h.active(h.active && potentially_active)),
        ),
    )
}

fn bottom_bar<'a>(tui_state: &'a TuiState) -> impl Widget + 'a {
    let spacer = " ".with_demand(|_| Demand2D {
        width: ColDemand::at_least(0),
        height: RowDemand::exact(1),
    });
    let mut hlayout = HLayout::new().separator(GraphemeCluster::try_from(' ').unwrap());

    if let Some(msg) = &tui_state.last_error_message {
        hlayout = hlayout.widget(msg)
    } else if matches!(tui_state.mode.builtin_mode(), BuiltinMode::Command) {
        hlayout = hlayout.widget(tui_state.command_line.as_widget())
    }

    hlayout = hlayout
        .widget(spacer)
        .widget(tui_state.mode.to_string())
        .widget(format!("{}", tui_state.previous_keys));
    hlayout
}

fn tui<'a>(state: &'a State, tui_state: &'a TuiState, tasks: Tasks<'a>) -> impl Widget + 'a {
    let mut hlayout = HLayout::new()
        .separator(GraphemeCluster::try_from('│').unwrap())
        .widget_weighted(rooms::Rooms(state, tui_state).as_widget(), 0.25);
    if let Some(tui_room) = tui_state.current_room_state() {
        if let Some(room) = state.rooms.get(&tui_room.id) {
            hlayout = hlayout.widget_weighted(
                VLayout::new()
                    .widget(messages::Messages(state, tui_state, tasks))
                    .widget(msg_edit(
                        tui_room,
                        room,
                        matches!(tui_state.mode.builtin_mode(), BuiltinMode::Insert),
                    )),
                0.75,
            )
        }
    }
    VLayout::new().widget(hlayout).widget(bottom_bar(tui_state))
}

#[derive(Debug)]
pub enum Event {
    Update,
    Input(Input),
    Signal(signal::Signal),
    Bell,
}

#[derive(Debug)]
pub enum SendMessageType {
    Simple,
    Reply(SyncMessageEvent<RoomMessageEventContent>),
}

#[derive(Clone)]
pub struct MessageQueryRequest {
    pub room: Box<RoomId>,
    pub kind: MessageQuery,
}

pub async fn run_tui(
    mut events: mpsc::Receiver<Event>,
    message_query_sink: watch::Sender<Option<MessageQueryRequest>>,
    state: Arc<Mutex<State>>,
    client: Client,
    command_environment: CommandEnvironment,
    config: Config,
) {
    let stdout = stdout();
    let mut term = Terminal::new(stdout.lock()).unwrap();
    let mut tui_state = {
        let state = state.lock().await;
        TuiState::new(state.rooms.keys().next().map(|k| &**k))
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
                Event::Bell => term.emit_bell(),
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

                    let mut state = state.lock().await;

                    let mut c = actions::CommandContext {
                        tui_state: &mut tui_state,
                        state: &mut state,
                        client: &client,
                        tasks,
                        continue_running: &mut run,
                        config: &config,
                        command_environment: &command_environment,
                    };
                    let input = input.chain(key_action_behavior(&mut c));
                    match tui_state.mode.builtin_mode() {
                        BuiltinMode::Normal => {}
                        BuiltinMode::Insert => {
                            if let Some(room) = tui_state.current_room_state_mut() {
                                input.chain(EditBehavior::new(&mut room.msg_edit));
                            }
                        }
                        BuiltinMode::Command => {
                            input
                                .chain(
                                    EditBehavior::new(&mut tui_state.command_line)
                                        .delete_forwards_on(Key::Delete)
                                        .delete_backwards_on(Key::Backspace)
                                        .left_on(Key::Left)
                                        .right_on(Key::Right),
                                )
                                .chain(
                                    ScrollBehavior::new(&mut tui_state.command_line)
                                        .backwards_on(Key::Up)
                                        .forwards_on(Key::Down),
                                );
                        }
                        BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread => {
                            input.chain(
                                EditBehavior::new(&mut tui_state.room_filter_line)
                                    .delete_forwards_on(Key::Delete)
                                    .delete_backwards_on(Key::Backspace),
                            );
                        }
                    };

                    if let Some(id) = &tui_state.room_selection.current() {
                        if let Some(room) = state.rooms.get_mut(*id) {
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

#[derive(Clone, Hash, PartialEq, Eq)]
pub enum Mode {
    Builtin(BuiltinMode),
    Custom(String, BuiltinMode),
}

impl Mode {
    fn builtin_mode(&self) -> BuiltinMode {
        match self {
            Mode::Builtin(m) => *m,
            Mode::Custom(_, m) => *m,
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Builtin(m) => write!(f, "{}", m),
            Mode::Custom(s, _) => write!(f, "{}", s),
        }
    }
}

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub enum BuiltinMode {
    Normal,
    Insert,
    Command,
    RoomFilter,
    RoomFilterUnread,
}

impl std::default::Default for Mode {
    fn default() -> Self {
        Mode::Builtin(BuiltinMode::Normal)
    }
}

impl FromStr for BuiltinMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "normal" => BuiltinMode::Normal,
            "insert" => BuiltinMode::Insert,
            "command" => BuiltinMode::Command,
            "roomfilter" => BuiltinMode::RoomFilter,
            "roomfilterunread" => BuiltinMode::RoomFilterUnread,
            _ => return Err(()),
        })
    }
}

impl std::fmt::Display for BuiltinMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            BuiltinMode::Normal => "normal",
            BuiltinMode::Insert => "insert",
            BuiltinMode::Command => "command",
            BuiltinMode::RoomFilter => "roomfilter",
            BuiltinMode::RoomFilterUnread => "roomfilterunread",
        };
        write!(f, "{}", s)
    }
}

#[derive(Clone)]
pub struct ModeSet {
    custom: HashMap<String, BuiltinMode>,
}

impl ModeSet {
    pub fn new() -> Self {
        ModeSet {
            custom: HashMap::new(),
        }
    }
    pub fn define(&mut self, name: String, base: BuiltinMode) -> Result<(), ()> {
        if self.get(&name).is_none() {
            self.custom.insert(name, base);
            Ok(())
        } else {
            Err(())
        }
    }
    pub fn get(&self, name: &str) -> Option<Mode> {
        if let Ok(m) = BuiltinMode::from_str(name) {
            Some(Mode::Builtin(m))
        } else if let Some(m) = self.custom.get(name) {
            Some(Mode::Custom(name.to_string(), *m))
        } else {
            None
        }
    }
}
