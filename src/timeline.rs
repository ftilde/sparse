use matrix_sdk::ruma::api::client::r0::message::get_message_events::{self, Direction};
use matrix_sdk::ruma::events::room::encrypted::Relation;
use matrix_sdk::{
    deserialized_responses::SyncRoomEvent,
    room::{Messages, Room},
    ruma::events::{
        reaction::ReactionEventContent, AnySyncMessageEvent, AnySyncRoomEvent, SyncMessageEvent,
    },
    ruma::identifiers::EventId,
    Client,
};
use std::collections::{HashMap, VecDeque};

pub type Event = AnySyncRoomEvent;
pub type Reaction = SyncMessageEvent<ReactionEventContent>;
pub type Reactions = HashMap<String, Vec<Reaction>>;

pub enum CacheEndState {
    Open,
    Reached,
}

pub struct RoomTimelineCache {
    message_index_offset: isize,
    messages: VecDeque<Event>,
    index: HashMap<Box<EventId>, RoomTimelineIndex>,
    pub begin: CacheEndState,
    pub end: CacheEndState,
    begin_token: Option<String>,
    end_token: Option<String>,
    reactions: HashMap<Box<EventId>, Reactions>,
    msg_to_edits: HashMap<Box<EventId>, Vec<Event>>,
    edits_to_original: HashMap<Box<EventId>, Box<EventId>>,
}

