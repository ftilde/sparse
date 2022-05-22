use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::events::SyncMessageEvent;
use std::fmt::Write;
use unsegen::base::*;
use unsegen::input::{OperationResult, Scrollable};
use unsegen::widget::*;

use crate::timeline::{EventWalkResult, EventWalkResultNewest, MessageQuery, TimelineEntry};
use crate::tui_app::State;

use crate::tui_app::tui::{MessageSelection, Tasks};

use matrix_sdk::{
    self,
    ruma::events::{
        room::message::{MessageType, Relation},
        AnySyncMessageEvent, AnySyncStateEvent,
    },
    ruma::identifiers::{EventId, RoomId},
    ruma::UserId,
};

use super::EventDetail;

macro_rules! message_fetch_symbol {
    () => {
        "[...]"
    };
}
pub const REPLY_PREFIX: &str = "┏➤ ";
pub const EDIT_PREFIX: &str = "Editing: ";

pub struct MessagesMut<'a>(pub &'a mut State);

impl Scrollable for MessagesMut<'_> {
    fn scroll_backwards(&mut self) -> OperationResult {
        let mut current = self.0.current_room_state_mut().ok_or(())?;
        let messages = &current.messages;
        let pos = match &current.tui.selection {
            MessageSelection::Newest => messages.walk_from_newest().message(),
            MessageSelection::Specific(id) => {
                let pos = messages.walk_from_known(&id).message().ok_or(())?;
                messages.previous(pos).message()
            }
        }
        .ok_or(())?;
        current.tui.selection =
            MessageSelection::Specific(messages.message(pos).event_id().to_owned());
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        let mut current = self.0.current_room_state_mut().ok_or(())?;
        let messages = &current.messages;
        let pos = match &current.tui.selection {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(id) => messages.walk_from_known(&id).message(),
        }
        .ok_or(())?;
        current.tui.selection = match messages.next(pos) {
            EventWalkResult::End => MessageSelection::Newest,
            EventWalkResult::Message(pos) => {
                MessageSelection::Specific(messages.message(pos).event_id().to_owned())
            }
            EventWalkResult::RequiresFetch => return Err(()),
        };
        Ok(())
    }

    fn scroll_to_end(&mut self) -> OperationResult {
        let mut current = self.0.current_room_state_mut().ok_or(())?;
        current.tui.selection = match &current.tui.selection {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(_id) => MessageSelection::Newest,
        };
        Ok(())
    }
}

fn detailed(state: &State, selected: bool) -> bool {
    match state.tui.event_detail {
        EventDetail::Full => true,
        EventDetail::Reduced => false,
        EventDetail::Selected => selected,
    }
}

pub struct Messages<'a>(pub &'a State, pub Tasks<'a>);

