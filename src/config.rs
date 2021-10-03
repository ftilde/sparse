use rlua::{Lua, RegistryKey, UserData, UserDataMethods, Value};
use std::error::Error;
use std::{collections::HashMap, path::PathBuf};
use url::Url;

use crate::tui_app::tui::{
    actions::{CommandContext, ACTIONS},
    Mode,
};

use unsegen::input::Key;

pub struct KeyAction<'a>(&'a RegistryKey);

struct KeyMap(HashMap<Keys, RegistryKey>);

impl std::default::Default for KeyMap {
    fn default() -> Self {
        KeyMap(HashMap::new())
    }
}

impl std::borrow::Borrow<[Key]> for Keys {
    fn borrow(&self) -> &[Key] {
        return &self.0;
    }
}

pub enum KeyMapFunctionResult<'a> {
    IsPrefix(usize),
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
            KeyMapFunctionResult::IsPrefix(i) => Err(rlua::Error::RuntimeError(format!(
                "Key binding '{}' conflicts with existing binding(s) with prefix {}",
                keys,
                Keys(keys.0[..i].to_vec())
            ))),
            KeyMapFunctionResult::Found(_) => Err(rlua::Error::RuntimeError(format!(
                "Key binding '{}' already exists",
                keys
            ))),
            KeyMapFunctionResult::NotFound => {
                //TODO: check function signature somehow?
                let k = lua.create_registry_value(f)?;
                let prev = self.0.insert(keys, k);
                assert!(prev.is_none());
                Ok(())
            }
        }
    }

    pub fn find<'a>(&'a self, keys: &Keys) -> KeyMapFunctionResult<'a> {
        for i in 0..keys.0.len() {
            let prefix = &keys.0[0..i];
            if self.0.contains_key(prefix) {
                return KeyMapFunctionResult::IsPrefix(i);
            }
        }
        match self.0.get(keys) {
            Some(f) => KeyMapFunctionResult::Found(KeyAction(f)),
            None => KeyMapFunctionResult::NotFound,
        }
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
            &['<', 'C' | 'c', '-', c @ '!'..='/' | c @ ':'..='~', '>', ref rest @ ..] => {
                chars = rest;
                Key::Ctrl(c)
            }
            &[c @ '!'..='/' | c @ ':'..='~', ref rest @ ..] => {
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
        write!(f, "{:?}", self.0) //TODO: proper formatting
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
        for (name, f) in ACTIONS {
            methods.add_method_mut(name, move |_, this, _: ()| {
                //TODO: report errors. This has to be done in a way that is accessible to lua code
                //in order to allow for discarding of errors etc.
                let _ = f(this);
                Ok(())
            });
        }
    }
}

impl rlua::FromLua<'_> for Mode {
    fn from_lua(lua_value: rlua::Value<'_>, _lua: rlua::Context<'_>) -> rlua::Result<Self> {
        if let rlua::Value::String(s) = lua_value {
            match s.to_str()? {
                "normal" => Ok(Mode::Normal),
                "insert" => Ok(Mode::Insert),
                "roomfilter" => Ok(Mode::RoomFilter),
                "roomfilterunread" => Ok(Mode::RoomFilterUnread),
                s => Err(rlua::Error::RuntimeError(format!("'{}' is not a mode", s))),
            }
        } else {
            Err(rlua::Error::RuntimeError(format!(
                "'{:?}' is not a mode",
                lua_value
            )))
        }
    }
}

pub struct Config {
    keymaps: HashMap<Mode, KeyMap>,
    host: Option<Url>,
    user: Option<String>,
    lua: Lua,
}

impl Config {
    pub fn new() -> Config {
        Config {
            keymaps: HashMap::new(),
            lua: Lua::new(),
            host: None,
            user: None,
        }
    }
    pub fn host(&self) -> Result<&Url, String> {
        self.host
            .as_ref()
            .ok_or_else(|| "Host not configured.".to_owned())
    }
    pub fn user(&self) -> Result<&str, String> {
        self.user
            .as_ref()
            .map(|u| u.as_str())
            .ok_or_else(|| "User not configured.".to_owned())
    }
    pub fn user_id(&self) -> Result<String, Box<dyn Error>> {
        Ok(format!(
            "@{}:{}",
            self.user()?,
            self.host()?
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
    pub fn set_host(&mut self, host: Url) {
        self.host = Some(host);
    }
    pub fn set_user(&mut self, user: String) {
        self.user = Some(user);
    }
    pub fn configure(&mut self, source: &str) -> rlua::Result<()> {
        let keymaps = std::cell::RefCell::new(&mut self.keymaps);
        let host = &mut self.host;
        let user = &mut self.user;
        self.lua.context(|lua_ctx| {
            lua_ctx.scope(|scope| {
                let globals = lua_ctx.globals();
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
                        if keymap.0.remove(&key).is_none() {
                            Err(rlua::Error::RuntimeError(format!(
                                "No binding of {} in mode {} exists",
                                key, mode
                            )))
                        } else {
                            Ok(())
                        }
                    })?,
                )?;

                globals.set(
                    "clear_bindings",
                    scope.create_function_mut(|_lua_ctx, _: ()| {
                        let mut keymaps = keymaps.borrow_mut();
                        for (_, m) in keymaps.iter_mut() {
                            m.0.clear();
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

                // Define a shortcut binding for all methods of CommandContext
                for (n, _) in ACTIONS {
                    lua_ctx
                        .load(&format!("{f} = function(c) c:{f}() end", f = n))
                        .eval()?;
                }

                lua_ctx.load(source).eval()?;
                Ok(())
            })
        })
    }
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
    ) -> rlua::Result<()> {
        self.lua.context(|lua_ctx| {
            lua_ctx.scope(|scope| {
                let c = scope.create_nonstatic_userdata(c)?;
                let action: rlua::Function = lua_ctx.registry_value(action.0).unwrap();
                action.call::<_, ()>(c)
            })
        })
    }
}