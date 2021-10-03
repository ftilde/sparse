use matrix_sdk::{
    self, async_trait,
    room::Room,
    ruma::api::client::r0::push::get_notifications::Notification,
    ruma::events::{
        room::message::{MessageEventContent, MessageType},
        room::{
            aliases::AliasesEventContent, canonical_alias::CanonicalAliasEventContent,
            member::MemberEventContent, name::NameEventContent,
        },
        AnySyncMessageEvent, AnySyncRoomEvent, SyncMessageEvent, SyncStateEvent,
    },
    ruma::identifiers::{EventId, RoomId},
    ruma::UserId,
    Client, EventHandler, SyncSettings,
};

use crate::timeline;

use nix::sys::signal::{SigSet, Signal};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use tui::Event;
use unsegen::base::Color;

mod config;
mod tui;
type UserColors = BTreeMap<UserId, Color>;

async fn calculate_user_colors(room: &Room) -> UserColors {
    let available_colors = [
        Color::Red,
        Color::Blue,
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
    ];
    let num_colors = available_colors.len();
    let own_color = Color::White;

    let own_user_id = room.own_user_id();
    let users = room.joined_user_ids().await.unwrap();

    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut raw_colors = users
        .into_iter()
        .filter(|i| i != own_user_id)
        .map(|i| {
            let mut hasher = DefaultHasher::new();
            i.as_str().hash(&mut hasher);
            (i.to_owned(), hasher.finish() as usize % num_colors)
        })
        .peekable();

    let mut user_colors = UserColors::new();
    user_colors.insert(own_user_id.to_owned(), own_color);

    let mut table = vec![None; num_colors];
    'outer: while let Some((_id, pos)) = raw_colors.peek() {
        for o in 0..num_colors {
            let pos = (pos + o) % num_colors;

            let entry = table.get_mut(pos).unwrap();
            if entry.is_none() {
                *entry = Some(raw_colors.next().unwrap().0);
                continue 'outer;
            }
        }

        for (i, e) in table.iter_mut().enumerate() {
            if let Some(e) = e.take() {
                user_colors.insert(e, available_colors[i]);
            }
        }
    }
    for (i, e) in table.into_iter().enumerate() {
        if let Some(e) = e {
            user_colors.insert(e, available_colors[i]);
        }
    }
    user_colors
}

pub struct RoomState {
    pub messages: timeline::RoomTimelineCache,
    name: String,
    latest_read_message: Option<EventId>,
    num_unread_notifications: u64,
    user_colors: UserColors,
}

impl RoomState {
    async fn from_room(room: &Room) -> Self {
        let name = room.display_name().await.unwrap();
        let latest_read_message = room
            .user_read_receipt(room.own_user_id())
            .await
            .unwrap()
            .map(|(id, _)| id);
        RoomState {
            messages: timeline::RoomTimelineCache::default(),
            name,
            latest_read_message,
            num_unread_notifications: room.unread_notification_counts().notification_count,
            user_colors: calculate_user_colors(room).await,
        }
    }