impl std::default::Default for RoomTimelineCache {
    fn default() -> Self {
        RoomTimelineCache {
            message_index_offset: 0,
            messages: VecDeque::new(),
            index: HashMap::new(),
            begin: CacheEndState::Open,
            end: CacheEndState::Open,
            begin_token: None,
            end_token: None,
            reactions: HashMap::new(),
            msg_to_edits: HashMap::new(),
            edits_to_original: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EventWalkResult {
    Message(RoomTimelineIndex),
    RequiresFetch,
    End,
}

impl EventWalkResult {
    pub fn message(&self) -> Option<RoomTimelineIndex> {
        if let EventWalkResult::Message(m) = self {
            Some(*m)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum EventWalkResultNewest {
    Message(RoomTimelineIndex),
    RequiresFetch(Option<RoomTimelineIndex>), //There may be newer events, but this is the newest we got
    End,
}

impl EventWalkResultNewest {
    pub fn message(&self) -> Option<RoomTimelineIndex> {
        if let EventWalkResultNewest::Message(m) = self {
            Some(*m)
        } else {
            None
        }
    }
}

#[derive(Copy, Clone)]
pub enum TimelineEntry<'a> {
    Simple(&'a Event),
    Deleted(&'a Event),
    Edited {
        original: &'a Event,
        versions: &'a Vec<Event>,
    },
}

impl<'a> TimelineEntry<'a> {
    pub fn latest(self) -> Option<&'a Event> {
        match self {
            TimelineEntry::Simple(e) => Some(e),
            TimelineEntry::Deleted(_) => None,
            TimelineEntry::Edited { versions, .. } => Some(versions.last().unwrap()),
        }
    }
    pub fn original(self) -> &'a Event {
        match self {
            TimelineEntry::Simple(e) => e,
            TimelineEntry::Deleted(e) => e,
            TimelineEntry::Edited { original, .. } => original,
        }
    }
    pub fn event_id(self) -> &'a EventId {
        self.original().event_id()
    }
}

const QUERY_BATCH_SIZE_LIMIT: u32 = 10;

#[derive(Copy, Clone, Debug)]
pub struct RoomTimelineIndex {
    pos: isize,
}

impl RoomTimelineIndex {
    fn new(pos: isize) -> Self {
        RoomTimelineIndex { pos }
    }
}

impl RoomTimelineCache {
    pub fn end_id(&self) -> Option<&EventId> {
        self.messages.back().map(|e| e.event_id())
    }

    pub fn reactions(&self, id: &EventId) -> Option<&Reactions> {
        self.reactions.get(id)
    }

    fn clear_timeline(&mut self) {
        self.messages.clear();
        self.message_index_offset = 0;
        self.index.clear();
        self.msg_to_edits.clear();
        self.edits_to_original.clear();
        self.reactions.clear();
    }

    pub fn clear(&mut self) {
        self.clear_timeline();
        self.begin = CacheEndState::Open;
        self.end = CacheEndState::Open;
        self.begin_token = None;
        self.end_token = None;
    }

    fn pre_process_message(&mut self, msg: Event) -> Option<Event> {
        match msg {
            Event::Message(AnySyncMessageEvent::Reaction(r)) => {
                self.reactions
                    .entry(r.content.relates_to.event_id.to_owned())
                    .or_default()
                    .entry(r.content.relates_to.emoji.to_owned())
                    .or_default()
                    .push(r);
                None
            }
            Event::Message(m) => {
                if let Some(Relation::Replacement(r)) = m.content().relation() {
                    self.edits_to_original
                        .insert(m.event_id().to_owned(), r.event_id.clone());
                    self.msg_to_edits
                        .entry(r.event_id.clone())
                        .or_default()
                        .push(Event::Message(m));
                    None
                } else {
                    Some(Event::Message(m))
                }
            }
            o => Some(o),
        }
    }

    fn buffer_index_to_message_index(&self, id: usize) -> RoomTimelineIndex {
        RoomTimelineIndex {
            pos: id as isize - self.message_index_offset,
        }
    }

    fn message_index_to_buffer_index(&self, id: RoomTimelineIndex) -> usize {
        let index = id.pos + self.message_index_offset;
        assert!(index >= 0);
        index as usize
    }

    fn append(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            let id = self.buffer_index_to_message_index(self.messages.len());
            self.index.insert(msg.event_id().to_owned(), id);
            self.messages.push_back(msg);
        }
    }
    fn prepend(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            self.message_index_offset += 1;

            let id = self.buffer_index_to_message_index(0);
            self.index.insert(msg.event_id().to_owned(), id);
            self.messages.push_front(msg)
        }
    }

    fn entry_from_event<'a>(&'a self, original: &'a Event) -> TimelineEntry<'a> {
        if let Some(e) = self.msg_to_edits.get(original.event_id()) {
            TimelineEntry::Edited {
                original,
                versions: e,
            }
        } else {
            TimelineEntry::Simple(original)
        }
    }

    fn event_at(&self, id: RoomTimelineIndex) -> &Event {
        let index = self.message_index_to_buffer_index(id);
        &self.messages[index]
    }

    pub fn message(&self, id: RoomTimelineIndex) -> TimelineEntry {
        let original = self.event_at(id);
        self.entry_from_event(original)
    }

    fn find(&self, id: &EventId) -> Option<RoomTimelineIndex> {
        let orig_id = self
            .edits_to_original
            .get(id)
            .map(|e| e.as_ref())
            .unwrap_or(id);
        self.index.get(orig_id).map(|i| *i)
    }

    pub fn original_message(&self, id: &EventId) -> Option<&Event> {
        self.find(id).map(|i| self.event_at(i))
    }

    pub fn message_from_id(&self, id: &EventId) -> Option<TimelineEntry> {
        self.find(id)
            .map(|i| self.entry_from_event(self.event_at(i)))
    }

    pub fn walk_from_known(&self, id: &EventId) -> EventWalkResult {
        if let Some(i) = self.find(id) {
            EventWalkResult::Message(i)
        } else {
            EventWalkResult::RequiresFetch
        }
    }

    pub fn walk_from_newest(&self) -> EventWalkResultNewest {
        match self.end {
            CacheEndState::Reached => {
                if !self.messages.is_empty() {
                    EventWalkResultNewest::Message(
                        self.buffer_index_to_message_index(self.messages.len() - 1),
                    )
                } else {
                    EventWalkResultNewest::End
                }
            }
            CacheEndState::Open => EventWalkResultNewest::RequiresFetch(
                if let Some(i) = self.messages.len().checked_sub(1) {
                    Some(self.buffer_index_to_message_index(i))
                } else {
                    None
                },
            ),
        }
    }

    pub fn next<'a>(&'a self, pos: RoomTimelineIndex) -> EventWalkResult {
        let new_pos = RoomTimelineIndex::new(pos.pos + 1);
        if self.message_index_to_buffer_index(new_pos) < self.messages.len() {
            EventWalkResult::Message(new_pos)
        } else {
            match self.end {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => EventWalkResult::RequiresFetch,
            }
        }
    }
    pub fn previous<'a>(&'a self, pos: RoomTimelineIndex) -> EventWalkResult {
        if pos.pos + self.message_index_offset > 0 {
            EventWalkResult::Message(RoomTimelineIndex::new(pos.pos - 1))
        } else {
            match self.begin {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => EventWalkResult::RequiresFetch,
            }
        }
    }

