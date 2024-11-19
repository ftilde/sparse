use matrix_sdk::OwnedServerName;
use rlua::{Lua, RegistryKey, Value};
use sequence_trie::SequenceTrie;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use crate::tui_app::tui::{
    actions::{Action, ActionResult, CommandEnvironment, ACTIONS_ARGS_NONE, ACTIONS_ARGS_STRING},
    BuiltinMode, Mode,
};

pub struct ModeSet {
    custom: HashMap<String, BuiltinMode>,
    on_enter: HashMap<String, RegistryKey>,
    on_leave: HashMap<String, RegistryKey>,
}

impl ModeSet {
    pub fn new() -> Self {
        ModeSet {
            custom: HashMap::new(),
            on_enter: HashMap::new(),
            on_leave: HashMap::new(),
        }
    }
    pub fn define(&mut self, name: String, base: BuiltinMode) -> Result<(), ()> {
        if self.get(&name).is_none() {
            self.custom.insert(name, base);
            Ok(())
        } else {
            Err(())
        }
    }
    pub fn get(&self, name: &str) -> Option<Mode> {
        if let Ok(m) = BuiltinMode::from_str(name) {
            Some(Mode::Builtin(m))
        } else if let Some(m) = self.custom.get(name) {
            Some(Mode::Custom(name.to_string(), *m))
        } else {
            None
        }
    }
    pub fn define_on_enter<'lua>(
        &mut self,
        mode: &Mode,
        lua: &rlua::Context<'lua>,
        f: rlua::Function<'lua>,
    ) -> rlua::Result<()> {
        let k = lua.create_registry_value(f)?;
        self.on_enter.insert(mode.to_string(), k);
        Ok(())
    }
    pub fn get_on_enter(&self, mode: &Mode) -> Option<Action> {
        self.on_enter.get(mode.as_str()).map(Action)
    }
    pub fn define_on_leave<'lua>(
        &mut self,
        mode: &Mode,
        lua: &rlua::Context<'lua>,
        f: rlua::Function<'lua>,
    ) -> rlua::Result<()> {
        let k = lua.create_registry_value(f)?;
        self.on_enter.insert(mode.to_string(), k);
        Ok(())
    }
    pub fn get_on_leave(&self, mode: &Mode) -> Option<Action> {
        self.on_leave.get(mode.as_str()).map(Action)
    }
}

const DEFAULT_OPEN_PROG: &str = "xdg-open";

use unsegen::input::Key;

pub enum KeyMapFunctionResult<'a> {
    IsPrefix(Keys),
    FoundPrefix(Keys),
    Found(Action<'a>),
    NotFound,
}

struct KeyMap(SequenceTrie<Key, RegistryKey>);

impl std::default::Default for KeyMap {
    fn default() -> Self {
        KeyMap(SequenceTrie::new())
    }
}

impl std::borrow::Borrow<[Key]> for Keys {
    fn borrow(&self) -> &[Key] {
        return &self.0;
    }
}

impl KeyMap {
    fn add_binding<'lua>(
        &mut self,
        keys: Keys,
        lua: &rlua::Context<'lua>,
        f: rlua::Function<'lua>,
    ) -> rlua::Result<()> {
        match self.find(&keys) {
            KeyMapFunctionResult::IsPrefix(conflict) => Err(rlua::Error::RuntimeError(format!(
                "Key binding '{}' is a prefix of the existing binding '{}'",
                keys, conflict,
            ))),
            KeyMapFunctionResult::FoundPrefix(conflict) => Err(rlua::Error::RuntimeError(format!(
                "Existing key binding '{}' is a prefix of the desired binding '{}'",
                conflict, keys
            ))),
            KeyMapFunctionResult::Found(_) => Err(rlua::Error::RuntimeError(format!(
                "Key binding '{}' already exists",
                keys
            ))),
            KeyMapFunctionResult::NotFound => {
                //TODO: check function signature somehow?
                let k = lua.create_registry_value(f)?;
                let prev = self.0.insert_owned(keys.0, k);
                assert!(prev.is_none());
                Ok(())
            }
        }
    }

    pub fn find<'a>(&'a self, keys: &Keys) -> KeyMapFunctionResult<'a> {
        if let Some(f) = self.0.get(keys.0.iter()) {
            return KeyMapFunctionResult::Found(Action(f));
        }
        let prefix_nodes = self.0.get_prefix_nodes(keys.0.iter());
        if let Some(longest_prefix) = prefix_nodes.last() {
            let num_prefix_keys = prefix_nodes.len() - 1; //root node is empty => -1;
            let mut it = longest_prefix.keys();
            if longest_prefix.value().is_some() {
                assert!(longest_prefix.is_leaf(), "There is a prefix in the trie");
                let prefix = keys.0[0..num_prefix_keys].iter().cloned().collect();
                return KeyMapFunctionResult::FoundPrefix(Keys(prefix));
            }
            if keys.0.len() == num_prefix_keys {
                let k = it.next().unwrap();
                let conflict_keys = keys
                    .0
                    .iter()
                    .cloned()
                    .chain(k.into_iter().cloned())
                    .collect();
                return KeyMapFunctionResult::IsPrefix(Keys(conflict_keys));
            }
        }
        KeyMapFunctionResult::NotFound
    }
}

