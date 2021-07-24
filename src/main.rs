use std::{env, process::exit};

mod tui;

use matrix_sdk::api::r0::message::get_message_events::Request as MessageRequest;
use matrix_sdk::{
    self, async_trait,
    events::{
        room::message::MessageEventContent,
        room::{
            aliases::AliasesEventContent,
            //avatar::AvatarEventContent,
            canonical_alias::CanonicalAliasEventContent,
            //join_rules::JoinRulesEventContent,
            member::MemberEventContent,
            //message::{feedback::FeedbackEventContent, MessageEventContent as MsgEventContent},
            name::NameEventContent,
            //power_levels::PowerLevelsEventContent,
            //redaction::SyncRedactionEvent,
            //tombstone::TombstoneEventContent,
        },
        AnyMessageEvent, AnyMessageEventContent, AnyRoomEvent, AnySyncMessageEvent,
        AnySyncRoomEvent, SyncMessageEvent, SyncStateEvent,
    },
    identifiers::RoomId,
    room::Room,
    Client, ClientConfig, EventHandler, LoopCtrl, Session, SyncSettings,
};

use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use url::Url;

type Msg = SyncMessageEvent<MessageEventContent>;

type RoomMessageChunkToken = String;

enum Previous {
    Known(usize),
    Unknown(RoomMessageChunkToken),
    None,
}

enum Next {
    Known(usize),
    Unknown(RoomMessageChunkToken),
}

struct RoomMessageChunk {
    previous: Previous,
    next: Next,
    messages: Vec<Msg>,
}

#[derive(Default)]
struct RoomMessages {
    chunks: Vec<RoomMessageChunk>,
    start: BTreeMap<RoomMessageChunkToken, usize>,
    end: BTreeMap<RoomMessageChunkToken, usize>,
    newest_chunk: usize,
}

#[derive(Copy, Clone)]
pub struct MessageID {
    chunk: usize,
    msg: usize,
}

pub enum MsgWalkResult {
    Msg(MessageID),
    End,
    RequiresFetch(RoomMessageChunkToken),
}

impl RoomMessages {
    fn newest_message(&self) -> Option<MessageID> {
        if self.chunks.is_empty() {
            None
        } else {
            let chunk = self.newest_chunk;
            Some(MessageID {
                chunk,
                msg: self.chunks[chunk].messages.len() - 1,
            })
        }
    }

    fn message(&self, id: MessageID) -> &Msg {
        &self.chunks[id.chunk].messages[id.msg]
    }

    fn next(&self, id: MessageID) -> MsgWalkResult {
        let chunk = &self.chunks[id.chunk];
        if id.msg + 1 < chunk.messages.len() {
            return MsgWalkResult::Msg(MessageID {
                chunk: id.chunk,
                msg: id.msg + 1,
            });
        }

        match &chunk.next {
            Next::Known(chunk_id) => MsgWalkResult::Msg(MessageID {
                chunk: *chunk_id,
                msg: 0,
            }),
            Next::Unknown(token) => {
                if id.chunk == self.newest_chunk {
                    MsgWalkResult::RequiresFetch(token.clone())
                } else {
                    MsgWalkResult::End
                }
            }
        }
    }

    fn previous(&self, id: MessageID) -> MsgWalkResult {
        let chunk = &self.chunks[id.chunk];
        if id.msg > 0 {
            return MsgWalkResult::Msg(MessageID {
                chunk: id.chunk,
                msg: id.msg - 1,
            });
        }

        match &chunk.previous {
            Previous::Known(chunk_id) => MsgWalkResult::Msg(MessageID {
                chunk: *chunk_id,
                msg: self.chunks[*chunk_id].messages.len() - 1,
            }),
            Previous::Unknown(token) => MsgWalkResult::RequiresFetch(token.clone()),
            Previous::None => MsgWalkResult::End,
        }
    }

    fn add_chunk(
        &mut self,
        messages: Vec<Msg>,
        before_token: Option<RoomMessageChunkToken>,
        after_token: RoomMessageChunkToken,
    ) {
        assert!(!messages.is_empty(), "Tried to add empty message chunk");

        let this_id = self.chunks.len();

        let next = if let Some(i) = self.start.get(&after_token) {
            self.chunks[*i].previous = Previous::Known(this_id);
            Next::Known(*i)
        } else {
            Next::Unknown(after_token.clone())
        };

        let previous = if let Some(t) = before_token.clone() {
            if let Some(i) = self.end.get(&t) {
                self.chunks[*i].next = Next::Known(this_id);
                if *i == self.newest_chunk {
                    self.newest_chunk = this_id;
                }
                Previous::Known(*i)
            } else {
                Previous::Unknown(t)
            }
        } else {
            Previous::None
        };

        if let Some(b) = before_token {
            let prev = self.start.insert(b, this_id);
            assert!(prev.is_none());
        }
        let prev = self.end.insert(after_token, this_id);
        assert!(prev.is_none());

        self.chunks.push(RoomMessageChunk {
            messages,
            previous,
            next,
        });
    }
}

pub struct State {
    messages: BTreeMap<RoomId, RoomMessages>,
    rooms: BTreeMap<RoomId, String>,
}