impl Messages<'_> {
    fn draw_up_from<'b>(
        &self,
        mut window: Window,
        hints: RenderingHints,
        mut msg: EventWalkResult<'b>,
        room: &RoomId,
        state: &'b crate::tui_app::RoomState,
    ) {
        loop {
            msg = match msg {
                EventWalkResult::Message(id) => {
                    let e = state.messages.message(id);
                    let evt = TuiEvent {
                        event: e,
                        width: window.get_width(),
                        room_state: state,
                        detailed: detailed(&self.0, false),
                    };
                    let h = evt.space_demand().height.min;
                    let window_height = window.get_height();
                    let (above, below) = match window.split((window_height - h).from_origin()) {
                        Ok(pair) => pair,
                        Err(_) => {
                            break;
                        }
                    };

                    evt.draw(below, hints);
                    window = above;
                    state.messages.previous(id)
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetch => {
                    let mut c = Cursor::new(&mut window);
                    write!(&mut c, message_fetch_symbol!()).unwrap();
                    self.1
                        .set_message_query(room.to_owned(), MessageQuery::BeforeCache);
                    break;
                }
            };
        }
    }
    fn draw_newest(
        &self,
        mut window: Window,
        hints: RenderingHints,
        room: &RoomId,
        state: &crate::tui_app::RoomState,
    ) {
        let msg_id = match state.messages.walk_from_newest() {
            EventWalkResultNewest::Message(m) => m,
            EventWalkResultNewest::End => return,
            EventWalkResultNewest::RequiresFetch(latest) => {
                if state.messages.reached_newest() {
                    // We have received the latest events, but none that are suitable for display
                    // (e.g. only state updates or message deletions)
                    self.1
                        .set_message_query(room.to_owned(), MessageQuery::BeforeCache);
                } else {
                    self.1
                        .set_message_query(room.to_owned(), MessageQuery::Newest);
                }

                let split = (window.get_height() - 1).from_origin();
                let (above, mut below) = match window.split(split) {
                    Ok((above, below)) => (Some(above), below),
                    Err(below) => (None, below),
                };
                let mut c = Cursor::new(&mut below);
                write!(&mut c, message_fetch_symbol!()).unwrap();

                if let Some(above) = above {
                    window = above;
                } else {
                    return;
                }
                if let Some(latest) = latest {
                    latest
                } else {
                    return;
                }
            }
        };
        self.draw_up_from(window, hints, EventWalkResult::Message(msg_id), room, state);
    }
    fn draw_specific(
        &self,
        window: Window,
        hints: RenderingHints,
        selected_msg: &EventId,
        room: &RoomId,
        state: &crate::tui_app::RoomState,
    ) {
        let start_msg = state.messages.walk_from_known(selected_msg);
        let mut msg = start_msg.clone();
        let mut collected_height = Height::new(0).unwrap();
        let window_height = window.get_height();
        loop {
            match msg {
                EventWalkResult::Message(id) => {
                    let event = state.messages.message(id);
                    let selected = event.event_id() == selected_msg;
                    collected_height += TuiEvent {
                        event,
                        width: window.get_width(),
                        room_state: state,
                        detailed: detailed(self.0, selected),
                    }
                    .space_demand()
                    .height
                    .min;
                    msg = state.messages.next(id);
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetch => {
                    collected_height += Height::new(1).unwrap();
                    break;
                }
            }
            if collected_height > window_height / 2 {
                break;
            }
        }
        let (above_selected, below_selected) =
            match window.split((window_height - collected_height).from_origin()) {
                Ok((above, below)) => (Some(above), below),
                Err(w) => (None, w),
            };
        if let (Some(above), Some(evt)) = (
            above_selected,
            start_msg.message().map(|id| state.messages.previous(id)),
        ) {
            self.draw_up_from(above, hints, evt, room, state);
        }
        let mut window = below_selected;
        let mut msg = start_msg;
        loop {
            msg = match msg {
                EventWalkResult::Message(id) => {
                    let event = state.messages.message(id);
                    let selected = event.event_id() == selected_msg;
                    let evt = TuiEvent {
                        event,
                        width: window.get_width(),
                        room_state: state,
                        detailed: detailed(self.0, selected),
                    };
                    let h = evt.space_demand().height.min;
                    let (mut current, below) = match window.split(h.from_origin()) {
                        Ok(pair) => pair,
                        Err(_) => {
                            break;
                        }
                    };

                    if selected {
                        current.set_default_style(
                            StyleModifier::new().invert(true).apply_to_default(),
                        );
                    }
                    evt.draw(current, hints);
                    window = below;
                    state.messages.next(id)
                }
                EventWalkResult::End => {
                    break;
                }
                EventWalkResult::RequiresFetch => {
                    let mut c = Cursor::new(&mut window);
                    write!(&mut c, message_fetch_symbol!()).unwrap();
                    // The normal assumption is that the new messages are below the current cache
                    // (we are drawing from top to bottom), but if we have reached the "new-end" of
                    // the timeline, this means that the messages we are searching for are actually
                    // before the cache.
                    let query = if state.messages.reached_newest() {
                        MessageQuery::BeforeCache
                    } else {
                        MessageQuery::AfterCache
                    };
                    self.1.set_message_query(room.to_owned(), query);
                    break;
                }
            };
        }
    }
}

impl Widget for Messages<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        Demand2D {
            width: ColDemand::at_least(Width::new(0).unwrap()),
            height: RowDemand::at_least(Height::new(0).unwrap()),
        }
    }

    fn draw(&self, window: Window, hints: RenderingHints) {
        if let Some(current) = self.0.current_room_state().as_ref() {
            match &current.tui.selection {
                MessageSelection::Newest => self.draw_newest(window, hints, &current.id, current),
                MessageSelection::Specific(id) => {
                    self.draw_specific(window, hints, id, &current.id, current)
                }
            }
        }
    }
}