fn parse_keys(s: &str) -> rlua::Result<Vec<Key>> {
    let chars = s.chars().collect::<Vec<_>>();
    let mut chars = chars.as_slice();
    let mut keys = Vec::new();
    loop {
        let key = match chars {
            &['<', 'R', 'e', 't', 'u', 'r', 'n', '>', ref rest @ ..]
            | &['<', 'C', '-', 'm' | 'M', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char('\n')
            }
            &['<', 'S', 'p', 'a', 'c', 'e', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char(' ')
            }
            &['<', 'T', 'a', 'b', '>', ref rest @ ..]
            | &['<', 'C', '-', 'i' | 'I', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char('\t')
            }
            &['<', 'E', 's', 'c', '>', ref rest @ ..]
            | &['<', 'C', '-', '[', '>', ref rest @ ..] => {
                chars = rest;
                Key::Esc
            }
            &['<', 'E', 'n', 'd', '>', ref rest @ ..] => {
                chars = rest;
                Key::End
            }
            &['<', 'H', 'o', 'm', 'e', '>', ref rest @ ..] => {
                chars = rest;
                Key::Home
            }
            &['<', 'P', 'g', 'U', 'p', '>', ref rest @ ..] => {
                chars = rest;
                Key::PageUp
            }
            &['<', 'P', 'g', 'D', 'o', 'w', 'n', '>', ref rest @ ..] => {
                chars = rest;
                Key::PageDown
            }
            &['<', 'L', 'e', 'f', 't', '>', ref rest @ ..] => {
                chars = rest;
                Key::Left
            }
            &['<', 'R', 'i', 'g', 'h', 't', '>', ref rest @ ..] => {
                chars = rest;
                Key::Right
            }
            &['<', 'U', 'p', '>', ref rest @ ..] => {
                chars = rest;
                Key::Up
            }
            &['<', 'D', 'o', 'w', 'n', '>', ref rest @ ..] => {
                chars = rest;
                Key::Down
            }
            &['<', 'B', 'a', 'c', 'k', 's', 'p', 'a', 'c', 'e', '>', ref rest @ ..] => {
                chars = rest;
                Key::Backspace
            }
            &['<', 'D', 'e', 'l', 'e', 't', 'e', '>', ref rest @ ..] => {
                chars = rest;
                Key::Delete
            }
            &['<', 'C' | 'c', '-', c @ '!'..='~', '>', ref rest @ ..] => {
                chars = rest;
                Key::Ctrl(c)
            }
            &[c @ '!'..='~', ref rest @ ..] => {
                chars = rest;
                Key::Char(c)
            }
            &[] => break,
            _ => {
                return Err(rlua::Error::RuntimeError(format!(
                    "'{}' is not a valid key sequence",
                    s
                )))
            }
        };
        keys.push(key);
    }
    Ok(keys)
}

#[derive(Debug, Hash, PartialEq, Eq)]
pub struct Keys(pub Vec<Key>);

