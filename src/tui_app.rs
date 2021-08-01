use matrix_sdk::{
    self, async_trait,
    room::Room,
    ruma::events::{
        room::message::MessageEventContent,
        room::{
            aliases::AliasesEventContent, canonical_alias::CanonicalAliasEventContent,
            member::MemberEventContent, name::NameEventContent,
        },
        AnyMessageEventContent, SyncMessageEvent, SyncStateEvent,
    },
    ruma::identifiers::RoomId,
    Client, EventHandler, SyncSettings,
};

use crate::timeline;
use crate::tui;

use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

pub struct State {
    messages: BTreeMap<RoomId, timeline::RoomTimelineCache>,
    rooms: BTreeMap<RoomId, String>,
}

impl State {
    pub fn rooms(&self) -> &BTreeMap<RoomId, String> {
        &self.rooms
    }
    pub fn messages(&self) -> &BTreeMap<RoomId, timeline::RoomTimelineCache> {
        &self.messages
    }
}

async fn run_matrix_event_loop(connection: Connection) {
    // since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`
    let settings = SyncSettings::default();
    // this keeps state from the server streaming in to Connection via the
    // EventHandler trait
    connection.client.sync(settings).await;
}

#[derive(Clone)]
struct Connection {
    client: Client,
    state: Arc<Mutex<State>>,
    events: Arc<Mutex<mpsc::Sender<tui::Event>>>,
}

impl Connection {
    pub async fn update(&self) {
        match self.events.lock().await.try_send(tui::Event::Update) {
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                /* We don't care if we can't send an update, since if the queue is full, the tui
                 * will be updated from these events anyway */
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => panic!("events was closed"),
        }
    }
    async fn update_room_info(&self, room: Room) {
        let display_name = room.display_name().await.unwrap();
        let mut state = self.state.lock().await;
        match room {
            Room::Joined(room) => {
                state.rooms.insert(room.room_id().clone(), display_name);
            }
            Room::Left(room) => {
                state.rooms.remove(room.room_id());
            }
            Room::Invited(_) => { /*TODO*/ }
        }
    }
}

#[async_trait]
impl EventHandler for Connection {
    // Handled in batches
    async fn on_room_message(&self, room: Room, _event: &SyncMessageEvent<MessageEventContent>) {
        //self.add_room_message(&room, event).await;
        let mut state = self.state.lock().await;
        let m = state.messages.entry(room.room_id().clone()).or_default();
        m.notify_new_messages();
        self.update().await;
    }
    async fn on_room_member(&self, room: Room, _: &SyncStateEvent<MemberEventContent>) {
        self.update_room_info(room).await
    }
    async fn on_room_name(&self, room: Room, _: &SyncStateEvent<NameEventContent>) {
        self.update_room_info(room).await
    }
    /// Fires when `Client` receives a `RoomEvent::RoomCanonicalAlias` event.
    async fn on_room_canonical_alias(
        &self,
        room: Room,
        _: &SyncStateEvent<CanonicalAliasEventContent>,
    ) {
        self.update_room_info(room).await
    }
    /// Fires when `Client` receives a `RoomEvent::RoomAliases` event.
    async fn on_room_aliases(&self, room: Room, _: &SyncStateEvent<AliasesEventContent>) {
        self.update_room_info(room).await
    }
}

async fn run_matrix_task_loop(c: Connection, mut tasks: mpsc::Receiver<tui::Task>) {
    while let Some(task) = tasks.recv().await {
        match task {
            tui::Task::Send(room_id, msg) => {
                if let Some(room) = c.client.get_joined_room(&room_id) {
                    let content =
                        AnyMessageEventContent::RoomMessage(MessageEventContent::text_plain(msg));
                    room.send(content, None).await.unwrap();
                } else {
                    panic!("can't send message, no joined room"); //TODO: we probably want to log something
                }
            }
        }
    }
}

async fn run_matrix_message_fetch_loop(
    c: Connection,
    mut tasks: watch::Receiver<Option<tui::MessageQueryRequest>>,
) {
    while tasks.changed().await.is_ok() {
        let task = { tasks.borrow().clone() };
        if let Some(task) = task {
            let rid = &task.room;
            let room = c.client.get_room(rid).unwrap();

            let query = {
                let mut state = c.state.lock().await;
                let m = state.messages.entry(rid.clone()).or_default();

                m.events_query(room, task.kind)
            };

            let res = query.await.unwrap();

            let mut state = c.state.lock().await;
            let m = state.messages.get_mut(rid).unwrap();
            m.update(res);
            c.update().await;
        }
    }
}

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let state = Arc::new(Mutex::new(State {
        rooms: BTreeMap::new(),
        messages: BTreeMap::new(),
    }));

    let (event_sender, event_receiver) = mpsc::channel(1);
    let (task_sender, task_receiver) = mpsc::channel(1);
    let (message_query_sender, message_query_receiver) = watch::channel(None);

    let connection = Connection {
        client: client.clone(),
        state: state.clone(),
        events: Arc::new(Mutex::new(event_sender.clone())),
    };

    client.set_event_handler(Box::new(connection.clone())).await;

    let mut rooms = BTreeMap::new();
    for room in client.joined_rooms() {
        let id = room.room_id();
        if let Some(room) = client.get_room(id) {
            rooms.insert(id.clone(), room.display_name().await.unwrap());
        }
    }
    {
        let mut state = state.lock().await;
        state.rooms = rooms;
    }

    let orig_attr = std::sync::Mutex::new(
        nix::sys::termios::tcgetattr(STDOUT).expect("Failed to get terminal attributes"),
    );

    const STDOUT: std::os::unix::io::RawFd = 0;
    ::std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        use std::os::unix::io::FromRawFd;
        // We open another instance of stdout here because the std::io::stdout is behind a lock
        // which is held by the tui thread. The output may be a bit garbled, but it's better than
        // not printing anything at all. We may find a better solution at some point...

        // Safety: stdout is always present
        let mut stdout = unsafe { std::fs::File::from_raw_fd(STDOUT) };

        // Switch back to main screen
        writeln!(
            stdout,
            "{}{}",
            termion::screen::ToMainScreen,
            termion::cursor::Show
        )
        .unwrap();
        // Restore old terminal behavior (will be restored later automatically, but we want to be
        // able to properly print the panic info)
        let _ = nix::sys::termios::tcsetattr(
            STDOUT,
            nix::sys::termios::SetArg::TCSANOW,
            &orig_attr.lock().unwrap(),
        );

        writeln!(
            stdout,
            "Oh no! sparse crashed!\n{}\n{:?}",
            info,
            backtrace::Backtrace::new()
        )
        .unwrap();
    }));

    let connection_events = connection.clone();
    let connection_queries = connection.clone();
    let _task_loop = tokio::spawn(async { run_matrix_task_loop(connection, task_receiver).await });
    let _event_loop = tokio::spawn(async { run_matrix_event_loop(connection_events).await });
    let _message_query_loop = tokio::spawn(async {
        run_matrix_message_fetch_loop(connection_queries, message_query_receiver).await
    });
    //tokio::spawn(async { tui::run_keyboard_loop(sender) });
    tui::start_keyboard_thread(event_sender);

    tui::run_tui(event_receiver, task_sender, message_query_sender, state).await;

    Ok(())
}