struct StyledLine {
    content: Vec<StyledGraphemeCluster>,
    style: Style,
}

impl StyledLine {
    fn new(w: Width, style: Style) -> Self {
        StyledLine {
            content: vec![
                StyledGraphemeCluster::new(GraphemeCluster::space(), Style::plain());
                w.raw_value() as _
            ],
            style,
        }
    }
}
impl CursorTarget for StyledLine {
    fn get_width(&self) -> Width {
        Width::new(self.content.len() as i32).unwrap()
    }
    fn get_height(&self) -> Height {
        Height::new(1).unwrap()
    }
    fn get_cell_mut(&mut self, x: ColIndex, y: RowIndex) -> Option<&mut StyledGraphemeCluster> {
        if x < 0 || y != 0 {
            return None;
        }
        self.content.get_mut(x.raw_value() as usize)
    }
    fn get_cell(&self, x: ColIndex, y: RowIndex) -> Option<&StyledGraphemeCluster> {
        if x < 0 || y != 0 {
            return None;
        }
        self.content.get(x.raw_value() as usize)
    }
    fn get_default_style(&self) -> Style {
        self.style
    }
}

struct TuiEvent<'a> {
    event: crate::timeline::TimelineEntry<'a>,
    width: Width,
    room_state: &'a crate::tui_app::RoomState,
    detailed: bool,
}

pub fn strip_body(mut body: &str) -> &str {
    while let [b'>', b' ', ..] = body.as_bytes() {
        if let Some(e) = body.find('\n') {
            body = &body[e + 1..];
        } else {
            return "";
        }
    }
    if let [b'\n', ..] = body.as_bytes() {
        body = &body[1..];
    }
    body
}

fn write_user<T: unsegen::base::CursorTarget>(
    c: &mut Cursor<T>,
    user_id: &UserId,
    state: &crate::tui_app::RoomState,
) {
    // The user color map is automatically updated when users join a room. Howevery, as this
    // happens asyncronously, rendering of new events may happen beforehand. Hence we need a
    // (temporary in any case) fallback here.
    let color = state.user_colors.get(user_id).unwrap_or(&Color::Default);
    let mut c = c.save().style_modifier();
    c.set_style_modifier(StyleModifier::new().fg_color(*color).bold(true));
    let _ = write!(c, "{}", user_id.as_str());
}

pub fn draw_event_preview<T: unsegen::base::CursorTarget, D: DrawEvent>(
    prefix: &str,
    event: &D,
    room_state: &crate::tui_app::RoomState,
    target: &mut T,
) {
    let w = target.get_width();
    let mut c = Cursor::<T>::new(target);
    c.write(prefix);
    event.draw(room_state, &mut c, true);
    if c.get_row() != 0 || c.get_col() >= w.from_origin() {
        c = c.position((w - 3).from_origin(), AxisIndex::new(0));
        c.write("...");
    }
}

pub trait DrawEvent {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    );
}

