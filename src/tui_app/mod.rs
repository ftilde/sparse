use matrix_sdk::{
    self,
    config::SyncSettings,
    room::Room,
    ruma::api::client::r0::push::get_notifications::Notification,
    ruma::identifiers::{EventId, RoomId},
    ruma::{
        events::{
            room::message::MessageType, AnySyncMessageEvent, AnySyncRoomEvent, AnyToDeviceEvent,
        },
        UserId,
    },
    Client, LoopCtrl,
};

use crate::timeline::{self};

use nix::sys::signal::{SigSet, Signal};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use tui::Event;
use unsegen::base::Color;

pub mod tui;

type UserColors = BTreeMap<Box<UserId>, Color>;

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
    id: Box<RoomId>,
    pub messages: timeline::RoomTimelineCache,
    name: String,
    latest_read_message: Option<Box<EventId>>,
    num_unread_notifications: u64,
    last_notification_handle: Option<notify_rust::NotificationHandle>,
    user_colors: UserColors,

    pub tui: tui::RoomTuiState,
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
            id: room.room_id().to_owned(),
            messages: timeline::RoomTimelineCache::default(),
            name,
            latest_read_message,
            num_unread_notifications: room.unread_notification_counts().notification_count,
            last_notification_handle: None,
            user_colors: calculate_user_colors(room).await,
            tui: tui::RoomTuiState::at_last_message(),
        }
    }

    pub fn mark_newest_event_as_read(&mut self) -> Option<Box<EventId>> {
        self.num_unread_notifications = 0;
        self.last_notification_handle
            .take()
            .map(|handle| handle.close());
        let latest = self.messages.end_id();
        if latest.is_some() && self.latest_read_message.as_deref() != latest {
            self.latest_read_message = latest.map(|e| e.to_owned());
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
    pub rooms: BTreeMap<Box<RoomId>, RoomState>,
    tui: tui::TuiState,
    clipboard_context: Option<cli_clipboard::ClipboardContext>,
}

fn init_clipboard() -> Option<cli_clipboard::ClipboardContext> {
    use cli_clipboard::ClipboardProvider;
    match cli_clipboard::ClipboardContext::new() {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::error!("Failed to initiate clipboard {}", e);
            None
        }
    }
}

impl State {
    fn new(rooms: BTreeMap<Box<RoomId>, RoomState>) -> Self {
        let tui = crate::tui_app::tui::TuiState::new(rooms.keys().next().map(|k| &**k));
        State {
            rooms,
            tui,
            clipboard_context: init_clipboard(),
        }
    }
    async fn update_room_info(&mut self, room: &Room) {
        match room {
            Room::Joined(_) => {
                if let Some(r) = self.rooms.get_mut(room.room_id()) {
                    r.name = room.display_name().await.unwrap();
                    r.user_colors = calculate_user_colors(room).await;
                } else {
                    self.rooms
                        .insert(room.room_id().to_owned(), RoomState::from_room(room).await);
                }
            }
            Room::Left(room) => {
                self.rooms.remove(room.room_id());
            }
            Room::Invited(_) => { /*TODO*/ }
        }
    }
    fn current_room_state(&self) -> Option<&RoomState> {
        self.tui
            .room_selection
            .current()
            .map(|r| self.rooms.get(r).unwrap())
    }
    fn current_room_state_mut(&mut self) -> Option<&mut RoomState> {
        if let Some(id) = self.tui.room_selection.current() {
            Some(self.rooms.get_mut(id).unwrap())
        } else {
            None
        }
    }
}

async fn handle_notification(c: &Connection, room: &Room, notification: Notification) {
    let c = c.clone();
    let mut bell = None;
    let mut notification_handle = None;
    if notification
        .actions
        .iter()
        .any(|t| matches!(t, matrix_sdk::ruma::push::Action::Notify))
    {
        use crate::config::NotificationStyle;
        match notification.event.deserialize() {
            Ok(e) => {
                if Some(e.sender()) != c.client.user_id().await.as_deref() {
                    let mut notification = notify_rust::Notification::new();
                    let sender = e.sender().to_string();
                    let group_string = if room.is_direct() {
                        format!("{}", sender)
                    } else {
                        let g = room
                            .display_name()
                            .await
                            .unwrap_or_else(|_| "Unknown room".to_owned());
                        format!("{} in {}", sender, g)
                    };
                    let content = match e {
                        AnySyncRoomEvent::Message(m) => match m {
                            AnySyncMessageEvent::RoomMessage(m) => match m.content.msgtype {
                                MessageType::Text(t) => t.body,
                                MessageType::Image(_) => String::from("sent an image"),
                                MessageType::Audio(_) => String::from("sent an audio message"),
                                MessageType::Video(_) => String::from("sent a video"),
                                MessageType::File(_) => String::from("sent a file"),
                                o => format!("{:?}", o),
                            },
                            o => format!("{:?}", o),
                        },
                        _ => String::new(),
                    };
                    match c.config.notification_style {
                        NotificationStyle::Disabled => {}
                        NotificationStyle::NameOnly => {
                            notification.summary(&format!("{}", sender));
                        }
                        NotificationStyle::NameAndGroup => {
                            notification.summary(&group_string);
                        }
                        NotificationStyle::Full => {
                            notification.summary(&group_string);
                            notification.body(&format!("{}", content));
                        }
                    }
                    if !matches!(c.config.notification_style, NotificationStyle::Disabled) {
                        match notification.show() {
                            Ok(handle) => notification_handle = Some(handle),
                            Err(e) => tracing::error!("Failed to show notification {}", e),
                        }
                        bell = Some(Event::Bell);
                    }
                }
            }
            Err(e) => {
                tracing::error!("can't deserialize event from notification: {:?}", e)
            }
        }
    }
    {
        let mut state = c.state.lock().await;
        let m = &mut state.rooms.get_mut(room.room_id()).unwrap();
        m.num_unread_notifications = room.unread_notification_counts().notification_count;
        if let Some(handle) = notification_handle {
            m.last_notification_handle
                .replace(handle)
                .map(|old_handle| old_handle.close());
        }
        if let Some(bell) = bell {
            c.events.lock().await.send(bell).await.unwrap();
        } else {
            c.update().await;
        }
    }
}
async fn reset_timeline_cache(c: &Connection, room_id: &RoomId) {
    tracing::info!("Reseting cache of room {} due to new room key", room_id);
    let mut state = c.state.lock().await;
    let m = &mut state.rooms.get_mut(room_id).unwrap().messages;
    m.clear();
}

