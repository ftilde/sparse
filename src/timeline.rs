use crate::search::filter;
use matrix_sdk::ruma::api::client::message::get_message_events;
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::events::room::message::Relation;
use matrix_sdk::ruma::events::room::redaction::SyncRoomRedactionEvent;
use matrix_sdk::ruma::events::{
    AnyMessageLikeEventContent, AnySyncMessageLikeEvent, AnySyncTimelineEvent,
};
use matrix_sdk::{
    room::{Messages, Room},
    ruma::events::reaction::ReactionEventContent,
    ruma::{EventId, OwnedEventId},
    Client,
};
use std::collections::{HashMap, VecDeque};

struct EventSequence {
    index_offset: isize,
    sequence: VecDeque<OwnedEventId>,
    index: HashMap<OwnedEventId, EventSequenceId>,
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

    fn append(&mut self, item: OwnedEventId) {
        let id = self.sequence_index_to_id(self.sequence.len());
        self.index.insert(item.clone(), id);
        self.sequence.push_back(item);
    }
    fn prepend(&mut self, item: OwnedEventId) {
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

pub type Event = AnySyncTimelineEvent;
pub type Reaction = SyncMessageEvent<ReactionEventContent>;
pub type Reactions = HashMap<String, Vec<Reaction>>;

pub enum CacheEndState {
    Open,
    Reached,
}

pub struct RoomTimelineCache {
    full_timeline: EventSequence,
    filtered_timeline: Option<FilteredTimeline>,
    events: HashMap<OwnedEventId, Event>,
    pub begin: CacheEndState,
    pub end: CacheEndState,
    begin_token: Option<String>,
    end_token: Option<String>,
    reactions: HashMap<OwnedEventId, Reactions>,
    reactions_to_target: HashMap<OwnedEventId, OwnedEventId>,
    msg_to_edits: HashMap<OwnedEventId, Vec<Event>>,
    edits_to_original: HashMap<OwnedEventId, OwnedEventId>,
    redactions: HashMap<OwnedEventId, Box<SyncRoomRedactionEvent>>,
    has_undecrypted_messages: bool,
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
            reactions_to_target: HashMap::new(),
            msg_to_edits: HashMap::new(),
            edits_to_original: HashMap::new(),
            redactions: HashMap::new(),
            has_undecrypted_messages: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EventWalkResult<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch,
    End,
}

impl<'a> EventWalkResult<'a> {
    pub fn message(self) -> Option<RoomTimelineIndex<'a>> {
        if let EventWalkResult::Message(m) = self {
            Some(m)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EventWalkResultNewest<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch(Option<RoomTimelineIndex<'a>>), //There may be newer events, but this is the newest we got
    End,
}

impl<'a> EventWalkResultNewest<'a> {
    pub fn message(self) -> Option<RoomTimelineIndex<'a>> {
        if let EventWalkResultNewest::Message(m) = self {
            Some(m)
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

#[derive(Clone, Copy, Debug)]
pub struct RoomTimelineIndex<'a> {
    pos: &'a EventId,
}

impl RoomTimelineIndex<'_> {
    fn new<'a>(pos: &'a EventId) -> RoomTimelineIndex<'a> {
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
        self.reactions_to_target.clear();
        let f = self.filtered_timeline.as_ref().map(|ft| ft.filter.clone());
        self.set_filter(f);
    }

    pub fn clear(&mut self) {
        self.clear_timeline();
        self.begin = CacheEndState::Open;
        self.end = CacheEndState::Open;
        self.begin_token = None;
        self.end_token = None;
        self.has_undecrypted_messages = false;
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

    pub fn has_undecrypted_messages(&self) -> bool {
        self.has_undecrypted_messages
    }

    fn pre_process_message(&mut self, event: Event) -> Option<Event> {
        if let Event::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(_)) = &event {
            self.has_undecrypted_messages = true;
        }
        match event {
            Event::MessageLike(msg) => {
                let eid = msg.event_id();
                let Some(content) = msg.original_content() else {
                    // Ignore all redacted messages (TODO: Do we actually want that??)
                    return None;
                };
                match content {
                    AnyMessageLikeEventContent::Reaction(r) => {
                        self.reactions_to_target
                            .insert(eid.to_owned(), r.relates_to.event_id.clone());
                        self.reactions
                            .entry(r.relates_to.event_id.to_owned())
                            .or_default()
                            .entry(r.relates_to.key.to_owned())
                            .or_default()
                            .push(r);
                        None
                    }
                    AnyMessageLikeEventContent::RoomRedaction(r) => {
                        let id = &r.redacts.unwrap();
                        // TODO: remove reactions
                        if let Some(reaction_target) = self.reactions_to_target.remove(id) {
                            if let Some(reactions) = self.reactions.get_mut(&reaction_target) {
                                for (emoji, reaction_events) in reactions.iter_mut() {
                                    if let Some(i) =
                                        reaction_events.iter().position(|e| *e.event_id == *id)
                                    {
                                        reaction_events.remove(i);
                                    }
                                    if reaction_events.is_empty() {
                                        let emoji = emoji.to_owned();
                                        reactions.remove(&emoji);
                                        break;
                                    }
                                }
                                if reactions.is_empty() {
                                    self.reactions.remove(&reaction_target);
                                }
                            }
                        } else {
                            self.redactions.insert(id.to_owned(), Box::new(r));
                        }
                        None
                    }
                    AnyMessageLikeEventContent::Message(m) => {
                        if let Some(Relation::Replacement(r)) = m.relates_to {
                            self.edits_to_original.insert(eid.into(), r.event_id.into());
                            self.msg_to_edits
                                .entry(r.event_id.into())
                                .or_default()
                                .push(event);
                            None
                        } else {
                            Some(event)
                        }
                    }
                    o => Some(o),
                }
            }
            Event::State(s) => Some(event),
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

                self.end = if num_events < QUERY_BATCH_SIZE_LIMIT as usize {
                    // For some reason the /messages endpoint returns no end token when we reach
                    // the "newest" end of the timeline. For this reason we cannot set an end_token
                    // here. ugh...
                    CacheEndState::Reached
                } else {
                    self.end_token = Some(batch.end.unwrap());
                    CacheEndState::Open
                };
            }
            MessageQuery::BeforeCache => {
                for msg in transform_events(msgs.into_iter().map(|e| e.into())) {
                    self.prepend(msg);
                }

                self.begin = if num_events < QUERY_BATCH_SIZE_LIMIT as usize {
                    self.begin_token = None;
                    CacheEndState::Reached
                } else {
                    self.begin_token = Some(batch.end.unwrap());
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
        if let Some(_e) = self.redactions.get(original.event_id()) {
            TimelineEntry::Deleted(original)
        } else if let Some(e) = self.msg_to_edits.get(original.event_id()) {
            TimelineEntry::Edited {
                original,
                versions: e,
            }
        } else {
            TimelineEntry::Simple(original)
        }
    }

    pub fn message(&self, id: RoomTimelineIndex) -> TimelineEntry {
        let original = self.events.get(id.pos).unwrap();
        self.entry_from_event(original)
    }

    fn find<'a>(&'a self, id: &'a EventId) -> Option<RoomTimelineIndex<'a>> {
        if self.events.get(id).is_some() {
            Some(RoomTimelineIndex::new(id))
        } else {
            None
        }
    }

    pub fn message_from_id(&self, id: &EventId) -> Option<TimelineEntry> {
        let original_id = self.edits_to_original.get(id).map(|v| &**v).unwrap_or(id);
        self.events
            .get(original_id)
            .map(|i| self.entry_from_event(i))
    }

    pub fn walk_from_known<'a>(&'a self, id: &'a EventId) -> EventWalkResult<'a> {
        if let Some(i) = self.find(id) {
            EventWalkResult::Message(i)
        } else {
            EventWalkResult::RequiresFetch
        }
    }

    pub fn walk_from_newest<'a>(&'a self) -> EventWalkResultNewest<'a> {
        let newest_index = if let Some(ft) = &self.filtered_timeline {
            ft.filtered_messages.last()
        } else {
            self.full_timeline.last()
        };
        let newest_index = newest_index.map(|i| RoomTimelineIndex::new(i));
        match self.end {
            CacheEndState::Reached => {
                if let Some(i) = newest_index {
                    EventWalkResultNewest::Message(i)
                } else {
                    match self.begin {
                        CacheEndState::Reached => EventWalkResultNewest::End,
                        CacheEndState::Open => EventWalkResultNewest::RequiresFetch(None),
                    }
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
        let new_pos = new_pos.map(|i| RoomTimelineIndex::new(i));
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
        let new_pos = new_pos.map(|i| RoomTimelineIndex::new(i));
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
                    room.last_prev_batch().unwrap(), //TODO: not sure if this is correct?
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
                    room.last_prev_batch().unwrap(), //TODO: not sure if this is correct?
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