impl DrawEvent for SyncMessageEvent<RoomMessageEventContent> {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    ) {
        if !simplified {
            if let Some(crate::timeline::Event::Message(AnySyncMessageEvent::RoomMessage(m))) =
                room_state.messages.original_message(&self.event_id)
            {
                if let Some(Relation::Reply { in_reply_to: rel }) = &m.content.relates_to {
                    if let Some(rel) = room_state.messages.message_from_id(&rel.event_id) {
                        let mut l =
                            StyledLine::new(c.target().get_width(), c.target().get_default_style());
                        draw_event_preview(REPLY_PREFIX, &rel, room_state, &mut l);
                        c.write_preformatted(l.content.as_slice());
                        c.wrap_line();
                    }
                }
            }
        }
        write_user(c, &self.sender, room_state);
        c.set_wrapping_mode(WrappingMode::Wrap);
        match &self.content.msgtype {
            MessageType::Text(text) => {
                let _ = write!(c, ": ");
                let start = c.get_col();
                c.set_line_start_column(start);
                c.write(strip_body(&text.body));
            }
            MessageType::Image(img) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " sent an image ({})", img.body);
            }
            MessageType::Video(v) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " sent a video ({})", v.body);
            }
            MessageType::Audio(a) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " sent an audio message ({})", a.body);
            }
            MessageType::File(f) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " sent a file ({})", f.body);
            }
            MessageType::Emote(e) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " {}", e.body);
            }
            MessageType::Location(e) => {
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, " sends the location {} ({})", e.body, e.geo_uri);
            }
            MessageType::Notice(n) => {
                let _ = write!(c, ": ");
                c.set_style_modifier(StyleModifier::new().italic(true));
                let start = c.get_col();
                c.set_line_start_column(start);
                let _ = write!(c, "{}", &n.body);
            }
            MessageType::ServerNotice(n) => {
                let _ = write!(c, ": ");
                c.set_style_modifier(StyleModifier::new().italic(true));
                let start = c.get_col();
                c.set_line_start_column(start);
                let _ = write!(c, "{} [server notice]", &n.body);
            }
            MessageType::VerificationRequest(_r) => {
                let _ = write!(c, " sent a verification request.");
            }
            MessageType::_Custom(e) => {
                let _ = write!(c, " sent a custom event: ");
                c.set_style_modifier(StyleModifier::new().italic(true));
                let start = c.get_col();
                c.set_line_start_column(start);
                let _ = write!(c, "{:?}", e);
            }
            o => {
                c.set_wrapping_mode(WrappingMode::Wrap);
                let _ = write!(c, "Other message {:?}", o);
            }
        }
    }
}

impl DrawEvent for AnySyncMessageEvent {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    ) {
        match self {
                AnySyncMessageEvent::RoomMessage(msg) => {
                    msg.draw(room_state, c, simplified)
                }
                AnySyncMessageEvent::RoomEncrypted(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("*Unable to decrypt message from ");
                    write_user(c, &msg.sender, room_state);
                    c.write("*");
                }
                AnySyncMessageEvent::CallAnswer(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("Call answer from ");
                    write_user(c, &msg.sender, room_state);
                    c.write(".");
                }
                AnySyncMessageEvent::CallInvite(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("Call invite from ");
                    write_user(c, &msg.sender, room_state);
                    c.write(".");
                }
                AnySyncMessageEvent::CallHangup(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("Call hangup from ");
                    write_user(c, &msg.sender, room_state);
                    c.write(".");
                }
                AnySyncMessageEvent::CallCandidates(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("Call candidates from ");
                    write_user(c, &msg.sender, room_state);
                    c.write(".");
                }
                AnySyncMessageEvent::KeyVerificationStart(msg) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    c.write("Ignoring verification start message from ");
                    write_user(c, &msg.sender, room_state);
                    c.write(".");
                }
                AnySyncMessageEvent::KeyVerificationReady(_) // Intentionally ignored
                | AnySyncMessageEvent::KeyVerificationCancel(_)
                | AnySyncMessageEvent::KeyVerificationAccept(_)
                | AnySyncMessageEvent::KeyVerificationKey(_)
                | AnySyncMessageEvent::KeyVerificationMac(_)
                | AnySyncMessageEvent::KeyVerificationDone(_) => {}
                AnySyncMessageEvent::Reaction(_) => {
                    panic!("Reactions should be filtered from the timeline")
                }
                AnySyncMessageEvent::RoomMessageFeedback(_) => {
                    //Ignored
                }
                AnySyncMessageEvent::RoomRedaction(e) => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    let _ = write!(c, "Room Redaction {:?}", e);
                }
                AnySyncMessageEvent::Sticker(msg) => {
                    write_user(c, &msg.sender, room_state);
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    let _ = write!(c, " sent a sticker ({})", msg.content.body);
                }
                AnySyncMessageEvent::_Custom(e) => {
                    let _ = write!(c, " sent a message event: ");
                    c.set_style_modifier(StyleModifier::new().italic(true));
                    let start = c.get_col();
                    c.set_line_start_column(start);
                    let _ = write!(c, "{:?}", e);
                }
                o => {
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    let _ = write!(c, "Other message event {:?}", o);
                }
            }
    }
}

