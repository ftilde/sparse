use std::{env, process::exit};

mod tui;

use matrix_sdk::{
    self, async_trait,
    deserialized_responses::SyncRoomEvent,
    room::Room,
    ruma::events::{
        room::message::MessageEventContent,
        room::{
            aliases::AliasesEventContent,
            //avatar::AvatarEventContent,
            canonical_alias::CanonicalAliasEventContent,
            //join_rules::JoinRulesEventContent,
            member::MemberEventContent,
            //message::{feedback::FeedbackEventContent, MessageEventContent as EventEventContent},
            name::NameEventContent,
            //power_levels::PowerLevelsEventContent,
            //redaction::SyncRedactionEvent,
            //tombstone::TombstoneEventContent,
        },
        AnyMessageEventContent, AnySyncMessageEvent, AnySyncRoomEvent, SyncMessageEvent,
        SyncStateEvent,
    },
    ruma::identifiers::{EventId, RoomId},
    Client, ClientConfig, EventHandler, Session, SyncSettings,
};

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use url::Url;

type Event = AnySyncMessageEvent;

enum CacheEndState {
    Open,
    Reached,
}

struct RoomTimelineCache {
    messages: VecDeque<Event>,
    begin: CacheEndState,
    end: CacheEndState,
}

impl std::default::Default for RoomTimelineCache {
    fn default() -> Self {
        RoomTimelineCache {
            messages: VecDeque::new(),
            begin: CacheEndState::Open,
            end: CacheEndState::Open,
        }
    }
}

#[derive(Debug)]
enum EventWalkResult<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetchFrom(EventId),
    End,
}

#[derive(Debug)]
enum EventWalkResultNewest<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch,
    End,
}

#[derive(Copy, Clone, Debug)]
struct RoomTimelineIndex<'a> {
    pos: usize,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl RoomTimelineIndex<'_> {
    fn new(pos: usize) -> Self {
        RoomTimelineIndex {
            pos,
            _marker: std::marker::PhantomData::default(),
        }
    }
}

impl RoomTimelineCache {
    fn begin(&self) -> Option<&EventId> {
        self.messages.front().map(|m| m.event_id())
    }
    fn end(&self) -> Option<&EventId> {
        self.messages.back().map(|m| m.event_id())
    }

    fn clear(&mut self) {
        self.messages.clear();
    }

    fn append(&mut self, msg: Event) {
        self.messages.push_back(msg)
    }
    fn prepend(&mut self, msg: Event) {
        self.messages.push_front(msg)
    }

    fn message(&self, id: RoomTimelineIndex) -> &Event {
        &self.messages[id.pos]
    }

    fn walk_from_known(&self, id: &EventId) -> EventWalkResult {
        if let Some((i, _)) = self
            .messages
            .iter()
            .enumerate()
            .find(|(_, m)| *m.event_id() == *id)
        {
            EventWalkResult::Message(RoomTimelineIndex::new(i))
        } else {
            EventWalkResult::RequiresFetchFrom(id.clone())
        }
    }

    fn walk_from_newest(&self) -> EventWalkResultNewest {
        match self.end {
            CacheEndState::Reached => {
                if !self.messages.is_empty() {
                    EventWalkResultNewest::Message(RoomTimelineIndex::new(self.messages.len() - 1))
                } else {
                    EventWalkResultNewest::End
                }
            }
            CacheEndState::Open => EventWalkResultNewest::RequiresFetch,
        }
    }

    fn next<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
        let new_pos = pos.pos + 1;
        if new_pos < self.messages.len() {
            EventWalkResult::Message(RoomTimelineIndex::new(new_pos))
        } else {
            match self.end {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => {
                    let id = self
                        .end()
                        .expect("Since we have pos, messages cannot be empty");
                    EventWalkResult::RequiresFetchFrom(id.clone())
                }
            }
        }
    }
    fn previous<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
        if pos.pos > 0 {
            EventWalkResult::Message(RoomTimelineIndex::new(pos.pos - 1))
        } else {
            match self.begin {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => {
                    let id = self
                        .begin()
                        .expect("Since we have pos, messages cannot be empty");
                    EventWalkResult::RequiresFetchFrom(id.clone())
                }
            }
        }
    }
}

pub struct State {
    messages: BTreeMap<RoomId, RoomTimelineCache>,
    rooms: BTreeMap<RoomId, String>,
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
    events: Arc<Mutex<Sender<tui::Event>>>,
}