    pub fn mark_newest_event_as_read(&mut self) -> Option<EventId> {
        self.num_unread_notifications = 0;
        let latest = self.messages.end().cloned();
        if latest.is_some() && self.latest_read_message != latest {
            self.latest_read_message = latest;
            self.latest_read_message.clone()
        } else {
            None
        }
    }
    pub fn num_unread_notifications(&self) -> u64 {
        self.num_unread_notifications
    }
    pub fn has_unread(&self) -> bool {
        self.num_unread_notifications > 0
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub struct State {
    pub rooms: BTreeMap<RoomId, RoomState>,
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
        let mut state = self.state.lock().await;
        match room {
            Room::Joined(_) => {
                if let Some(r) = state.rooms.get_mut(room.room_id()) {
                    r.name = room.display_name().await.unwrap();
                    r.user_colors = calculate_user_colors(&room).await;
                } else {
                    state
                        .rooms
                        .insert(room.room_id().clone(), RoomState::from_room(&room).await);
                }
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
        let m = &mut state.rooms.get_mut(&room.room_id()).unwrap().messages;
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
    /// Fires when `Client` receives room events that trigger notifications
    /// according to the push rules of the user.
    async fn on_room_notification(&self, room: Room, notification: Notification) {
        if notification
            .actions
            .iter()
            .any(|t| matches!(t, matrix_sdk::ruma::push::Action::Notify))
        {
            match notification.event.deserialize() {
                Ok(e) => {
                    if Some(e.sender()) != self.client.user_id().await.as_ref() {
                        let mut notification = notify_rust::Notification::new();
                        if room.is_direct() {
                            notification.summary(e.sender().as_str());
                        } else {
                            notification.summary(&format!(
                                "{} in {}",
                                e.sender().as_str(),
                                room.display_name()
                                    .await
                                    .unwrap_or_else(|_| "Unknown room".to_owned())
                            ));
                        }
                        match e {
                            AnySyncRoomEvent::Message(m) => match m {
                                AnySyncMessageEvent::RoomMessage(m) => match m.content.msgtype {
                                    MessageType::Text(t) => {
                                        notification.body(&t.body);
                                    }
                                    o => {
                                        notification.body(&format!("{:?}", o));
                                    }
                                },
                                o => {
                                    notification.body(&format!("{:?}", o));
                                }
                            },
                            _ => {}
                        }
                        if let Err(e) = notification.show() {
                            tracing::error!("Failed to show notification {}", e);
                        }
                    }
                }
                Err(e) => tracing::error!("can't deserialize event from notification: {:?}", e),
            }
        }
        {
            let mut state = self.state.lock().await;
            let m = &mut state.rooms.get_mut(&room.room_id()).unwrap();
            m.num_unread_notifications = room.unread_notification_counts().notification_count;
            self.update().await;
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
                let state = c.state.lock().await;
                let m = state.rooms.get(&rid).unwrap();

                m.messages.events_query(room, task.kind)
            };

            let res = query.await.unwrap();

            let mut state = c.state.lock().await;
            let m = state.rooms.get_mut(rid).unwrap();
            m.messages.update(res);
            c.update().await;
        }
    }
}

fn signals_to_block() -> SigSet {
    let mut signals_to_block = signals_to_wait();
    signals_to_block.add(Signal::SIGCONT);
    signals_to_block
}
fn signals_to_wait() -> SigSet {
    let mut signals_to_wait = nix::sys::signal::SigSet::empty();
    signals_to_wait.add(Signal::SIGWINCH);
    signals_to_wait.add(Signal::SIGTSTP);
    signals_to_wait.add(Signal::SIGTERM);
    signals_to_wait
}

fn start_signal_thread(sink: mpsc::Sender<Event>) {
    let _ = std::thread::Builder::new()
        .name("input".to_owned())
        .spawn(move || {
            let signals_to_wait = signals_to_wait();
            loop {
                if let Ok(signal) = signals_to_wait.wait() {
                    sink.blocking_send(Event::Signal(signal)).unwrap();
                }
            }
        });
}

fn start_keyboard_thread(sink: mpsc::Sender<Event>) {
    use unsegen::input::Input;
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

pub fn init() {
    signals_to_block().thread_block().unwrap();
}

pub async fn run(client: Client) -> Result<(), matrix_sdk::Error> {
    let state = Arc::new(Mutex::new(State {
        rooms: BTreeMap::new(),
    }));

    // Fetch the initial list of rooms. This is required (for some reason) because joined_rooms()
    // returns an empty vec on the first start for some reason.
    //
    // Also: We have to enable lazy loading of members because otherwise the calculation of room
    // display names is broken. (There is a note about that in the implementation in matrix_sdk...)
    use matrix_sdk::ruma::api::client::r0::filter::{FilterDefinition, LazyLoadOptions};
    use matrix_sdk::ruma::api::client::r0::sync::sync_events::Filter;
    client
        .sync_once(SyncSettings::new().filter({
            let mut filter_def = FilterDefinition::empty();
            filter_def.room.state.lazy_load_options = LazyLoadOptions::Enabled {
                include_redundant_members: false,
            };
            Filter::FilterDefinition(filter_def)
        }))
        .await?;
    let mut rooms = BTreeMap::new();
    for room in client.joined_rooms() {
        let id = room.room_id();
        if let Some(room) = client.get_room(id) {
            rooms.insert(id.clone(), RoomState::from_room(&room).await);
        }
    }
    {
        let mut state = state.lock().await;
        state.rooms = rooms;
    }

    let (event_sender, event_receiver) = mpsc::channel(1);
    let (message_query_sender, message_query_receiver) = watch::channel(None);

    let connection = Connection {
        client: client.clone(),
        state: state.clone(),
        events: Arc::new(Mutex::new(event_sender.clone())),
    };

    client.set_event_handler(Box::new(connection.clone())).await;

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

    let tui_client = connection.client.clone();
    let connection_events = connection.clone();
    let connection_queries = connection;
    let _event_loop = tokio::spawn(async { run_matrix_event_loop(connection_events).await });
    let _message_query_loop = tokio::spawn(async {
        run_matrix_message_fetch_loop(connection_queries, message_query_receiver).await
    });
    //tokio::spawn(async { tui::run_keyboard_loop(sender) });

    start_signal_thread(event_sender.clone());
    start_keyboard_thread(event_sender);

    tui::run_tui(event_receiver, message_query_sender, state, tui_client).await;

    Ok(())
}