    pub async fn events_query(
        &self,
        client: &Client,
        room: Room,
        query: MessageQuery,
    ) -> impl std::future::Future<Output = matrix_sdk::Result<MessageQueryResult>> {
        let room_id = room.room_id().to_owned();

        let (query, dir, from, to) = {
            match (query, &self.begin_token, &self.end_token) {
                (MessageQuery::AfterCache, _, Some(t)) => (
                    MessageQuery::AfterCache,
                    Direction::Forward,
                    t.clone(),
                    client.sync_token().await,
                ),
                (MessageQuery::BeforeCache, Some(t), _) => (
                    MessageQuery::BeforeCache,
                    Direction::Backward,
                    t.clone(),
                    None,
                ),
                (_, _, t) => (
                    MessageQuery::Newest,
                    Direction::Backward,
                    client.sync_token().await.unwrap(),
                    t.clone(),
                ),
            }
        };

        async move {
            let mut request = get_message_events::Request::new(&room_id, &from, dir);
            request.limit = QUERY_BATCH_SIZE_LIMIT.into();
            request.to = to.as_deref();

            let events = room.messages(request).await?;

            Ok(MessageQueryResult { events, query })
        }
    }

    pub fn update(&mut self, query_result: MessageQueryResult) {
        let batch = query_result.events;
        let msgs = batch.chunk;
        let num_events = msgs.len() + batch.state.len();
        match query_result.query {
            MessageQuery::AfterCache => {
                for msg in transform_events(msgs.into_iter().map(|e| e.into())) {
                    self.append(msg);
                }

                self.end_token = Some(batch.end.unwrap());
                self.end = if num_events < QUERY_BATCH_SIZE_LIMIT as usize {
                    CacheEndState::Reached
                } else {
                    CacheEndState::Open
                };
            }
            MessageQuery::BeforeCache => {
                for msg in transform_events(msgs.into_iter().map(|e| e.into())) {
                    self.prepend(msg);
                }

                self.begin_token = Some(batch.end.unwrap());
                self.begin = if num_events < QUERY_BATCH_SIZE_LIMIT as usize {
                    CacheEndState::Reached
                } else {
                    CacheEndState::Open
                };
            }
            MessageQuery::Newest => {
                if num_events >= QUERY_BATCH_SIZE_LIMIT as usize {
                    // We fetch from the latest sync token backwards, possibly up to the cache end.
                    // We cannot compare sync tokens, so the only thing we know here, is that we
                    // DON'T have to invalidate the cache, if we get less than
                    // QUERY_BATCH_SIZE_LIMIT. In all other cases we just have to assume that the
                    // cache is invalid now. :(
                    self.begin_token = Some(batch.end.unwrap());
                    self.begin = CacheEndState::Open;
                    self.clear_timeline();
                }
                self.end_token = Some(batch.start.unwrap());
                self.end = CacheEndState::Reached;

                for msg in transform_events(msgs.into_iter().rev().map(|e| e.into())) {
                    self.append(msg);
                }
            }
        }
    }

    pub fn handle_sync_batch(
        &mut self,
        batch: matrix_sdk::deserialized_responses::Timeline,
        end_token: &str,
    ) {
        if matches!(self.end, CacheEndState::Reached) {
            let events = batch.events.into_iter();

            if batch.limited {
                self.clear_timeline();
                if let Some(token) = batch.prev_batch {
                    self.begin_token = Some(token);
                    self.begin = CacheEndState::Open;
                } else {
                    self.begin = CacheEndState::Reached;
                }
            }

            self.end_token = Some(end_token.to_owned());
            self.end = CacheEndState::Reached;

            for msg in transform_events(events.into_iter()) {
                self.append(msg);
            }
        }
    }
}

fn transform_events(i: impl Iterator<Item = SyncRoomEvent>) -> impl Iterator<Item = Event> {
    i.filter_map(|msg| match msg.event.deserialize() {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!("Failed to deserialize message {:?}", e);
            None
        }
    })
}

#[derive(Debug, Copy, Clone)]
pub enum MessageQuery {
    BeforeCache,
    AfterCache,
    Newest,
}

pub struct MessageQueryResult {
    query: MessageQuery,
    events: Messages,
}
