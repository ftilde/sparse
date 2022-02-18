use std::fmt::Write;
use unsegen::base::*;
use unsegen::input::{OperationResult, Scrollable};
use unsegen::widget::*;

use matrix_sdk::ruma::identifiers::RoomId;

use crate::tui_app::tui::BuiltinMode;
use crate::tui_app::State;

#[derive(Copy, Clone)]
pub struct Rooms<'a>(pub &'a State);

impl<'a> Rooms<'a> {
    fn all_rooms<'r>(
        self,
    ) -> impl DoubleEndedIterator<Item = (&'a Box<RoomId>, &'a crate::tui_app::RoomState)> + 'a
    {
        self.0.rooms.iter()
    }
    fn active_rooms(
        self,
    ) -> impl DoubleEndedIterator<Item = (&'a Box<RoomId>, &'a crate::tui_app::RoomState)> {
        let s = self.0.tui.room_filter_line.get();
        let s_lower = s.to_lowercase();
        let mixed = s != s_lower;
        let rooms = self.all_rooms();
        let only_with_unread = matches!(
            self.0.tui.mode.builtin_mode(),
            BuiltinMode::RoomFilterUnread
        );
        rooms.filter(move |(_i, r)| {
            let passes_filter_string = if mixed {
                r.name().contains(s)
            } else {
                r.name().to_lowercase().contains(&s_lower)
            };
            let passes_unread_filter = !(only_with_unread && !r.has_unread());
            passes_filter_string && passes_unread_filter
        })
    }
    pub fn active_contains_current(&self) -> bool {
        if let Some(current) = &self.0.tui.room_selection.current() {
            self.active_rooms()
                .into_iter()
                .find(|(id, _)| **id == *current)
                .is_some()
        } else {
            false
        }
    }
    pub fn as_widget(self) -> impl Widget + 'a {
        let mut layout = VLayout::new();

        if let BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread =
            self.0.tui.mode.builtin_mode()
        {
            layout = layout.widget(
                HLayout::new()
                    .widget("# ")
                    .widget(self.0.tui.room_filter_line.as_widget()),
            );
        };
        for (id, r) in self.active_rooms().into_iter() {
            layout = layout.widget(RoomSummary {
                state: r,
                current: self.0.tui.room_selection.current() == Some(id),
            });
        }
        layout
    }
}

pub struct RoomsMut<'a>(pub &'a mut State);

impl RoomsMut<'_> {
    pub fn as_rooms<'b>(&'b self) -> Rooms<'b> {
        Rooms(self.0)
    }
}
impl Scrollable for RoomsMut<'_> {
    fn scroll_backwards(&mut self) -> OperationResult {
        let new_current_room = if let Some(current) = self.0.tui.room_selection.current() {
            let rooms = self.as_rooms();
            let mut it = rooms
                .active_rooms()
                .into_iter()
                .rev()
                .skip_while(|(id, _)| **id != current);
            it.next();
            Some(
                it.next()
                    .or(self.as_rooms().active_rooms().into_iter().rev().next())
                    .map(|(k, _)| &**k)
                    .unwrap_or(current),
            )
        } else {
            self.0.rooms.keys().rev().next().map(|k| &**k)
        }
        .map(|r| r.to_owned());
        self.0.tui.set_current_room(new_current_room.as_deref());
        Ok(())
    }

    fn scroll_forwards(&mut self) -> OperationResult {
        let new_current_room = if let Some(current) = self.0.tui.room_selection.current() {
            let rooms = self.as_rooms();
            let mut it = rooms
                .active_rooms()
                .into_iter()
                .skip_while(|(id, _)| **id != current);
            it.next();
            Some(
                it.next()
                    .or(self.as_rooms().active_rooms().into_iter().next())
                    .map(|(k, _)| &**k)
                    .unwrap_or(current),
            )
        } else {
            self.0.rooms.keys().next().map(|k| &**k)
        }
        .map(|r| r.to_owned());
        self.0.tui.set_current_room(new_current_room.as_deref());
        Ok(())
    }
}

struct RoomSummary<'a> {
    state: &'a crate::tui_app::RoomState,
    current: bool,
}

impl Widget for RoomSummary<'_> {
    fn space_demand(&self) -> Demand2D {
        let mut w = text_width(self.state.name());
        let h = Height::new(1).unwrap();
        if self.state.has_unread() {
            w += text_width(&format!(" {}", self.state.num_unread_notifications()));
            //h += 1;
        }
        Demand2D {
            width: ColDemand::exact(w),
            height: RowDemand::from_to(Height::new(1).unwrap(), h),
        }
    }

    fn draw(&self, mut window: Window, _hints: RenderingHints) {
        let mut c = Cursor::new(&mut window);
        let mut style = StyleModifier::new();
        if self.current {
            style = style.invert(true);
        }
        if self.state.has_unread() {
            style = style.fg_color(unsegen::base::Color::Yellow);
        }
        c.set_style_modifier(style);

        c.write(self.state.name());

        if self.state.has_unread() {
            let _ = write!(c, " {}", self.state.num_unread_notifications());
            //let _ = write!(" {} \n {}", self.0.num_unread_notifications(), )
        }
    }
}
