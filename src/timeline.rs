use matrix_sdk::ruma::api::client::r0::message::get_message_events::{self, Direction};
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
    messages: VecDeque<Event>,
    pub begin: CacheEndState,
    pub end: CacheEndState,
    begin_token: Option<String>,
    end_token: Option<String>,
    reactions: HashMap<Box<EventId>, Reactions>,
}

impl std::default::Default for RoomTimelineCache {
    fn default() -> Self {
        RoomTimelineCache {
            messages: VecDeque::new(),
            begin: CacheEndState::Open,
            end: CacheEndState::Open,
            begin_token: None,
            end_token: None,
            reactions: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum EventWalkResult<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch,
    End,
}

impl<'a> EventWalkResult<'a> {
    pub fn message(&self) -> Option<RoomTimelineIndex<'a>> {
        if let EventWalkResult::Message(m) = self {
            Some(*m)
        } else {
            None
        }
    }
}

#[derive(Debug)]
pub enum EventWalkResultNewest<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch(Option<RoomTimelineIndex<'a>>), //There may be newer events, but this is the newest we got
    End,
}

impl<'a> EventWalkResultNewest<'a> {
    pub fn message(&self) -> Option<RoomTimelineIndex<'a>> {
        if let EventWalkResultNewest::Message(m) = self {
            Some(*m)
        } else {
            None
        }
    }
}

const QUERY_BATCH_SIZE_LIMIT: u32 = 10;

#[derive(Copy, Clone, Debug)]
pub struct RoomTimelineIndex<'a> {
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
    pub fn end_id(&self) -> Option<&EventId> {
        self.messages.back().map(|e| e.event_id())
    }

    pub fn reactions(&self, id: &EventId) -> Option<&Reactions> {
        self.reactions.get(id)
    }

    fn clear(&mut self) {
        self.messages.clear();
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
            o => Some(o),
        }
    }

    fn append(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            self.messages.push_back(msg)
        }
    }
    fn prepend(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            self.messages.push_front(msg)
        }
    }

    pub fn message(&self, id: RoomTimelineIndex) -> &Event {
        &self.messages[id.pos]
    }

    fn find(&self, id: &EventId) -> Option<(usize, &Event)> {
        // TODO: We might want to store an index to speed this operation up if it's too slow
        self.messages
            .iter()
            .enumerate()
            .find(|(_, m)| *m.event_id() == *id)
    }

    pub fn message_from_id(&self, id: &EventId) -> Option<&Event> {
        self.find(id).map(|(_, e)| e)
    }

    pub fn walk_from_known(&self, id: &EventId) -> EventWalkResult {
        if let Some((i, _)) = self.find(id) {
            EventWalkResult::Message(RoomTimelineIndex::new(i))
        } else {
            EventWalkResult::RequiresFetch
        }
    }

    pub fn walk_from_newest(&self) -> EventWalkResultNewest {
        match self.end {
            CacheEndState::Reached => {
                if !self.messages.is_empty() {
                    EventWalkResultNewest::Message(RoomTimelineIndex::new(self.messages.len() - 1))
                } else {
                    EventWalkResultNewest::End
                }
            }
            CacheEndState::Open => EventWalkResultNewest::RequiresFetch(
                self.messages
                    .len()
                    .checked_sub(1)
                    .map(RoomTimelineIndex::new),
            ),
        }
    }

    pub fn next<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
        let new_pos = pos.pos + 1;
        if new_pos < self.messages.len() {
            EventWalkResult::Message(RoomTimelineIndex::new(new_pos))
        } else {
            match self.end {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => EventWalkResult::RequiresFetch,
            }
        }
    }
    pub fn previous<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
        if pos.pos > 0 {
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
                    self.clear();
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
                self.clear();
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
