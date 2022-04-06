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

#[derive(Clone)]
pub struct Filter {
    pub sender_content: String,
}

impl Filter {
    fn matches(&self, event: &Event) -> bool {
        event.sender().as_str().contains(&self.sender_content)
    }
}

struct EventSequence {
    index_offset: isize,
    sequence: VecDeque<Box<EventId>>,
    index: HashMap<Box<EventId>, EventSequenceId>,
}

#[derive(Copy, Clone)]
pub struct EventSequenceId {
    pos: isize,
}

impl EventSequence {
    fn empty() -> Self {
        EventSequence {
            sequence: VecDeque::new(),
            index: HashMap::new(),
            index_offset: 0,
        }
    }

    fn sequence_index_to_id(&self, id: usize) -> EventSequenceId {
        let pos: isize = id as isize - self.index_offset;
        EventSequenceId { pos }
    }

    fn id_to_sequence_index(&self, id: EventSequenceId) -> usize {
        let index = id.pos + self.index_offset;
        assert!(index >= 0);
        index as usize
    }

    fn append(&mut self, item: Box<EventId>) {
        let id = self.sequence_index_to_id(self.sequence.len());
        self.index.insert(item.clone(), id);
        self.sequence.push_back(item);
    }
    fn prepend(&mut self, item: Box<EventId>) {
        self.index_offset += 1;

        let id = self.sequence_index_to_id(0);
        self.index.insert(item.clone(), id);
        self.sequence.push_front(item)
    }

    fn event(&self, id: EventSequenceId) -> &EventId {
        let i = self.id_to_sequence_index(id);
        &self.sequence[i]
    }

    fn id(&self, value: &EventId) -> Option<EventSequenceId> {
        self.index.get(value).cloned()
    }

    fn next(&self, e: &EventId) -> Option<&EventId> {
        let id = self.id(e)?;
        let next = self.id_to_sequence_index(id) + 1;
        if next < self.sequence.len() {
            Some(self.event(self.sequence_index_to_id(next)))
        } else {
            None
        }
    }

    fn prev(&self, e: &EventId) -> Option<&EventId> {
        let id = self.id(e)?;
        let current = self.id_to_sequence_index(id);
        if current > 0 {
            let prev = current - 1;
            Some(self.event(self.sequence_index_to_id(prev)))
        } else {
            None
        }
    }

    fn last(&self) -> Option<&EventId> {
        self.sequence.back().map(|e| &**e)
    }
}

#[derive(Clone)]
struct FilterId(isize);

impl From<isize> for FilterId {
    fn from(i: isize) -> Self {
        FilterId(i)
    }
}
impl Into<isize> for FilterId {
    fn into(self) -> isize {
        self.0
    }
}

struct FilteredTimeline {
    filter: Filter,
    filtered_messages: EventSequence,
}

impl FilteredTimeline {
    fn try_append(&mut self, event: &Event) {
        if self.filter.matches(event) {
            self.filtered_messages.append(event.event_id().to_owned());
        }
    }
    fn try_prepend(&mut self, event: &Event) {
        if self.filter.matches(event) {
            self.filtered_messages.prepend(event.event_id().to_owned());
        }
    }
}

pub type Event = AnySyncRoomEvent;
pub type Reaction = SyncMessageEvent<ReactionEventContent>;
pub type Reactions = HashMap<String, Vec<Reaction>>;

pub enum CacheEndState {
    Open,
    Reached,
}

pub struct RoomTimelineCache {
    full_timeline: EventSequence,
    filtered_timeline: Option<FilteredTimeline>,
    events: HashMap<Box<EventId>, Event>,
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
            filtered_timeline: None,
            full_timeline: EventSequence::empty(),
            events: HashMap::new(),
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
            Some(m.clone())
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
            Some(m.clone())
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

#[derive(Clone, Debug)]
pub struct RoomTimelineIndex {
    pos: Box<EventId>,
}

impl RoomTimelineIndex {
    fn new(pos: Box<EventId>) -> Self {
        RoomTimelineIndex { pos }
    }
}

impl RoomTimelineCache {
    pub fn end_id(&self) -> Option<&EventId> {
        self.full_timeline.last()
    }

    pub fn reactions(&self, id: &EventId) -> Option<&Reactions> {
        self.reactions.get(id)
    }