impl std::fmt::Display for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for k in &self.0 {
            match k {
                Key::Backspace => write!(f, "<Backspace>"),
                Key::Left => write!(f, "<Left>"),
                Key::Right => write!(f, "<Right>"),
                Key::Up => write!(f, "<Up>"),
                Key::Down => write!(f, "<Down>"),
                Key::Home => write!(f, "<Home>"),
                Key::End => write!(f, "<End>"),
                Key::PageUp => write!(f, "<PageUp>"),
                Key::PageDown => write!(f, "<PageDown>"),
                Key::BackTab => write!(f, "<BackTab>"),
                Key::Delete => write!(f, "<Delete>"),
                Key::Insert => write!(f, "<Insert>"),
                Key::F(n) => write!(f, "<F{}>", n),
                Key::Char(' ') => write!(f, "<Space>"),
                Key::Char('\n') => write!(f, "<Return>"),
                Key::Char('\t') => write!(f, "<Tab>"),
                Key::Char(c) => write!(f, "{}", c),
                Key::Alt(c) => write!(f, "<A-{}>", c),
                Key::Ctrl(c) => write!(f, "<C-{}>", c),
                Key::Null => write!(f, "<Null>"),
                Key::Esc => write!(f, "<Esc>"),
                _ => write!(f, "<UnknownKey>"),
            }?
        }
        Ok(())
    }
}

impl rlua::FromLua<'_> for Keys {
    fn from_lua(lua_value: Value<'_>, _lua: rlua::Context<'_>) -> rlua::Result<Self> {
        if let Value::String(s) = lua_value {
            parse_keys(s.to_str()?).map(Keys)
        } else {
            Err(rlua::Error::RuntimeError(format!(
                "'{:?}' is not a valid key sequence",
                lua_value
            )))
        }
    }
}

impl rlua::FromLua<'_> for BuiltinMode {
    fn from_lua(lua_value: rlua::Value<'_>, _lua: rlua::Context<'_>) -> rlua::Result<Self> {
        if let rlua::Value::String(s) = lua_value {
            let s = s.to_str()?;
            BuiltinMode::from_str(&s)
                .map_err(|_| rlua::Error::RuntimeError(format!("'{}' is not a builtin mode", s)))
        } else {
            Err(rlua::Error::RuntimeError(format!(
                "'{:?}' is not a mode",
                lua_value
            )))
        }
    }
}

#[derive(Copy, Clone)]
pub enum NotificationStyle {
    Disabled,
    NameOnly,
    NameAndGroup,
    Full,
}

impl std::default::Default for NotificationStyle {
    fn default() -> Self {
        NotificationStyle::Full
    }
}

impl rlua::FromLua<'_> for NotificationStyle {
    fn from_lua(lua_value: rlua::Value<'_>, _lua: rlua::Context<'_>) -> rlua::Result<Self> {
        if let rlua::Value::String(s) = lua_value {
            match s.to_str()? {
                "disabled" => Ok(NotificationStyle::Disabled),
                "nameonly" => Ok(NotificationStyle::NameOnly),
                "nameandgroup" => Ok(NotificationStyle::NameAndGroup),
                "full" => Ok(NotificationStyle::Full),
                s => Err(rlua::Error::RuntimeError(format!(
                    "'{}' is not a valid notification style",
                    s
                ))),
            }
        } else {
            Err(rlua::Error::RuntimeError(format!(
                "'{:?}' is not a valid notification style",
                lua_value
            )))
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub host: OwnedServerName,
    pub user: String,
    pub notification_style: NotificationStyle,
    pub file_open_program: String,
    pub url_open_program: String,
    pub keymaps: Arc<KeyMaps>,
    pub modes: Arc<ModeSet>,
}

impl Config {
    pub fn user_id(&self) -> String {
        format!("@{}:{}", self.user, self.host.host())
    }

    pub fn data_dir(&self) -> PathBuf {
        dirs::data_local_dir()
            .unwrap()
            .join(crate::APP_NAME)
            .join(self.user_id())
    }

    pub fn session_file_path(&self) -> PathBuf {
        self.data_dir().join("session")
    }
}
pub struct KeyMaps(HashMap<Mode, KeyMap>);

impl KeyMaps {
    pub fn find_action<'a>(&'a self, mode: &Mode, keys: &Keys) -> KeyMapFunctionResult<'a> {
        self.0
            .get(&mode)
            .map(|keymap| keymap.find(keys))
            .unwrap_or(KeyMapFunctionResult::NotFound)
    }
}

