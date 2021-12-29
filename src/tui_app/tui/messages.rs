use std::fmt::Write;
use unsegen::base::*;
use unsegen::input::{OperationResult, Scrollable};
use unsegen::widget::*;

use crate::timeline::{EventWalkResult, EventWalkResultNewest, MessageQuery};
use crate::tui_app::State;

use crate::tui_app::tui::{MessageSelection, Tasks, TuiState};

use matrix_sdk::{
    self,
    ruma::events::{room::message::MessageType, AnySyncMessageEvent},
    ruma::identifiers::{EventId, RoomId},
    ruma::UserId,
};

macro_rules! message_fetch_symbol {
    () => {
        "[...]"
    };
}

pub struct MessagesMut<'a>(pub &'a State, pub &'a mut TuiState);

impl Scrollable for MessagesMut<'_> {
    fn scroll_backwards(&mut self) -> OperationResult {
        let mut current = self.1.current_room_state_mut().ok_or(())?;
        let messages = &self.0.rooms.get(&current.id).ok_or(())?.messages;
        let pos = match &current.selection {
            MessageSelection::Newest => messages.walk_from_newest().message(),
            MessageSelection::Specific(id) => {
                let pos = messages.walk_from_known(&id).message().ok_or(())?;
                messages.previous(pos).message()
            }
        }
        .ok_or(())?;
        current.selection = MessageSelection::Specific(messages.message(pos).event_id().to_owned());
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        let mut current = self.1.current_room_state_mut().ok_or(())?;
        let messages = &self.0.rooms.get(&current.id).ok_or(())?.messages;
        let pos = match &current.selection {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(id) => messages.walk_from_known(&id).message(),
        }
        .ok_or(())?;
        current.selection = match messages.next(pos) {
            EventWalkResult::End => MessageSelection::Newest,
            EventWalkResult::Message(pos) => {
                MessageSelection::Specific(messages.message(pos).event_id().to_owned())
            }
            EventWalkResult::RequiresFetch => return Err(()),
        };
        Ok(())
    }

    fn scroll_to_end(&mut self) -> OperationResult {
        let mut current = self.1.current_room_state_mut().ok_or(())?;
        current.selection = match &current.selection {
            MessageSelection::Newest => return Err(()),
            MessageSelection::Specific(_id) => MessageSelection::Newest,
        };
        Ok(())
    }
}

pub struct Messages<'a>(pub &'a State, pub &'a TuiState, pub Tasks<'a>);

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
                    let evt = TuiEvent(state.messages.message(id), window.get_width(), state);
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
                    self.2
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
                self.2
                    .set_message_query(room.to_owned(), MessageQuery::Newest);

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
    fn draw_selected(
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
                    collected_height +=
                        TuiEvent(state.messages.message(id), window.get_width(), state)
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
        let mut drawing_selected = true;
        loop {
            msg = match msg {
                EventWalkResult::Message(id) => {
                    let evt = TuiEvent(state.messages.message(id), window.get_width(), state);
                    let h = evt.space_demand().height.min;
                    let (mut current, below) = match window.split(h.from_origin()) {
                        Ok(pair) => pair,
                        Err(_) => {
                            break;
                        }
                    };

                    if drawing_selected {
                        current.set_default_style(
                            StyleModifier::new().invert(true).apply_to_default(),
                        );
                        drawing_selected = false;
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
                    self.2
                        .set_message_query(room.to_owned(), MessageQuery::AfterCache);
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

    fn draw(&self, mut window: Window, hints: RenderingHints) {
        if let Some(current) = self.1.current_room_state().as_ref() {
            if let Some(state) = self.0.rooms.get(&current.id) {
                match &current.selection {
                    MessageSelection::Newest => self.draw_newest(window, hints, &current.id, state),
                    MessageSelection::Specific(id) => {
                        self.draw_selected(window, hints, id, &current.id, state)
                    }
                }
            } else {
                let mut c = Cursor::new(&mut window);
                c.move_to_bottom();
                write!(&mut c, message_fetch_symbol!()).unwrap();
                let query = match &current.selection {
                    MessageSelection::Newest => MessageQuery::Newest,
                    MessageSelection::Specific(_id) => MessageQuery::BeforeCache,
                };
                self.2.set_message_query(current.id.to_owned(), query);
            }
        }
    }
}

struct TuiEvent<'a>(
    &'a crate::timeline::Event,
    Width,
    &'a crate::tui_app::RoomState,
);

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

impl TuiEvent<'_> {
    fn write_time<T: unsegen::base::CursorTarget>(&self, c: &mut Cursor<T>) {
        use chrono::TimeZone;
        let send_time_secs_unix = self.0.origin_server_ts().as_secs();
        let send_time_naive =
            chrono::naive::NaiveDateTime::from_timestamp(send_time_secs_unix.into(), 0);
        let send_time = chrono::Local.from_utc_datetime(&send_time_naive);
        let time_str = send_time.format("%m-%d %H:%M");
        let _ = write!(c, "{} ", time_str);
    }

    fn draw_with_cursor<T: unsegen::base::CursorTarget>(&self, c: &mut Cursor<T>) {
        self.write_time(c);
        match self.0 {
            crate::timeline::Event::Message(e) => match e {
                AnySyncMessageEvent::RoomMessage(msg) => {
                    write_user(c, &msg.sender, self.2);
                    c.set_wrapping_mode(WrappingMode::Wrap);
                    match &msg.content.msgtype {
                        MessageType::Text(text) => {
                            let _ = write!(c, ": ");
                            let start = c.get_col();
                            c.set_line_start_column(start);
                            let _ = write!(c, "{}", &text.body);
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
                        o => {
                            let _ = write!(c, "Other message {:?}", o);
                        }
                    }
                }
                AnySyncMessageEvent::RoomEncrypted(_msg) => {
                    c.write("*Unable to decrypt message*");
                }
                o => {
                    let _ = write!(c, "Other event {:?}", o);
                }
            },
            o => {
                let _ = write!(c, "Other event {:?}", o);
            }
        }
    }
}

impl Widget for TuiEvent<'_> {
    fn space_demand(&self) -> unsegen::widget::Demand2D {
        let mut est = unsegen::base::window::ExtentEstimationWindow::with_width(self.1);
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
