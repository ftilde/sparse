use matrix_sdk::Client;
use std::cell::RefCell;
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

pub struct RoomTuiState {
    pub msg_edit: TextEdit,
    msg_edit_type: SendMessageType,
    selection: MessageSelection,
}

impl RoomTuiState {
    pub fn at_last_message() -> Self {
        RoomTuiState {
            msg_edit: TextEdit::new(),
            msg_edit_type: SendMessageType::Simple,
            selection: MessageSelection::Newest,
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
    pub fn current(&self) -> Option<&RoomId> {
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
    pub room_selection: RoomSelectionHistory,
    pub event_detail: EventDetail,
    mode_stack: Vec<Mode>, // Invariant: always at least one element
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
            c.state.tui.previous_keys.0.push(k);
        } else {
            return Some(input);
        }
        std::mem::swap(&mut new_keys, &mut c.state.tui.previous_keys);
        match c
            .config
            .keymaps
            .find_action(&c.state.tui.current_mode(), &new_keys)
        {
            KeyMapFunctionResult::IsPrefix(_) => {
                c.state.tui.previous_keys = new_keys;
                None
            }
            KeyMapFunctionResult::Found(action) => {
                use crate::tui_app::tui::actions::ActionResult;
                match c.command_environment.run_action(action, c) {
                    Ok(ActionResult::Ok | ActionResult::Noop) => {}
                    Ok(ActionResult::Error(e)) => {
                        c.state.tui.last_error_message = Some(e);
                    }
                    Err(e) => {
                        c.state.tui.last_error_message = Some(format!("{}", e));
                    }
                }
                None
            }
            KeyMapFunctionResult::NotFound | KeyMapFunctionResult::FoundPrefix(_) => Some(input),
        }
    }
}

impl TuiState {
    pub fn new(current_room: Option<&RoomId>) -> Self {
        let mut s = TuiState {
            room_selection: RoomSelectionHistory::default(),
            event_detail: EventDetail::default(),
            mode_stack: vec![Mode::default()],
            room_filter_line: LineEdit::new(),
            command_line: PromptLine::with_prompt(":".to_owned()),
            previous_keys: Keys(Vec::new()),
            last_error_message: None,
        };
        s.set_current_room(current_room);
        s
    }
    fn current_mode(&self) -> &Mode {
        self.mode_stack.last().unwrap()
    }
    fn current_mode_mut(&mut self) -> &mut Mode {
        self.mode_stack.last_mut().unwrap()
    }
    fn handle_mode_change_side_effects(&mut self, previous: BuiltinMode, new: BuiltinMode) {
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
    }
    fn switch_mode(&mut self, mode: Mode) -> OperationResult {
        self.handle_mode_change_side_effects(
            self.current_mode().builtin_mode(),
            mode.builtin_mode(),
        );
        let current = self.current_mode_mut();
        if *current != mode {
            *current = mode;
            Ok(())
        } else {
            Err(())
        }
    }
    fn push_mode(&mut self, mode: Mode) {
        self.handle_mode_change_side_effects(
            self.current_mode().builtin_mode(),
            mode.builtin_mode(),
        );
        self.mode_stack.push(mode);
    }
    fn pop_mode(&mut self) -> OperationResult {
        if self.mode_stack.len() == 1 {
            return Err(());
        }
        let old = self.mode_stack.pop().unwrap();
        self.handle_mode_change_side_effects(
            old.builtin_mode(),
            self.current_mode().builtin_mode(),
        );
        Ok(())
    }
    fn set_current_room(&mut self, id: Option<&RoomId>) {
        if let Some(id) = id {
            self.room_selection.select(id);
        } else {
            self.room_selection.deselect();
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
    room_state: &'a crate::tui_app::RoomState,
    potentially_active: bool,
) -> impl Widget + 'a {
    let mut layout = VLayout::new();
    if let SendMessageType::Reply(orig) = &room_state.tui.msg_edit_type {
        layout = layout.widget(Foo(
            ColDemand::at_least(1),
            RowDemand::exact(1),
            move |mut w, _| messages::draw_event_preview(orig, room_state, &mut w),
        ));
    }
    layout.widget(
        HLayout::new().widget("> ").widget(
            room_state
                .tui
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
    } else if matches!(
        tui_state.current_mode().builtin_mode(),
        BuiltinMode::Command
    ) {
        hlayout = hlayout.widget(tui_state.command_line.as_widget())
    }

    hlayout = hlayout
        .widget(spacer)
        .widget(tui_state.current_mode().to_string())
        .widget(format!("{}", tui_state.previous_keys));
    hlayout
}

fn tui<'a>(state: &'a State, tasks: Tasks<'a>) -> impl Widget + 'a {
    let mut hlayout = HLayout::new()
        .separator(GraphemeCluster::try_from('│').unwrap())
        .widget_weighted(rooms::Rooms(state).as_widget(), 0.25);
    if let Some(room) = state.current_room_state() {
        hlayout = hlayout.widget_weighted(
            VLayout::new()
                .widget(messages::Messages(state, tasks))
                .widget(msg_edit(
                    room,
                    matches!(state.tui.current_mode().builtin_mode(), BuiltinMode::Insert),
                )),
            0.75,
        )
    }
    VLayout::new()
        .widget(hlayout)
        .widget(bottom_bar(&state.tui))
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

    let mut run = true;

    let message_query = RefCell::new(None);

    let tasks = Tasks {
        message_query: &message_query,
    };
    while run {
        {
            let state = state.lock().await;
            let win = term.create_root_window();
            tui(&state, tasks).draw(win, RenderingHints::new().active(true));
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
                        state: &mut state,
                        client: &client,
                        tasks,
                        continue_running: &mut run,
                        config: &config,
                        command_environment: &command_environment,
                    };
                    let input = input.chain(key_action_behavior(&mut c));
                    match state.tui.current_mode().builtin_mode() {
                        BuiltinMode::Normal => {}
                        BuiltinMode::Insert => {
                            if let Some(room) = state.current_room_state_mut() {
                                input.chain(EditBehavior::new(&mut room.tui.msg_edit));
                            }
                        }
                        BuiltinMode::Command => {
                            input
                                .chain(
                                    EditBehavior::new(&mut state.tui.command_line)
                                        .delete_forwards_on(Key::Delete)
                                        .delete_backwards_on(Key::Backspace)
                                        .left_on(Key::Left)
                                        .right_on(Key::Right),
                                )
                                .chain(
                                    ScrollBehavior::new(&mut state.tui.command_line)
                                        .backwards_on(Key::Up)
                                        .forwards_on(Key::Down),
                                );
                        }
                        BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread => {
                            input.chain(
                                EditBehavior::new(&mut state.tui.room_filter_line)
                                    .delete_forwards_on(Key::Delete)
                                    .delete_backwards_on(Key::Backspace),
                            );
                        }
                    };

                    if let Some(room) = state.current_room_state_mut() {
                        if let Some(read_event_id) = room.mark_newest_event_as_read() {
                            send_read_receipt(&client, &room.id, read_event_id);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum EventDetail {
    Full,
    Selected,
    Reduced,
}

impl std::default::Default for EventDetail {
    fn default() -> Self {
        EventDetail::Selected
    }
}

impl FromStr for EventDetail {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "full" => EventDetail::Full,
            "selected" => EventDetail::Selected,
            "reduced" => EventDetail::Reduced,
            _ => return Err(()),
        })
    }
}

impl std::fmt::Display for EventDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EventDetail::Full => "full",
            EventDetail::Selected => "selected",
            EventDetail::Reduced => "reduced",
        };
        write!(f, "{}", s)
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