async fn run_matrix_event_loop(c: Connection) {
    // since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`
    let settings = SyncSettings::default();
    let client = c.client.clone();

    let c = &c;
    client
        .sync_with_callback(settings, |response| async move {
            for (room_id, notifications) in response.notifications {
                if let Some(room) = c.client.get_room(&room_id) {
                    for notification in notifications {
                        handle_notification(c, &room, notification).await;
                    }
                }
            }
            for e in response.to_device.events {
                match e.deserialize() {
                    Ok(AnyToDeviceEvent::RoomKey(e)) => {
                        reset_timeline_cache(&c, &e.content.room_id).await
                    }
                    Ok(AnyToDeviceEvent::ForwardedRoomKey(e)) => {
                        reset_timeline_cache(&c, &e.content.room_id).await
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("Failed to deserialize state event {}", e)
                    }
                }
            }
            for (room_id, room_info) in response.rooms.join {
                let timeline = room_info.timeline;

                let mut state = c.state.lock().await;
                let m = &mut state.rooms.get_mut(&room_id).unwrap().messages;
                m.handle_sync_batch(timeline, &response.next_batch);

                use matrix_sdk::ruma::events::AnySyncStateEvent;
                if let Some(room) = c.client.get_room(&room_id) {
                    for e in room_info.state.events {
                        match e.deserialize() {
                            Ok(
                                AnySyncStateEvent::RoomMember(_)
                                | AnySyncStateEvent::RoomName(_)
                                | AnySyncStateEvent::RoomCanonicalAlias(_),
                            ) => {
                                state.update_room_info(&room).await;
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("Failed to deserialize state event {}", e)
                            }
                        }
                    }
                }
            }

            c.update().await;
            LoopCtrl::Continue
        })
        .await;
}

#[derive(Clone)]
struct Connection {
    client: Client,
    state: Arc<Mutex<State>>,
    events: Arc<Mutex<mpsc::Sender<tui::Event>>>,
    config: crate::config::Config,
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
}

//#[async_trait]
//impl EventHandler for Connection {
//    /// Fires when `Client` receives room events that trigger notifications
//    /// according to the push rules of the user.
//    async fn on_room_notification(&self, room: Room, notification: Notification) {
//    }
//}

async fn run_matrix_message_fetch_loop(
    c: Connection,
    mut tasks: watch::Receiver<Option<tui::MessageQueryRequest>>,
) {
    while tasks.changed().await.is_ok() {
        let task = { tasks.borrow().clone() };
        if let Some(task) = task {
            let rid = task.room.as_ref();
            let room = c.client.get_room(rid).unwrap();

            let query = {
                let state = c.state.lock().await;
                let m = state.rooms.get(rid).unwrap();

                m.messages.events_query(&c.client, room, task.kind).await
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

pub async fn run(
    client: Client,
    config: crate::config::Config,
    command_environment: tui::actions::CommandEnvironment,
) -> Result<(), matrix_sdk::Error> {
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
            rooms.insert(id.to_owned(), RoomState::from_room(&room).await);
        }
    }
    let state = Arc::new(Mutex::new(State::new(rooms)));

    let (event_sender, event_receiver) = mpsc::channel(1);
    let (message_query_sender, message_query_receiver) = watch::channel(None);

    let connection = Connection {
        client: client.clone(),
        state: state.clone(),
        events: Arc::new(Mutex::new(event_sender.clone())),
        config: config.clone(),
    };

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

    tui::run_tui(
        event_receiver,
        message_query_sender,
        state,
        tui_client,
        command_environment,
        config,
    )
    .await;

    Ok(())
}
