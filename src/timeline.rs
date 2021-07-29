use matrix_sdk::ruma::api::client::r0::message::get_message_events::Direction;
use matrix_sdk::{
    deserialized_responses::SyncRoomEvent, room::Room, ruma::events::AnySyncRoomEvent,
    ruma::identifiers::EventId,
};
use std::collections::VecDeque;

pub type Event = AnySyncRoomEvent;

pub enum CacheEndState {
    Open,
    Reached,
}

pub struct RoomTimelineCache {
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
pub enum EventWalkResult<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetchFrom(EventId),
    End,
}

#[derive(Debug)]
pub enum EventWalkResultNewest<'a> {
    Message(RoomTimelineIndex<'a>),
    RequiresFetch,
    End,
}

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

    pub fn notify_new_messages(&mut self) {
        self.end = CacheEndState::Open;
    }

    pub fn message(&self, id: RoomTimelineIndex) -> &Event {
        &self.messages[id.pos]
    }

    pub fn walk_from_known(&self, id: &EventId) -> EventWalkResult {
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

    pub fn walk_from_newest(&self) -> EventWalkResultNewest {
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

    pub fn next<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
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
    pub fn previous<'a>(&'a self, pos: RoomTimelineIndex<'a>) -> EventWalkResult<'a> {
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

    pub fn events_query(
        &self,
        room: Room,
        query: MessageQuery,
    ) -> impl std::future::Future<Output = matrix_sdk::Result<MessageQueryResult>> {
        let (direction, start, end) = {
            match &query {
                MessageQuery::AfterCache => (Direction::Forward, self.end().cloned(), None),
                MessageQuery::BeforeCache => (Direction::Backward, self.begin().cloned(), None),
                MessageQuery::Newest => (Direction::Backward, None, self.end().cloned()),
            }
        };

        async move {
            let events = room
                .messages(start.as_ref(), end.as_ref(), 10, direction)
                .await?;

            Ok(MessageQueryResult { events, query })
        }
    }

    pub fn update(&mut self, query_result: MessageQueryResult) {
        fn transform_events(i: impl Iterator<Item = SyncRoomEvent>) -> impl Iterator<Item = Event> {
            i.filter_map(|msg| match msg.event.deserialize() {
                Ok(e) => Some(e),
                Err(e) => {
                    tracing::warn!("Failed to deserialize message {:?}", e);
                    None
                }
            })
        }

        if let Some(msgs) = query_result.events {
            match query_result.query {
                MessageQuery::AfterCache => {
                    for msg in transform_events(msgs.into_iter()) {
                        self.append(msg);
                    }
                }
                MessageQuery::BeforeCache => {
                    let mut iter = transform_events(msgs.into_iter());
                    if let Some(e) = iter.next() {
                        if self.begin() != Some(e.event_id()) {
                            self.clear();
                            self.prepend(e);
                        }
                    }
                    for msg in iter {
                        self.prepend(msg);
                    }
                }
                MessageQuery::Newest => {
                    let mut iter = transform_events(msgs.into_iter().rev());
                    if let Some(e) = iter.next() {
                        if self.end() != Some(e.event_id()) {
                            self.clear();
                            self.append(e);
                        }
                    }
                    for msg in iter {
                        self.append(msg);
                    }
                    self.end = CacheEndState::Reached;
                }
            }
        } else {
            match query_result.query {
                MessageQuery::AfterCache => {
                    self.end = CacheEndState::Reached;
                }
                MessageQuery::BeforeCache => {
                    self.begin = CacheEndState::Reached;
                }
                MessageQuery::Newest => {
                    self.end = CacheEndState::Reached;
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum MessageQuery {
    BeforeCache,
    AfterCache,
    Newest,
}

pub struct MessageQueryResult {
    query: MessageQuery,
    events: Option<Vec<SyncRoomEvent>>,
}