    fn clear_timeline(&mut self) {
        self.events.clear();
        self.full_timeline = EventSequence::empty();
        self.msg_to_edits.clear();
        self.edits_to_original.clear();
        self.reactions.clear();
        let f = self.filtered_timeline.as_ref().map(|ft| ft.filter.clone());
        self.set_filter(f);
    }

    pub fn clear(&mut self) {
        self.clear_timeline();
        self.begin = CacheEndState::Open;
        self.end = CacheEndState::Open;
        self.begin_token = None;
        self.end_token = None;
    }

    pub fn set_filter(&mut self, filter: Option<Filter>) {
        if let Some(filter) = filter {
            let mut ft = FilteredTimeline {
                filtered_messages: EventSequence::empty(),
                filter,
            };
            for eid in &self.full_timeline.sequence {
                let m = self.events.get(eid).unwrap();
                ft.try_append(&m);
            }
            self.filtered_timeline = Some(ft);
        } else {
            self.filtered_timeline = None;
        }
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

    fn append(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            let event_id = msg.event_id().to_owned();
            self.full_timeline.append(event_id.clone());
            if let Some(f) = &mut self.filtered_timeline {
                f.try_append(&msg);
            }
            self.events.insert(event_id, msg);
        }
    }
    fn prepend(&mut self, msg: Event) {
        if let Some(msg) = self.pre_process_message(msg) {
            let event_id = msg.event_id().to_owned();
            self.full_timeline.prepend(event_id.clone());
            if let Some(f) = &mut self.filtered_timeline {
                f.try_prepend(&msg);
            }
            self.events.insert(event_id, msg);
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

    pub fn message(&self, id: RoomTimelineIndex) -> TimelineEntry {
        let original = self.events.get(&id.pos).unwrap();
        self.entry_from_event(original)
    }

    fn find(&self, id: &EventId) -> Option<RoomTimelineIndex> {
        if self.events.get(id).is_some() {
            Some(RoomTimelineIndex::new(id.to_owned()))
        } else {
            None
        }
    }

    pub fn original_message(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
    }

    pub fn message_from_id(&self, id: &EventId) -> Option<TimelineEntry> {
        self.events.get(id).map(|i| self.entry_from_event(i))
    }

    pub fn walk_from_known(&self, id: &EventId) -> EventWalkResult {
        if let Some(i) = self.find(id) {
            EventWalkResult::Message(i)
        } else {
            EventWalkResult::RequiresFetch
        }
    }

    pub fn walk_from_newest(&self) -> EventWalkResultNewest {
        let newest_index = if let Some(ft) = &self.filtered_timeline {
            ft.filtered_messages.last()
        } else {
            self.full_timeline.last()
        };
        let newest_index = newest_index.map(|i| RoomTimelineIndex::new(i.to_owned()));
        match self.end {
            CacheEndState::Reached => {
                if let Some(i) = newest_index {
                    EventWalkResultNewest::Message(i)
                } else {
                    EventWalkResultNewest::End
                }
            }
            CacheEndState::Open => EventWalkResultNewest::RequiresFetch(newest_index),
        }
    }

    pub fn next<'a>(&'a self, pos: RoomTimelineIndex) -> EventWalkResult {
        let new_pos = if let Some(ft) = &self.filtered_timeline {
            ft.filtered_messages.next(&pos.pos)
        } else {
            self.full_timeline.next(&pos.pos)
        };
        let new_pos = new_pos.map(|i| RoomTimelineIndex::new(i.to_owned()));
        if let Some(new_pos) = new_pos {
            EventWalkResult::Message(new_pos)
        } else {
            match self.end {
                CacheEndState::Reached => EventWalkResult::End,
                CacheEndState::Open => EventWalkResult::RequiresFetch,
            }
        }
    }
    pub fn previous<'a>(&'a self, pos: RoomTimelineIndex) -> EventWalkResult {
        let new_pos = if let Some(ft) = &self.filtered_timeline {
            ft.filtered_messages.prev(&pos.pos)
        } else {
            self.full_timeline.prev(&pos.pos)
        };
        let new_pos = new_pos.map(|i| RoomTimelineIndex::new(i.to_owned()));
        if let Some(new_pos) = new_pos {
            EventWalkResult::Message(new_pos)
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

    pub fn reached_newest(&self) -> bool {
        matches!(self.end, CacheEndState::Reached)
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
