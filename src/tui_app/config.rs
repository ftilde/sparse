use crate::tui_app::tui::TuiState;
use std::collections::HashMap;
use unsegen::input::{EditBehavior, Event, Input, Key};

enum Mode {
    Insert(InsertMode),
    Normal(NormalMode),
    RoomFilter,
    RoomFilterActive,
}

struct InsertMode {
    map: HashMap<Key, InsertModeCommand>,
}
type InsertModeCommand = fn();

struct NormalMode {
    map: HashMap<Key, NormalModeCommand>,
}
type NormalModeCommand = fn();

impl InsertMode {
    fn process(&mut self, input: Input, tui_state: &mut TuiState) {
        if let Event::Key(k) = input.event {
            if let Some(c) = self.map.get(&k) {
                c();
                return;
            }
        }

        if let Some(room) = tui_state.current_room_state_mut() {
            input.chain(EditBehavior::new(&mut room.msg_edit));
        }
    }
}

struct KeyMap {}

impl KeyMap {
    fn process(&mut self, input: Input) {}
}

type RoomFilterModeCommand = fn();
type RoomFilterActiveModeCommand = fn();

fn send_message() {}