impl Connection {
    pub fn new(client: Client, state: Arc<Mutex<State>>, events: Sender<tui::Event>) -> Self {
        Self {
            client,
            state,
            events: Arc::new(Mutex::new(events)),
        }
    }

    pub async fn update(&self) {
        self.events
            .lock()
            .await
            .send(tui::Event::Update)
            .await
            .unwrap();
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
        m.end = CacheEndState::Open;
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

use std::path::PathBuf;

struct Config {
    username: String,
    host: Url,
}

impl Config {
    fn user_id(&self) -> String {
        format!("@{}:{}", self.username, self.host.host_str().unwrap_or(""))
    }

    fn data_dir(&self) -> PathBuf {
        dirs::data_local_dir()
            .unwrap()
            .join(APP_NAME)
            .join(self.user_id())
    }

    fn session_file_path(&self) -> PathBuf {
        self.data_dir().join("session")
    }
}

fn try_load_session(config: &Config) -> Result<Session, Box<dyn std::error::Error>> {
    let session_file = std::fs::File::open(config.session_file_path())?; //TODO: encrypt?
    Ok(serde_json::from_reader(session_file)?)
}

fn try_store_session(config: &Config, session: &Session) -> Result<(), Box<dyn std::error::Error>> {
    let session_file_path = config.session_file_path();
    std::fs::create_dir_all(session_file_path.parent().unwrap())?;
    let session_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(session_file_path)?;
    serde_json::to_writer(session_file, session)?;
    Ok(())
}

const APP_NAME: &str = "sparse";

async fn try_restore_session(
    client: &Client,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = try_load_session(config)?;
    client.restore_login(session).await?;

    // Test the token which may have been invalidated: We don't actually care about the result, but
    // it will fail if we are not logged in with the old token.
    let _ = client.devices().await?;
    Ok(())
}

async fn login(
    events: Sender<tui::Event>,
    config: Config,
) -> Result<(Client, Arc<Mutex<State>>), matrix_sdk::Error> {
    // the location for `JsonStore` to save files to

    let data_dir = config.data_dir();
    let client_config = ClientConfig::new().store_path(data_dir);
    // create a new Client with the given homeserver url and config
    let client = Client::new_with_config(config.host.clone(), client_config).unwrap();

    if try_restore_session(&client, &config).await.is_err() {
        eprintln!(
            "Could not restore session. Please provide the password for user {} to log in:",
            config.username
        );

        loop {
            match rpassword::read_password_from_tty(Some("Password: ")) {
                Ok(pw) if pw.is_empty() => {}
                Ok(pw) => {
                    let response = client
                        .login(&config.username, &pw, None, Some("command bot"))
                        .await;
                    match response {
                        Ok(response) => {
                            let session = Session {
                                access_token: response.access_token,
                                user_id: response.user_id,
                                device_id: response.device_id,
                            };

                            try_store_session(&config, &session).unwrap();
                            break;
                        }
                        Err(matrix_sdk::Error::Http(matrix_sdk::HttpError::ClientApi(
                            matrix_sdk::ruma::api::error::FromHttpResponseError::Http(
                                matrix_sdk::ruma::api::error::ServerError::Known(r),
                            ),
                        ))) => {
                            eprintln!("{}", r.message);
                        }
                        Err(e) => {
                            panic!("Unexpected error: {}", e);
                        }
                    }
                }
                Err(e) => panic!("{}", e),
            }
        }
    }
    eprintln!("Logged in as {}", config.username);
    let state = Arc::new(Mutex::new(State {
        rooms: BTreeMap::new(),
        messages: BTreeMap::new(),
    }));

    let handler_state = Arc::clone(&state);
    client
        .set_event_handler(Box::new(Connection::new(
            client.clone(),
            handler_state,
            events,
        )))
        .await;

    //let settings = SyncSettings::default()
    //    .token(client.sync_token().await.unwrap())
    //    .full_state(true);
    //client.sync(settings).await;

    //client.sync_once(SyncSettings::default()).await?;

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

    Ok((client, state))
}

async fn run_matrix_task_loop(c: Connection, mut tasks: Receiver<tui::Task>) {
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
            tui::Task::MoreMessages(rid, query) => {
                let room = c.client.get_room(&rid).unwrap();
                use matrix_sdk::ruma::api::client::r0::message::get_message_events::Direction;
                let (direction, start, end) = {
                    let mut state = c.state.lock().await;
                    let m = state.messages.entry(rid.clone()).or_default();

                    match &query {
                        tui::MessageQuery::AfterCache => {
                            (Direction::Forward, m.end().cloned(), None)
                        }
                        tui::MessageQuery::BeforeCache => {
                            (Direction::Backward, m.begin().cloned(), None)
                        }
                        tui::MessageQuery::Newest => (Direction::Backward, None, m.end().cloned()),
                    }
                };

                let messages = room
                    .messages(start.as_ref(), end.as_ref(), 10, direction)
                    .await
                    .unwrap();

                let mut state = c.state.lock().await;
                let m = state.messages.entry(rid).or_default();

                fn transform_events(
                    i: impl Iterator<Item = SyncRoomEvent>,
                ) -> impl Iterator<Item = Event> {
                    i.filter_map(|msg| match msg.event.deserialize() {
                        Ok(AnySyncRoomEvent::Message(e)) => Some(e),
                        Ok(o) => {
                            tracing::warn!("Unexpected event in get_messages call {:?}", o);
                            None
                        }
                        Err(e) => {
                            tracing::warn!("Failed to deserialize message {:?}", e);
                            None
                        }
                    })
                }

                if let Some(msgs) = messages {
                    match &query {
                        tui::MessageQuery::AfterCache => {
                            for msg in transform_events(msgs.into_iter()) {
                                m.append(msg);
                            }
                        }
                        tui::MessageQuery::BeforeCache => {
                            let mut iter = transform_events(msgs.into_iter());
                            if let Some(e) = iter.next() {
                                if m.begin() != Some(e.event_id()) {
                                    m.clear();
                                    m.prepend(e);
                                }
                            }
                            for msg in iter {
                                m.prepend(msg);
                            }
                        }
                        tui::MessageQuery::Newest => {
                            let mut iter = transform_events(msgs.into_iter().rev());
                            if let Some(e) = iter.next() {
                                if m.end() != Some(e.event_id()) {
                                    m.clear();
                                    m.append(e);
                                }
                            }
                            for msg in iter {
                                m.append(msg);
                            }
                            m.end = CacheEndState::Reached;
                        }
                    }
                } else {
                    match &query {
                        tui::MessageQuery::AfterCache => {
                            m.end = CacheEndState::Reached;
                        }
                        tui::MessageQuery::BeforeCache => {
                            m.begin = CacheEndState::Reached;
                        }
                        tui::MessageQuery::Newest => {
                            m.end = CacheEndState::Reached;
                        }
                    }
                }
                c.update().await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), matrix_sdk::Error> {
    //TODO: remove dirty dirty dirty hack with leak here
    let file = &*Box::leak(Box::new(std::fs::File::create("heyo.log").unwrap()));
    tracing_subscriber::fmt()
        .with_writer(move || file)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let (homeserver_url, username) = match (env::args().nth(1), env::args().nth(2)) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            eprintln!(
                "Usage: {} <homeserver_url> <username>",
                env::args().next().unwrap()
            );
            exit(1)
        }
    };

    const STDOUT: std::os::unix::io::RawFd = 0;
    let orig_attr = std::sync::Mutex::new(
        nix::sys::termios::tcgetattr(STDOUT).expect("Failed to get terminal attributes"),
    );

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

    let host = Url::parse(&homeserver_url).expect("Couldn't parse the homeserver URL");

    let config = Config { host, username };

    let (event_sender, event_receiver) = channel(5);
    let (task_sender, task_receiver) = channel(5);
    let (client, state) = login(event_sender.clone(), config).await?;

    let connection_tasks = Connection {
        client: client.clone(),
        state: state.clone(),
        events: Arc::new(Mutex::new(event_sender.clone())),
    };
    let connection_events = connection_tasks.clone();
    let _task_loop =
        tokio::spawn(async { run_matrix_task_loop(connection_tasks, task_receiver).await });
    let _event_loop = tokio::spawn(async { run_matrix_event_loop(connection_events).await });
    //tokio::spawn(async { tui::run_keyboard_loop(sender) });
    tui::start_keyboard_thread(event_sender);

    tui::run_tui(event_receiver, task_sender, state).await;

    // TODO: log out?
    Ok(())
}