impl DrawEvent for AnySyncStateEvent {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        _simplified: bool,
    ) {
        c.set_wrapping_mode(WrappingMode::Wrap);
        c.set_style_modifier(StyleModifier::new().italic(true));
        match self {
            //Not sure what to do with these...
            //AnySyncStateEvent::PolicyRuleRoom(_) => todo!(),
            //AnySyncStateEvent::PolicyRuleServer(_) => todo!(),
            //AnySyncStateEvent::PolicyRuleUser(_) => todo!(),
            //AnySyncStateEvent::RoomTombstone(_) => todo!(),
            //AnySyncStateEvent::RoomPowerLevels(_) => todo!(),
            //AnySyncStateEvent::RoomServerAcl(_) => todo!(),
            //AnySyncStateEvent::SpaceChild(_) => todo!(),
            //AnySyncStateEvent::SpaceParent(_) => todo!(),
            //AnySyncStateEvent::_Custom(_) => todo!(),
            AnySyncStateEvent::RoomCanonicalAlias(e) => {
                write_user(c, &e.sender, room_state);
                if let Some(a) = &e.content.alias {
                    let _ = write!(c, " changed the canonical room alias to {}.", a.as_str());
                } else {
                    let _ = write!(c, " has removed the room alias.",);
                }
            }
            AnySyncStateEvent::RoomCreate(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(c, " created the room.");
            }
            AnySyncStateEvent::RoomAliases(_) | AnySyncStateEvent::RoomAvatar(_) => {}
            AnySyncStateEvent::RoomEncryption(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(c, " enabled encryption.");
            }
            AnySyncStateEvent::RoomGuestAccess(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(
                    c,
                    " changed the guest access to {}.",
                    e.content.guest_access.as_str()
                );
            }
            AnySyncStateEvent::RoomHistoryVisibility(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(
                    c,
                    " changed the history visibility to {}.",
                    e.content.history_visibility.as_str()
                );
            }
            AnySyncStateEvent::RoomJoinRules(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(
                    c,
                    " changed the join rules to {}.",
                    e.content.join_rule.as_str()
                );
            }
            AnySyncStateEvent::RoomMember(e) => {
                use matrix_sdk::ruma::events::room::member::MembershipChange::*;
                let s = match e.membership_change() {
                    None => Option::None,
                    Error => panic!("Membership change is 'Error'"),
                    Joined => Some(" joined."),
                    Left => Some(" left."),
                    Banned => Some(" was banned."),
                    Unbanned => Some(" was unbanned."),
                    Kicked => Some(" was kicked."),
                    Invited => Some(" was invited."),
                    KickedAndBanned => Some(" was kicked and banned."),
                    InvitationRejected => Some(" rejected the invitation."),
                    ProfileChanged {
                        displayname_changed: true,
                        avatar_url_changed: _,
                    } => Some(" changed their displayname."),
                    NotImplemented => Option::None,
                    InvitationRevoked => Some("'s invitation was revoked."),
                    _o => Some(" had another state change"),
                };
                if let Some(s) = s {
                    if let Some(u) = &e.content.displayname {
                        let _ = write!(c, "{}", u);
                    } else {
                        let _ = write!(c, "Unknown user");
                    }
                    let _ = write!(c, "{}", s);
                }
            }
            AnySyncStateEvent::RoomName(e) => {
                write_user(c, &e.sender, room_state);
                if let Some(a) = &e.content.name {
                    let _ = write!(c, " changed the room name to '{}'.", a.as_str());
                } else {
                    let _ = write!(c, " has unset the room name.",);
                }
            }
            AnySyncStateEvent::RoomPinnedEvents(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(
                    c,
                    " has pinned the following events {:?}.",
                    e.content.pinned
                );
                //TODO: We will have to see how to display this properly
            }
            AnySyncStateEvent::RoomThirdPartyInvite(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(c, " has invited {}.", e.content.display_name);
            }
            AnySyncStateEvent::RoomTopic(e) => {
                write_user(c, &e.sender, room_state);
                let _ = write!(c, " has changed the topic to '{}'.", e.content.topic);
            }
            o => {
                let _ = write!(c, "Other state event {:?}", o);
            }
        }
    }
}

impl DrawEvent for crate::tui_app::timeline::Event {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    ) {
        let mut c = c.save().style_modifier();
        match self {
            crate::timeline::Event::Message(e) => e.draw(room_state, &mut c, simplified),
            crate::timeline::Event::State(e) => e.draw(room_state, &mut c, simplified),
            o => {
                c.set_wrapping_mode(WrappingMode::Wrap);
                let _ = write!(c, "Other event {:?}", o);
            }
        }
    }
}