async fn run_matrix_event_loop(connection: Connection) {
    // since we called `sync_once` before we entered our sync loop we must pass
    // that sync token to `sync`
    let settings = SyncSettings::default();
    // this keeps state from the server streaming in to Connection via the
    // EventHandler trait
    //client.sync(settings).await;
    connection
        .client
        .sync_with_callback(
            settings,
            |r: matrix_sdk::deserialized_responses::SyncResponse| async {
                eprint!("Sync!");
                for (rid, room) in r.rooms.join {
                    let mut msgs = Vec::new();
                    for msg in room.timeline.events {
                        match msg.event.deserialize() {
                            Ok(AnySyncRoomEvent::Message(AnySyncMessageEvent::RoomMessage(e))) => {
                                msgs.push(e);
                            }
                            Ok(o) => {
                                tracing::warn!("Unexpected event in get_messages call {:?}", o)
                            }
                            Err(e) => tracing::warn!("Failed to deserialize message {:?}", e),
                        }
                    }

                    eprint!("Sync2!");
                    let mut state = connection.state.lock().await;
                    eprint!("Sync3!");
                    let room_messages = state.messages.entry(rid).or_default();

                    let after_token = r.next_batch.clone();
                    let before_token = room.timeline.prev_batch;

                    if !msgs.is_empty() {
                        room_messages.add_chunk(msgs, before_token, after_token);
                    }
                }
                eprint!("Sync4!");
                connection.update().await;

                LoopCtrl::Continue
            },
        )
        .await;
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
    //async fn add_room_message(&self, room: &Room, event: &SyncMessageEvent<MessageEventContent>) {
    //    eprintln!("got message: {:?}", event);
    //    if let Room::Joined(room) = room {
    //        {
    //            let mut state = self.state.lock().await;
    //            let room_messages = state.messages.entry(room.room_id().clone()).or_default();
    //            room_messages.insert(event.origin_server_ts, event.clone());
    //        }
    //    }
    //}
}

#[async_trait]
impl EventHandler for Connection {
    // Handled in batches
    //async fn on_room_message(&self, room: Room, event: &SyncMessageEvent<MessageEventContent>) {
    //    //self.add_room_message(&room, event).await;
    //    self.update().await;
    //}
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
                            matrix_sdk::FromHttpResponseError::Http(
                                matrix_sdk::ServerError::Known(r),
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
                eprintln!("Trying to get messages...");
                let room = c.client.get_room(&rid).unwrap();
                let messages = room
                    .messages(match &query {
                        tui::MessageQuery::After(tok) => MessageRequest::forward(&rid, tok),
                        tui::MessageQuery::Before(tok) => MessageRequest::backward(&rid, tok),
                    })
                    .await
                    .unwrap();
                let mut msgs = Vec::new();
                for msg in messages.chunk {
                    match msg.deserialize() {
                        Ok(AnyRoomEvent::Message(AnyMessageEvent::RoomMessage(e))) => {
                            let msg: SyncMessageEvent<_> = e.into();
                            msgs.push(msg);
                        }
                        Ok(o) => tracing::warn!("Unexpected event in get_messages call {:?}", o),
                        Err(e) => tracing::warn!("Failed to deserialize message {:?}", e),
                    }
                }

                {
                    let mut state = c.state.lock().await;
                    let room_messages = state.messages.entry(rid).or_default();

                    let (msgs, before_token, after_token) = match query {
                        tui::MessageQuery::After(_tok) => {
                            (msgs, messages.start, messages.end.unwrap())
                        }
                        tui::MessageQuery::Before(_tok) => (
                            msgs.into_iter().rev().collect(),
                            messages.end,
                            messages.start.unwrap(),
                        ),
                    };

                    room_messages.add_chunk(msgs, before_token, after_token);
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
        // Switch back to main screen
        println!("{}{}", termion::screen::ToMainScreen, termion::cursor::Show);
        // Restore old terminal behavior (will be restored later automatically, but we want to be
        // able to properly print the panic info)
        let _ = nix::sys::termios::tcsetattr(
            STDOUT,
            nix::sys::termios::SetArg::TCSANOW,
            &orig_attr.lock().unwrap(),
        );

        println!("Oh no! sparse crashed!");
        println!("{}", info);
        println!("{:?}", backtrace::Backtrace::new());
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
    let task_loop =
        tokio::spawn(async { run_matrix_task_loop(connection_tasks, task_receiver).await });
    let event_loop = tokio::spawn(async { run_matrix_event_loop(connection_events).await });
    //tokio::spawn(async { tui::run_keyboard_loop(sender) });
    tui::start_keyboard_thread(event_sender);

    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let tui_loop = tokio::task::spawn_local(async {
                tui::run_tui(event_receiver, task_sender, state).await
            });
            //TODO: detect if task_loop/event_loop are canceled and stop tui loop first, then print panic somehow.  maybe join in select with timeout or something?
            let _ = tokio::select!(
                _ = task_loop => {},
                _ = event_loop => {},
                _ = tui_loop => {},
            );
        })
        .await;

    // TODO: log out?
    Ok(())
}
