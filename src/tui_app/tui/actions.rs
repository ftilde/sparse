use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use rlua::{Lua, RegistryKey, UserData, UserDataMethods};

use matrix_sdk::Client;
use unsegen::input::{Editable, Navigatable, OperationResult, Scrollable, Writable};
use unsegen::widget::builtin::TextEdit;

use matrix_sdk::ruma::events::{room::message::MessageType, AnySyncMessageEvent};

use super::super::State;
use super::{BuiltinMode, SendMessageType, Tasks};
use crate::config::Config;

pub struct KeyAction<'a>(pub &'a RegistryKey);

pub struct CommandContext<'a> {
    pub client: &'a Client,
    pub state: &'a mut State,
    pub config: &'a Config,
    pub tasks: Tasks<'a>,
    pub continue_running: &'a mut bool,
    pub command_environment: &'a CommandEnvironment,
}

impl<'a> CommandContext<'a> {
    pub fn run_command(&mut self, cmd: &str) -> rlua::Result<ActionResult> {
        self.command_environment.lua.context(|lua_ctx| {
            lua_ctx.scope(|scope| {
                let c = scope.create_nonstatic_userdata(self)?;
                let f: rlua::Function = match lua_ctx.load(cmd).eval() {
                    Ok(f) => f,
                    Err(rlua::Error::FromLuaConversionError { from: "nil", .. }) => {
                        return Ok(ActionResult::Error(format!(
                            "Expression '{}' is not a valid command.",
                            cmd
                        )))
                    }
                    Err(e) => return Err(e),
                };
                lua_ctx.load(cmd).eval()?;
                f.call::<_, ActionResult>(c)
            })
        })
    }
}

impl UserData for &mut CommandContext<'_> {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        for (name, f) in ACTIONS_ARGS_NONE {
            methods.add_method_mut(name, move |_, this, _: ()| Ok(f(this)));
        }
        for (name, f) in ACTIONS_ARGS_STRING {
            methods.add_method_mut(name, move |_, this, s: String| Ok(f(this, s)));
        }
        methods.add_method_mut("get_message_content", move |_, this, _: ()| {
            if let Some(r) = this.state.current_room_state_mut() {
                match &r.tui.selection {
                    super::MessageSelection::Newest => {
                        Err(rlua::Error::RuntimeError("No message selected".to_owned()))
                    }
                    super::MessageSelection::Specific(eid) => {
                        if let Some(crate::timeline::Event::Message(
                            AnySyncMessageEvent::RoomMessage(msg),
                        )) = r.messages.message_from_id(&eid)
                        {
                            Ok(msg.content.body().to_owned())
                        } else {
                            Err(rlua::Error::RuntimeError(
                                "Can only get content from message events".to_owned(),
                            ))
                        }
                    }
                }
            } else {
                Err(rlua::Error::RuntimeError("No current room".to_owned()))
            }
        });
    }
}

pub struct CommandEnvironment {
    lua: Lua,
}

impl CommandEnvironment {
    pub fn new(lua: Lua) -> Self {
        CommandEnvironment { lua }
    }
    pub fn run_action<'a>(
        &'a self,
        action: KeyAction<'a>,
        c: &mut CommandContext,
    ) -> rlua::Result<ActionResult> {
        self.lua.context(|lua_ctx| {
            lua_ctx.scope(|scope| {
                let c = scope.create_nonstatic_userdata(c)?;
                let action: rlua::Function = lua_ctx.registry_value(action.0).unwrap();
                let res = action.call::<_, ActionResult>(c);
                res
            })
        })
    }
}

#[must_use]
#[derive(Clone)]
pub enum ActionResult {
    Ok,
    Noop,
    Error(String),
}

impl From<OperationResult> for ActionResult {
    fn from(o: OperationResult) -> Self {
        match o {
            Ok(()) => Self::Ok,
            Err(()) => Self::Noop,
        }
    }
}

impl UserData for ActionResult {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_method_mut("is_ok", move |_, this, _: ()| {
            Ok(matches!(this, ActionResult::Ok))
        });
        methods.add_method_mut("is_noop", move |_, this, _: ()| {
            Ok(matches!(this, ActionResult::Noop))
        });
        methods.add_method_mut("is_error", move |_, this, _: ()| {
            Ok(matches!(this, ActionResult::Error(_)))
        });
    }
}

pub type ActionArgsNone = fn(&mut CommandContext) -> ActionResult;
pub type ActionArgsString = fn(&mut CommandContext, String) -> ActionResult;