struct Detailed<'a>(TimelineEntry<'a>);

impl DrawEvent for Detailed<'_> {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    ) {
        match self.0 {
            TimelineEntry::Simple(m) => {
                m.draw(room_state, c, simplified);
            }
            TimelineEntry::Deleted(m) => {
                m.draw(room_state, c, simplified);
                let mut c = c.save().style_modifier();
                c.set_style_modifier(StyleModifier::new().italic(true));
                c.write(" (deleted)");
            }
            TimelineEntry::Edited { original, versions } => {
                {
                    let mut c = c.save().style_modifier().line_start_column();
                    original.draw(room_state, &mut c, simplified);
                }
                for r in versions {
                    let mut c = c.save().style_modifier().line_start_column();
                    c.wrap_line();
                    write_time(&mut c, &r);
                    r.draw(room_state, &mut c, true);
                }
            }
        }
    }
}

impl DrawEvent for TimelineEntry<'_> {
    fn draw<T: unsegen::base::CursorTarget>(
        &self,
        room_state: &crate::tui_app::RoomState,
        c: &mut Cursor<T>,
        simplified: bool,
    ) {
        match self {
            TimelineEntry::Simple(m) => {
                m.draw(room_state, c, simplified);
            }
            TimelineEntry::Deleted(m) => {
                let mut c = c.save().style_modifier();
                write_user(&mut c, &m.sender(), room_state);
                c.set_style_modifier(StyleModifier::new().italic(true));
                c.write(" deleted message");
            }
            TimelineEntry::Edited { versions, .. } => {
                versions.last().unwrap().draw(room_state, c, simplified);
                let mut c = c.save().style_modifier();
                c.set_style_modifier(StyleModifier::new().italic(true));
                c.write(" (edited)");
            }
        }
    }
}

fn write_time<T: unsegen::base::CursorTarget>(c: &mut Cursor<T>, event: &crate::timeline::Event) {
    use chrono::TimeZone;
    let send_time_secs_unix = event.origin_server_ts().as_secs();
    let send_time_naive =
        chrono::naive::NaiveDateTime::from_timestamp(send_time_secs_unix.into(), 0);
    let send_time = chrono::Local.from_utc_datetime(&send_time_naive);
    let time_str = send_time.format("%m-%d %H:%M");
    let _ = write!(c, "{} ", time_str);
}

impl TuiEvent<'_> {
    fn draw_with_cursor<T: unsegen::base::CursorTarget>(&self, c: &mut Cursor<T>) {
        write_time(c, self.event.original());

        let start = c.get_col();
        c.set_line_start_column(start);

        if self.detailed {
            Detailed(self.event).draw(self.room_state, c, false);
        } else {
            self.event.draw(self.room_state, c, false);
        }

        if let Some(reactions) = self.room_state.messages.reactions(self.event.event_id()) {
            {
                let mut c = c.save().style_modifier();
                c.set_style_modifier(StyleModifier::new().italic(true));
                let _ = write!(c, "\nReactions: ");
            }
            for (emoji, events) in reactions {
                if self.detailed {
                    let _ = write!(c, "\n");
                    for e in events {
                        write_user(c, &e.sender, self.room_state);
                        let _ = write!(c, " ");
                    }
                    let _ = write!(c, "{}", emoji);
                } else {
                    let _ = write!(c, "{}", emoji);
                    let n = events.len();
                    if n > 1 {
                        let mut c = c.save().style_modifier();
                        c.set_style_modifier(StyleModifier::new().italic(true));
                        let _ = write!(c, " {}", n);
                    }
                    let _ = write!(c, "  ");
                }
            }
        }
    }
}

impl Widget for TuiEvent<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let mut est = unsegen::base::window::ExtentEstimationWindow::with_width(self.width);
        let mut c = Cursor::new(&mut est);
        self.draw_with_cursor(&mut c);
        Demand2D {
            width: ColDemand::exact(est.extent_x()),
            height: RowDemand::exact(est.extent_y()),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        // Apply initial background style to whole window
        window.clear();

        let mut c = Cursor::new(&mut window);
        self.draw_with_cursor(&mut c);
    }
}
