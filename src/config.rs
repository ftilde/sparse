use rlua::{Lua, RegistryKey, UserData, UserDataMethods, Value};
use sequence_trie::SequenceTrie;
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::str::FromStr;
use url::Url;

use crate::tui_app::tui::{
    actions::{ActionResult, CommandContext, ACTIONS_ARGS_NONE, ACTIONS_ARGS_STRING},
    Mode,
};

const DEFAULT_OPEN_PROG: &str = "xdg-open";

use unsegen::input::Key;

pub struct KeyAction<'a>(&'a RegistryKey);

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

pub enum KeyMapFunctionResult<'a> {
    IsPrefix(Keys),
    FoundPrefix(Keys),
    Found(KeyAction<'a>),
    NotFound,
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
            return KeyMapFunctionResult::Found(KeyAction(f));
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
            &['<', 'R', 'e', 't', 'u', 'r', 'n', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char('\n')
            }
            &['<', 'S', 'p', 'a', 'c', 'e', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char(' ')
            }
            &['<', 'T', 'a', 'b', '>', ref rest @ ..] => {
                chars = rest;
                Key::Char('\t')
            }
            &['<', 'E', 's', 'c', '>', ref rest @ ..] => {
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

impl UserData for &mut CommandContext<'_> {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        for (name, f) in ACTIONS_ARGS_NONE {
            methods.add_method_mut(name, move |_, this, _: ()| Ok(f(this)));
        }
        for (name, f) in ACTIONS_ARGS_STRING {
            methods.add_method_mut(name, move |_, this, s: String| Ok(f(this, s)));
        }
    }
}

impl rlua::FromLua<'_> for Mode {
    fn from_lua(lua_value: rlua::Value<'_>, _lua: rlua::Context<'_>) -> rlua::Result<Self> {
        if let rlua::Value::String(s) = lua_value {
            let s = s.to_str()?;
            Mode::from_str(&s)
                .map_err(|_| rlua::Error::RuntimeError(format!("'{}' is not a mode", s)))
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
    pub host: Url,
    pub user: String,
    pub notification_style: NotificationStyle,
    pub file_open_program: String,
    pub url_open_program: String,
}

impl Config {
    pub fn user_id(&self) -> Result<String, Box<dyn Error>> {
        Ok(format!(
            "@{}:{}",
            self.user,
            self.host
                .host_str()
                .ok_or_else(|| "Configured host is not a valid string".to_owned())?
        ))
    }

    pub fn data_dir(&self) -> Result<PathBuf, Box<dyn Error>> {
        Ok(dirs::data_local_dir()
            .unwrap()
            .join(crate::APP_NAME)
            .join(self.user_id()?))
    }

    pub fn session_file_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        Ok(self.data_dir()?.join("session"))
    }
}

pub struct KeyMapping {
    keymaps: HashMap<Mode, KeyMap>,
    lua: Lua,
}

impl KeyMapping {
    pub fn find_action<'a>(&'a self, mode: &Mode, keys: &Keys) -> KeyMapFunctionResult<'a> {
        self.keymaps
            .get(&mode)
            .map(|keymap| keymap.find(keys))
            .unwrap_or(KeyMapFunctionResult::NotFound)
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
                action.call::<_, ActionResult>(c)
            })
        })
    }
}
pub struct ConfigBuilder {
    keymaps: HashMap<Mode, KeyMap>,
    lua: Lua,
    host: Option<Url>,
    user: Option<String>,
    notification_style: NotificationStyle,
    file_open_program: String,
    url_open_program: String,
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
        }
    }
    pub fn finalize(self) -> Result<(Config, KeyMapping), String> {
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
            },
            KeyMapping {
                keymaps: self.keymaps,
                lua: self.lua,
            },
        ))
    }
    pub fn set_host(&mut self, host: Url) {
        self.host = Some(host);
    }
    pub fn set_user(&mut self, user: String) {
        self.user = Some(user);
    }
    pub fn configure(&mut self, source: &str) -> rlua::Result<()> {
        //TODO maybe we can avoid these bindings with disjoint struct capturing in 2021 edition?
        let keymaps = std::cell::RefCell::new(&mut self.keymaps);
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
                    "bind",
                    scope.create_function_mut(
                        |lua_ctx, (key, mode, action): (Keys, Mode, rlua::Function)| {
                            let mut keymaps = keymaps.borrow_mut();
                            let keymap = keymaps.entry(mode).or_default();
                            keymap.add_binding(key, &lua_ctx, action)?;
                            Ok(())
                        },
                    )?,
                )?;

                globals.set(
                    "unbind",
                    scope.create_function_mut(|_lua_ctx, (key, mode): (Keys, Mode)| {
                        let mut keymaps = keymaps.borrow_mut();
                        let keymap = keymaps.entry(mode).or_default();
                        if let KeyMapFunctionResult::Found(_) = keymap.find(&key) {
                            keymap.0.remove(&key.0);
                            Ok(())
                        } else {
                            Err(rlua::Error::RuntimeError(format!(
                                "No binding of {} in mode {} exists",
                                key, mode
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
                        let url = Url::parse(&host_str)
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
                        .eval()?;
                }

                for (n, _) in ACTIONS_ARGS_STRING {
                    lua_ctx
                        .load(&format!(
                            "{f} = function(s) return function(c) return c:{f}(s) end end",
                            f = n
                        ))
                        .eval()?;
                }

                lua_ctx.load(source).eval()?;
                Ok(())
            })
        })
    }
}