pub struct ConfigBuilder {
    keymaps: HashMap<Mode, KeyMap>,
    lua: Lua,
    host: Option<OwnedServerName>,
    user: Option<String>,
    notification_style: NotificationStyle,
    file_open_program: String,
    url_open_program: String,
    modes: ModeSet,
}

fn add_global_fun(context: &rlua::Context, name: &str, nargs: usize) -> rlua::Result<()> {
    let mut arg_str = String::with_capacity(3 * nargs);
    for i in 0..nargs {
        use std::fmt::Write;
        let _ = write!(arg_str, "s{},", i);
    }
    arg_str.pop();
    context
        .load(&format!(
            "{f} = function({args}) return function(c) return c:{f}({args}) end end",
            f = name,
            args = arg_str
        ))
        .eval()
}

impl ConfigBuilder {
    pub fn new() -> ConfigBuilder {
        ConfigBuilder {
            keymaps: HashMap::new(),
            lua: Lua::new(),
            host: None,
            user: None,
            notification_style: NotificationStyle::default(),
            file_open_program: DEFAULT_OPEN_PROG.to_owned(),
            url_open_program: DEFAULT_OPEN_PROG.to_owned(),
            modes: ModeSet::new(),
        }
    }
    pub fn finalize(self) -> Result<(Config, CommandEnvironment), String> {
        Ok((
            Config {
                host: self
                    .host
                    .ok_or_else(|| "Host not configured.".to_owned())?
                    .to_owned(),
                user: self.user.ok_or_else(|| "User not configured.".to_owned())?,
                notification_style: self.notification_style,
                file_open_program: self.file_open_program,
                url_open_program: self.url_open_program,
                keymaps: Arc::new(KeyMaps(self.keymaps)),
                modes: Arc::new(self.modes),
            },
            CommandEnvironment::new(self.lua),
        ))
    }
    pub fn set_host(&mut self, host: OwnedServerName) {
        self.host = Some(host);
    }
    pub fn set_user(&mut self, user: String) {
        self.user = Some(user);
    }
    pub fn configure(&mut self, source: &str) -> rlua::Result<()> {
        //TODO maybe we can avoid these bindings with disjoint struct capturing in 2021 edition?
        let keymaps = std::cell::RefCell::new(&mut self.keymaps);
        let modes = std::cell::RefCell::new(&mut self.modes);
        let host = &mut self.host;
        let user = &mut self.user;
        let notification_style = &mut self.notification_style;
        let file_open_program = &mut self.file_open_program;
        let url_open_program = &mut self.url_open_program;

        self.lua.context(|lua_ctx| {
            let globals = lua_ctx.globals();
            globals.set(
                "res_noop",
                lua_ctx.create_function_mut(|_lua_ctx, _: ()| Ok(ActionResult::Noop))?,
            )?;
            globals.set(
                "res_ok",
                lua_ctx.create_function_mut(|_lua_ctx, _: ()| Ok(ActionResult::Ok))?,
            )?;
            globals.set(
                "res_error",
                lua_ctx
                    .create_function_mut(|_lua_ctx, msg: String| Ok(ActionResult::Error(msg)))?,
            )?;

            lua_ctx.scope(|scope| {
                globals.set(
                    "define_mode",
                    scope.create_function_mut(
                        |_lua_ctx, (name, mode): (String, BuiltinMode)| {
                            let mut modes = modes.borrow_mut();
                            modes.define(name.clone(), mode).map_err(|_e| {
                                rlua::Error::RuntimeError(format!("Mode '{}' already exists", name))
                            })
                        },
                    )?,
                )?;

                globals.set(
                    "on_enter",
                    scope.create_function_mut(
                        |lua_ctx, (name, fun): (String, rlua::Function)| {
                            let mut modes = modes.borrow_mut();
                            let mode = modes.get(&name).ok_or_else(|| {
                                rlua::Error::RuntimeError(format!("No such mode '{}'", name))
                            })?;
                            modes.define_on_enter(&mode, &lua_ctx, fun)
                        },
                    )?,
                )?;

                globals.set(
                    "on_leave",
                    scope.create_function_mut(
                        |lua_ctx, (name, fun): (String, rlua::Function)| {
                            let mut modes = modes.borrow_mut();
                            let mode = modes.get(&name).ok_or_else(|| {
                                rlua::Error::RuntimeError(format!("No such mode '{}'", name))
                            })?;
                            modes.define_on_leave(&mode, &lua_ctx, fun)
                        },
                    )?,
                )?;

                globals.set(
                    "bind",
                    scope.create_function_mut(
                        |lua_ctx, (key, mode, action): (Keys, String, rlua::Function)| {
                            let modes = modes.borrow_mut();
                            let mode = modes.get(&mode).ok_or_else(|| {
                                rlua::Error::RuntimeError(format!("Mode '{}' is not defined", mode))
                            })?;
                            let mut keymaps = keymaps.borrow_mut();
                            let keymap = keymaps.entry(mode).or_default();
                            keymap.add_binding(key, &lua_ctx, action)?;
                            Ok(())
                        },
                    )?,
                )?;

                globals.set(
                    "unbind",
                    scope.create_function_mut(|_lua_ctx, (key, mode_s): (Keys, String)| {
                        let modes = modes.borrow_mut();
                        let mode = modes.get(&mode_s).ok_or_else(|| {
                            rlua::Error::RuntimeError(format!("Mode '{}' is not defined", mode_s))
                        })?;
                        let mut keymaps = keymaps.borrow_mut();
                        let keymap = keymaps.entry(mode).or_default();
                        if let KeyMapFunctionResult::Found(_) = keymap.find(&key) {
                            keymap.0.remove(&key.0);
                            Ok(())
                        } else {
                            Err(rlua::Error::RuntimeError(format!(
                                "No binding of {} in mode {} exists",
                                key, mode_s
                            )))
                        }
                    })?,
                )?;

                globals.set(
                    "clear_bindings",
                    scope.create_function_mut(|_lua_ctx, _: ()| {
                        let mut keymaps = keymaps.borrow_mut();
                        for (_, m) in keymaps.iter_mut() {
                            *m = KeyMap::default();
                        }
                        Ok(())
                    })?,
                )?;

                globals.set(
                    "host",
                    scope.create_function_mut(|_lua_ctx, host_str: String| {
                        println!("{}", host_str);
                        let url = matrix_sdk::ServerName::parse(&host_str)
                            .map_err(|e| rlua::Error::RuntimeError(format!("{}", e)))?;
                        *host = Some(url);
                        Ok(())
                    })?,
                )?;

                globals.set(
                    "user",
                    scope.create_function_mut(|_lua_ctx, user_str: String| {
                        *user = Some(user_str);
                        Ok(())
                    })?,
                )?;

                globals.set(
                    "notification_style",
                    scope.create_function_mut(|_lua_ctx, style: NotificationStyle| {
                        *notification_style = style;
                        Ok(())
                    })?,
                )?;

                globals.set(
                    "file_open_program",
                    scope.create_function_mut(|_lua_ctx, fop: String| {
                        *file_open_program = fop;
                        Ok(())
                    })?,
                )?;

                globals.set(
                    "url_open_program",
                    scope.create_function_mut(|_lua_ctx, uop: String| {
                        *url_open_program = uop;
                        Ok(())
                    })?,
                )?;

                // Define a shortcut binding for all methods of CommandContext
                for (n, _) in ACTIONS_ARGS_NONE {
                    lua_ctx
                        .load(&format!("{f} = function(c) return c:{f}() end", f = n))
                        .eval::<()>()?;
                }

                for (n, _) in ACTIONS_ARGS_STRING {
                    add_global_fun(&lua_ctx, n, 1)?;
                }
                add_global_fun(&lua_ctx, "cursor_move_forward", 1)?;
                add_global_fun(&lua_ctx, "cursor_move_backward", 1)?;
                add_global_fun(&lua_ctx, "cursor_delete", 2)?;

                lua_ctx.load(source).eval::<()>()?;
                Ok(())
            })
        })
    }
}