pub const ACTIONS_ARGS_NONE: &[(&'static str, ActionArgsNone)] = &[
    ("send_message", |c| {
        if let Some(room) = c.state.current_room_state_mut() {
            let msg = room.tui.msg_edit.get().to_owned();
            if !msg.is_empty() {
                room.tui.msg_edit.clear().unwrap();
                let mut tmp_type = SendMessageType::Simple;
                std::mem::swap(&mut tmp_type, &mut room.tui.msg_edit_type);
                if let Some(room) = c.client.get_joined_room(&room.id) {
                    tokio::spawn(async move {
                        let content = match tmp_type {
                            SendMessageType::Simple => RoomMessageEventContent::text_plain(msg),
                            SendMessageType::Reply(orig_msg) => {
                                RoomMessageEventContent::text_reply_plain(msg, &orig_msg)
                            }
                        };
                        room.send(
                            matrix_sdk::ruma::events::AnyMessageEventContent::RoomMessage(content),
                            None,
                        )
                        .await
                        .unwrap();
                    });
                } else {
                    tracing::error!("can't send message, no joined room");
                }
                ActionResult::Ok
            } else {
                ActionResult::Noop
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("quit", |c| {
        *c.continue_running = false;
        ActionResult::Ok
    }),
    ("select_next_message", |c| {
        super::messages::MessagesMut(c.state)
            .scroll_forwards()
            .into()
    }),
    ("select_prev_message", |c| {
        super::messages::MessagesMut(c.state)
            .scroll_backwards()
            .into()
    }),
    ("deselect_message", |c| {
        super::messages::MessagesMut(c.state).scroll_to_end().into()
    }),
    ("select_next_room", |c| {
        super::rooms::RoomsMut(c.state).scroll_forwards().into()
    }),
    ("select_prev_room", |c| {
        super::rooms::RoomsMut(c.state).scroll_backwards().into()
    }),
    ("select_room_history_next", |c| {
        c.state.tui.room_selection.scroll_forwards().into()
    }),
    ("select_room_history_prev", |c| {
        c.state.tui.room_selection.scroll_backwards().into()
    }),
    ("force_room_selection", |c| {
        let mut r = super::rooms::RoomsMut(&mut c.state);
        if !r.as_rooms().active_contains_current() {
            let _ = r.scroll_forwards(); // Implicitly select first
            ActionResult::Ok
        } else {
            ActionResult::Noop
        }
    }),
    ("start_reply", |c| {
        if let Some(room) = c.state.current_room_state_mut() {
            if let super::MessageSelection::Specific(eid) = &room.tui.selection {
                if let Some(m) = room.messages.message_from_id(&eid) {
                    if let crate::timeline::Event::Message(AnySyncMessageEvent::RoomMessage(msg)) =
                        m
                    {
                        room.tui.msg_edit_type = super::SendMessageType::Reply(msg.clone());
                        room.tui.selection = super::MessageSelection::Newest;
                        ActionResult::Ok
                    } else {
                        ActionResult::Error(
                            format!("Only simple message events can be replied to",),
                        )
                    }
                } else {
                    ActionResult::Error(format!("Cannot find message with id {:?}", eid))
                }
            } else {
                ActionResult::Error("No message selected".to_owned())
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("cancel_reply", |c| {
        if let Some(room) = c.state.current_room_state_mut() {
            if !matches!(room.tui.msg_edit_type, super::SendMessageType::Simple) {
                room.tui.msg_edit_type = super::SendMessageType::Simple;
                ActionResult::Ok
            } else {
                ActionResult::Noop
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("clear_message", |c| {
        if let Some(room) = c.state.current_room_state_mut() {
            room.tui.msg_edit.clear().into()
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("clear_error_message", |c| {
        if c.state.tui.last_error_message.is_some() {
            c.state.tui.last_error_message = None;
            ActionResult::Ok
        } else {
            ActionResult::Noop
        }
    }),
    ("open_selected_message", |c| {
        if let Some(r) = c.state.current_room_state_mut() {
            match &r.tui.selection {
                super::MessageSelection::Newest => {
                    ActionResult::Error("No message selected".to_owned())
                }
                super::MessageSelection::Specific(eid) => {
                    if let Some(crate::timeline::Event::Message(
                        AnySyncMessageEvent::RoomMessage(msg),
                    )) = r.messages.message_from_id(&eid)
                    {
                        match &msg.content.msgtype {
                            MessageType::Text(t) => {
                                use linkify::{LinkFinder, LinkKind};

                                let mut finder = LinkFinder::new();
                                let links = finder.kinds(&[LinkKind::Url]).links(&t.body);
                                let mut res = ActionResult::Noop;
                                for link in links {
                                    open_url(&c.config, link.as_str().to_owned());
                                    res = ActionResult::Ok;
                                }
                                res
                            }
                            MessageType::Image(f) => {
                                open_file(c.client.clone(), &c.config, f.clone());
                                ActionResult::Ok
                            }
                            MessageType::Video(f) => {
                                open_file(c.client.clone(), &c.config, f.clone());
                                ActionResult::Ok
                            }
                            MessageType::Audio(f) => {
                                open_file(c.client.clone(), &c.config, f.clone());
                                ActionResult::Ok
                            }
                            MessageType::File(f) => {
                                open_file(c.client.clone(), &c.config, f.clone());
                                ActionResult::Ok
                            }
                            o => ActionResult::Error(format!("No open action for message {:?}", o)),
                        }
                    } else {
                        ActionResult::Error("No message selected".to_owned())
                    }
                }
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("cursor_move_left", |c| with_msg_edit(c, |e| e.move_left())),
    ("cursor_move_right", |c| {
        with_msg_edit(c, |e| e.move_right())
    }),
    ("cursor_move_down", |c| with_msg_edit(c, |e| e.move_down())),
    ("cursor_move_up", |c| with_msg_edit(c, |e| e.move_up())),
    ("cursor_delete_left", |c| {
        with_msg_edit(c, |e| e.delete_backwards())
    }),
    ("cursor_delete_right", |c| {
        with_msg_edit(c, |e| e.delete_forwards())
    }),
    ("cursor_move_beginning_of_line", |c| {
        with_msg_edit(c, |e| e.go_to_beginning_of_line())
    }),
    ("cursor_move_end_of_line", |c| {
        with_msg_edit(c, |e| e.go_to_end_of_line())
    }),
    ("run_command", |c| {
        if !c.state.tui.command_line.get().is_empty() {
            let cmd = c.state.tui.command_line.finish_line().to_owned();
            match c.run_command(&cmd) {
                Ok(r) => r,
                Err(e) => ActionResult::Error(format!("{}", e)),
            }
        } else {
            ActionResult::Noop
        }
    }),
    ("pop_mode", |c| match c.state.tui.pop_mode() {
        Ok(()) => ActionResult::Ok,
        Err(()) => ActionResult::Error("Cannot pop last element from mode stack.".to_owned()),
    }),
];

pub const ACTIONS_ARGS_STRING: &[(&'static str, ActionArgsString)] = &[
    ("type", |c, s| {
        match c.state.tui.current_mode().builtin_mode() {
            BuiltinMode::Normal | BuiltinMode::Insert => {
                if let Some(room) = c.state.current_room_state_mut() {
                    for ch in s.chars() {
                        room.tui.msg_edit.write(ch).unwrap();
                    }
                    ActionResult::Ok
                } else {
                    ActionResult::Error("No current room".to_owned())
                }
            }
            BuiltinMode::Command => {
                for ch in s.chars() {
                    c.state.tui.command_line.write(ch).unwrap();
                }
                ActionResult::Ok
            }
            BuiltinMode::RoomFilter | BuiltinMode::RoomFilterUnread => {
                for ch in s.chars() {
                    c.state.tui.room_filter_line.write(ch).unwrap();
                }
                ActionResult::Ok
            }
        }
    }),
    ("switch_mode", |c, s| match c.config.modes.get(&s) {
        None => ActionResult::Error(format!("'{}' is not a mode", s)),
        Some(m) => c.state.tui.switch_mode(m).into(),
    }),
    ("push_mode", |c, s| match c.config.modes.get(&s) {
        None => ActionResult::Error(format!("'{}' is not a mode", s)),
        Some(m) => {
            c.state.tui.push_mode(m);
            ActionResult::Ok
        }
    }),
    ("react", |c, s| {
        if let Some(room) = c.state.current_room_state_mut() {
            if let super::MessageSelection::Specific(eid) = &room.tui.selection {
                let reaction = matrix_sdk::ruma::events::reaction::ReactionEventContent::new(
                    matrix_sdk::ruma::events::reaction::Relation::new(eid.clone(), s),
                );
                if let Some(joined_room) = c.client.get_joined_room(&room.id) {
                    let client = c.client.clone();
                    let room_id = joined_room.room_id().to_owned();
                    tokio::spawn(async move {
                        let txn_id = uuid::Uuid::new_v4().to_string();
                        let request =
                            matrix_sdk::ruma::api::client::r0::message::send_message_event::Request::new(
                                &room_id,
                                &txn_id,
                                &reaction,
                            )
                            .unwrap();
                        // The change below can be reversed if matrix-sdk issue #470 is fixed.
                        //if let Err(e) = joined_room.send(reaction, None).await {
                        if let Err(e) = client.send(request, None).await {
                            tracing::error!("Cannot react to event: {:?}", e);
                        }
                    });
                    ActionResult::Ok
                } else {
                    ActionResult::Error("Room not joined".to_owned())
                }
            } else {
                ActionResult::Error("No message selected".to_owned())
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("send_file", |c, path| {
        if let Some(room) = c.state.current_room_state_mut() {
            if let Some(joined_room) = c.client.get_joined_room(&room.id) {
                let path = std::path::PathBuf::from(path);
                match std::fs::File::open(&path) {
                    Ok(mut file) => {
                        let mime_type = mime_guess::from_path(&path).first_or_octet_stream();
                        let description: String =
                            path.file_name().unwrap().to_string_lossy().to_string();
                        tokio::spawn(async move {
                            if let Err(e) = joined_room
                                .send_attachment(&description, &mime_type, &mut file, None)
                                .await
                            {
                                tracing::error!("Cannot send file: {:?}", e);
                            }
                        });
                        ActionResult::Ok
                    }
                    Err(e) => ActionResult::Error(format!("Cannot open file for sending: {:?}", e)),
                }
            } else {
                ActionResult::Error("Room not joined".to_owned())
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
    ("save_file", |c, path| {
        if let Some(r) = c.state.current_room_state() {
            match &r.tui.selection {
                super::MessageSelection::Newest => {
                    ActionResult::Error("No message selected".to_owned())
                }
                super::MessageSelection::Specific(eid) => {
                    if let Some(crate::timeline::Event::Message(
                        AnySyncMessageEvent::RoomMessage(msg),
                    )) = r.messages.message_from_id(&eid)
                    {
                        match &msg.content.msgtype {
                            MessageType::Image(f) => save_file(c.client.clone(), f.clone(), &path),
                            MessageType::Video(f) => save_file(c.client.clone(), f.clone(), &path),
                            MessageType::Audio(f) => save_file(c.client.clone(), f.clone(), &path),
                            MessageType::File(f) => save_file(c.client.clone(), f.clone(), &path),
                            o => ActionResult::Error(format!("No file to save in message {:?}", o)),
                        }
                    } else {
                        ActionResult::Error("No message selected".to_owned())
                    }
                }
            }
        } else {
            ActionResult::Error("No current room".to_owned())
        }
    }),
];

fn with_msg_edit(
    c: &mut CommandContext,
    mut f: impl FnMut(&mut TextEdit) -> OperationResult,
) -> ActionResult {
    if let Some(room) = c.state.current_room_state_mut() {
        let _ = f(&mut room.tui.msg_edit);
        ActionResult::Ok
    } else {
        ActionResult::Error("No current room".to_owned())
    }
}

fn open_url(config: &Config, url: String) {
    let open_prog = config.url_open_program.clone();
    tokio::spawn(async move {
        let mut join_handle = tokio::process::Command::new(open_prog)
            .arg(url)
            .spawn()
            .unwrap();
        join_handle.wait().await.unwrap();
    });
}

async fn write_file(
    c: Client,
    content: impl matrix_sdk::media::MediaEventContent + Send,
    target: &mut (dyn std::io::Write + Send),
) -> Result<(), String> {
    if let Some(media_type) = content.file() {
        match c
            .get_media_content(
                &matrix_sdk::media::MediaRequest {
                    media_type,
                    format: matrix_sdk::media::MediaFormat::File,
                },
                true,
            )
            .await
        {
            Ok(bytes) => {
                target.write_all(&bytes[..]).unwrap();
                target.flush().unwrap();
                Ok(())
            }
            Err(e) => Err(format!("can't open file: {:?}", e)),
        }
    } else {
        Err("can't open file: No content".to_owned())
    }
}

fn open_file(
    c: Client,
    config: &Config,
    content: impl matrix_sdk::media::MediaEventContent + Send + Sync + 'static,
) {
    let open_prog = config.file_open_program.clone();
    tokio::spawn(async move {
        let path = {
            let mut tmpfile = tempfile::NamedTempFile::new().unwrap();
            if let Err(e) = write_file(c, content, &mut tmpfile).await {
                tracing::error!("{}", e);
                return;
            }
            tmpfile.into_temp_path()
        };
        let mut join_handle = tokio::process::Command::new(open_prog)
            .arg(&path)
            .spawn()
            .unwrap();
        join_handle.wait().await.unwrap();
        path.keep().unwrap(); //We don't know if the file was opened when xdg-open finished...
    });
}

fn save_file(
    c: Client,
    content: impl matrix_sdk::media::MediaEventContent + Send + Sync + 'static,
    path: &str,
) -> ActionResult {
    let path = std::path::PathBuf::from(path);
    match std::fs::File::create(&path) {
        Ok(mut file) => {
            tokio::spawn(async move {
                if let Err(e) = write_file(c, content, &mut file).await {
                    tracing::error!("{}", e);
                }
            });
            ActionResult::Ok
        }
        Err(e) => ActionResult::Error(format!("Cannot open file for sending: {:?}", e)),
    }
}
