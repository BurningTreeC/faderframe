//! The Lua state scripts run in (see the crate docs).

use crate::error::{ErrorKind, ScriptError};
use mlua::chunk::ChunkMode;
use mlua::thread::ThreadStatus;
use mlua::{
    Function, HookTriggers, IntoLuaMulti, Lua, LuaOptions, MultiValue, StdLib, Table, Thread,
    Value, Variadic, VmState,
};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How much a script may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// How long one run may compute (a script, a timer's call, a console
    /// line, the rest of a script after its prompt was answered). Time
    /// spent waiting for an answer never counts.
    pub time: Duration,
    /// The most memory the scripts' Lua state may hold (bytes).
    pub memory: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        time: Duration::from_secs(10),
        memory: 256 << 20,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The folders scripts reach.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Folders {
    /// Where `io.open` and `io.lines` may read and write (a relative path
    /// is in the first; files anywhere inside them, links followed).
    pub files: Vec<PathBuf>,
    /// Where `require` looks for modules, in order.
    pub modules: Vec<PathBuf>,
}

/// What a resumed run did.
#[derive(Debug)]
pub enum Resumed {
    /// It returned these values.
    Finished(MultiValue),
    /// It yielded these values (a prompt waiting for its answer).
    Yielded(MultiValue),
}

/// A run over its time keeps being yielded out of; where it cannot be (in
/// a C function that does not yield) it fails after this much more.
const GRACE: Duration = Duration::from_millis(500);

/// Lines of output kept until they are taken.
const OUTPUT_KEPT: usize = 5000;

/// The hook looks at the clock every this many instructions.
const HOOK_EVERY: u32 = 1000;

const LOAD_KEY: &str = "faderframe.load";
const ENV_MAKER_KEY: &str = "faderframe.environment";

#[derive(Default)]
struct Clock {
    deadline: Option<Instant>,
    /// The run went past its deadline and is being stopped.
    over: bool,
}

struct Shared {
    limits: Mutex<Limits>,
    clock: Mutex<Clock>,
    folders: Mutex<Folders>,
    output: Mutex<Output>,
}

#[derive(Default)]
struct Output {
    lines: Vec<String>,
    /// `io.write`'s text since its last newline.
    partial: String,
    dropped: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn limit_message(&self) -> String {
        let t = lock(&self.limits).time;
        let time = if t.subsec_millis() == 0 {
            format!("{} s", t.as_secs())
        } else {
            format!("{:.2} s", t.as_secs_f64())
        };
        format!("the script ran longer than {time} and was stopped (time limit)")
    }

    fn push_line(&self, line: String) {
        let mut o = lock(&self.output);
        if o.lines.len() >= OUTPUT_KEPT {
            o.lines.remove(0);
            o.dropped += 1;
        }
        o.lines.push(line);
    }

    fn write(&self, text: &str) {
        let mut done = Vec::new();
        {
            let mut o = lock(&self.output);
            o.partial.push_str(text);
            while let Some(i) = o.partial.find('\n') {
                let line: String = o.partial.drain(..=i).collect();
                done.push(line.trim_end_matches(['\n', '\r']).to_string());
            }
        }
        for l in done {
            self.push_line(l);
        }
    }

    /// The file `path` names, if scripts may open it.
    fn allowed(&self, path: &str) -> Result<PathBuf, String> {
        let folders = lock(&self.folders).files.clone();
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|f| std::fs::canonicalize(f).ok())
            .collect();
        let Some(first) = roots.first() else {
            return Err(
                "no folder is open to scripts here (save the project, or use the scripts folder)"
                    .into(),
            );
        };
        let p = Path::new(path);
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            first.join(p)
        };
        let resolved = match std::fs::canonicalize(&p) {
            Ok(c) => c,
            Err(_) => {
                // A file to be made: its folder must exist, its name be a
                // plain name.
                let name = p
                    .file_name()
                    .ok_or_else(|| format!("{path}: not a file name"))?;
                if Path::new(name)
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
                {
                    return Err(format!("{path}: not a file name"));
                }
                let parent = p.parent().ok_or_else(|| format!("{path}: no folder"))?;
                std::fs::canonicalize(parent)
                    .map_err(|e| format!("{path}: {e}"))?
                    .join(name)
            }
        };
        if roots.iter().any(|r| resolved.starts_with(r)) {
            Ok(resolved)
        } else {
            Err(format!(
                "{path} is outside the folders scripts may use (the project's folder and the scripts folders)"
            ))
        }
    }
}

/// A fresh run environment of a sandbox's state, from inside one of its
/// callbacks (what [`Sandbox::environment`] gives).
pub fn environment_of(lua: &Lua) -> mlua::Result<Table> {
    let make: Function = lua.named_registry_value(ENV_MAKER_KEY)?;
    make.call::<Table>(())
}

/// The scripts' Lua state (see the crate docs).
pub struct Sandbox {
    lua: Lua,
    shared: Arc<Shared>,
}

impl Sandbox {
    pub fn new(limits: Limits) -> Result<Self, ScriptError> {
        let libs = StdLib::TABLE
            | StdLib::STRING
            | StdLib::MATH
            | StdLib::UTF8
            | StdLib::COROUTINE
            | StdLib::OS
            | StdLib::IO;
        let lua = Lua::new_with(libs, LuaOptions::default())?;
        let shared = Arc::new(Shared {
            limits: Mutex::new(limits),
            clock: Mutex::new(Clock::default()),
            folders: Mutex::new(Folders::default()),
            output: Mutex::new(Output::default()),
        });
        let sandbox = Self { lua, shared };
        sandbox.install()?;
        sandbox.set_limits(limits)?;
        Ok(sandbox)
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    pub fn limits(&self) -> Limits {
        *lock(&self.shared.limits)
    }

    pub fn set_limits(&self, limits: Limits) -> Result<(), ScriptError> {
        *lock(&self.shared.limits) = limits;
        self.lua.set_memory_limit(limits.memory)?;
        Ok(())
    }

    pub fn folders(&self) -> Folders {
        lock(&self.shared.folders).clone()
    }

    pub fn set_folders(&self, folders: Folders) {
        *lock(&self.shared.folders) = folders;
    }

    /// The lines scripts printed since the last call (a note first when
    /// some were dropped for want of room).
    pub fn take_output(&self) -> Vec<String> {
        let mut o = lock(&self.shared.output);
        let mut lines = std::mem::take(&mut o.lines);
        if o.dropped > 0 {
            lines.insert(0, format!("… {} lines of output left out", o.dropped));
            o.dropped = 0;
        }
        lines
    }

    /// Print `line` as a script would.
    pub fn print(&self, line: impl Into<String>) {
        self.shared.push_line(line.into());
    }

    /// A fresh global environment for one run: its own globals over the
    /// shared libraries (and whatever [`Self::set_global`] put there).
    pub fn environment(&self) -> Result<Table, ScriptError> {
        Ok(environment_of(&self.lua)?)
    }

    /// Put `value` among the shared globals every environment sees.
    pub fn set_global(&self, name: &str, value: impl mlua::IntoLua) -> Result<(), ScriptError> {
        self.lua.globals().set(name, value)?;
        Ok(())
    }

    /// A read-only view of `table`: reading goes through, assigning
    /// fails, `pairs` lists the table's entries.
    pub fn freeze(&self, table: Table, name: &str) -> Result<Table, ScriptError> {
        let f: Function = self
            .lua
            .load(
                r#"
                local t, name = ...
                return setmetatable({}, {
                    __index = t,
                    __newindex = function(_, k)
                        error(name .. " is read-only (cannot set '" .. tostring(k) .. "')", 2)
                    end,
                    __pairs = function() return next, t, nil end,
                    __len = function() return #t end,
                    __metatable = false,
                })
                "#,
            )
            .set_name("=freeze")
            .into_function()?;
        Ok(f.call::<Table>((table, name))?)
    }

    /// Compile `source` as the chunk `name` (what errors name: the file's
    /// name, `console`) in `env`. Text only.
    pub fn compile(&self, source: &str, name: &str, env: &Table) -> Result<Function, ScriptError> {
        Ok(self
            .lua
            .load(source)
            .set_name(format!("@{name}"))
            .set_mode(ChunkMode::Text)
            .set_environment(env.clone())
            .into_function()?)
    }

    /// Run `thread` until it returns or yields, under the time limit.
    pub fn resume(&self, thread: &Thread, args: impl IntoLuaMulti) -> Result<Resumed, ScriptError> {
        self.arm();
        let r = thread.resume::<MultiValue>(args);
        let over = self.disarm();
        match r {
            Err(e) => Err(self.describe(&e, over)),
            Ok(_) if over && thread.status() == ThreadStatus::Resumable => Err(ScriptError::new(
                ErrorKind::TimeLimit,
                self.shared.limit_message(),
            )),
            Ok(v) if thread.status() == ThreadStatus::Resumable => Ok(Resumed::Yielded(v)),
            Ok(v) => Ok(Resumed::Finished(v)),
        }
    }

    /// Call `f` (not as a coroutine: it cannot prompt) under the time
    /// limit.
    pub fn call(&self, f: &Function, args: impl IntoLuaMulti) -> Result<MultiValue, ScriptError> {
        self.arm();
        let r = f.call::<MultiValue>(args);
        let over = self.disarm();
        r.map_err(|e| self.describe(&e, over))
    }

    fn arm(&self) {
        let time = lock(&self.shared.limits).time;
        *lock(&self.shared.clock) = Clock {
            deadline: Some(Instant::now() + time),
            over: false,
        };
    }

    /// Stop the clock; whether the run went over its time.
    fn disarm(&self) -> bool {
        let mut c = lock(&self.shared.clock);
        c.deadline = None;
        std::mem::take(&mut c.over)
    }

    fn describe(&self, e: &mlua::Error, over: bool) -> ScriptError {
        let mut out = ScriptError::from_lua(e);
        if over {
            out.kind = ErrorKind::TimeLimit;
            out.message = self.shared.limit_message();
        }
        out
    }

    /// The libraries as scripts see them (see the crate docs).
    fn install(&self) -> Result<(), ScriptError> {
        let lua = &self.lua;
        let g = lua.globals();
        // Base: nothing that reads files by itself; `load` for text only.
        g.set("dofile", Value::Nil)?;
        g.set("loadfile", Value::Nil)?;
        let raw_load: Function = g.get("load")?;
        lua.set_named_registry_value(LOAD_KEY, raw_load)?;
        let string: Table = g.get("string")?;
        string.set("dump", Value::Nil)?;
        // os: the clock and dates only.
        let os: Table = g.get("os")?;
        let safe_os = lua.create_table()?;
        for name in ["clock", "date", "difftime", "time"] {
            safe_os.set(name, os.get::<Value>(name)?)?;
        }
        g.set("os", safe_os)?;
        self.install_io(&g)?;
        self.install_print(&g)?;
        self.install_limits(&g)?;
        self.install_require(&g)?;
        // A fresh environment per run (Lua closures, so the collector
        // sees through them).
        let maker: Function = lua
            .load(
                r#"
                local G, raw_load = ...
                return function()
                    local env = setmetatable({}, { __index = G })
                    env._G = env
                    env.load = function(chunk, name, _, e)
                        return raw_load(chunk, name, "t", e or env)
                    end
                    return env
                end
                "#,
            )
            .set_name("=environment")
            .call::<Function>((g.clone(), lua.named_registry_value::<Function>(LOAD_KEY)?))?;
        lua.set_named_registry_value(ENV_MAKER_KEY, maker)?;
        Ok(())
    }

    fn install_print(&self, g: &Table) -> Result<(), ScriptError> {
        let s = Arc::clone(&self.shared);
        let print = self.lua.create_function(move |_, args: Variadic<Value>| {
            let parts = args
                .iter()
                .map(Value::to_string)
                .collect::<mlua::Result<Vec<String>>>()?;
            s.push_line(parts.join("\t"));
            Ok(())
        })?;
        g.set("print", print)?;
        Ok(())
    }

    /// `io`: files inside the allowed folders; `write` to the console.
    fn install_io(&self, g: &Table) -> Result<(), ScriptError> {
        let lua = &self.lua;
        let io: Table = g.get("io")?;
        let raw_open: Function = io.get("open")?;
        let raw_lines: Function = io.get("lines")?;
        let safe = lua.create_table()?;
        let s = Arc::clone(&self.shared);
        safe.set(
            "open",
            lua.create_function(move |_, (path, mode): (String, Option<String>)| {
                let mode = mode.unwrap_or_else(|| "r".into());
                if mode.is_empty() || !mode.chars().all(|c| "rwa+b".contains(c)) {
                    return Err(mlua::Error::runtime(format!("io.open: bad mode '{mode}'")));
                }
                let p = s.allowed(&path).map_err(mlua::Error::runtime)?;
                raw_open.call::<MultiValue>((p.to_string_lossy().into_owned(), mode))
            })?,
        )?;
        let s = Arc::clone(&self.shared);
        safe.set(
            "lines",
            lua.create_function(move |lua, (path, formats): (String, Variadic<Value>)| {
                let p = s.allowed(&path).map_err(mlua::Error::runtime)?;
                let mut args = vec![Value::String(
                    lua.create_string(p.to_string_lossy().as_bytes())?,
                )];
                args.extend(formats);
                raw_lines.call::<MultiValue>(MultiValue::from_iter(args))
            })?,
        )?;
        safe.set("type", io.get::<Value>("type")?)?;
        let s = Arc::clone(&self.shared);
        safe.set(
            "write",
            lua.create_function(move |_, args: Variadic<Value>| {
                for a in args.iter() {
                    match a {
                        Value::String(t) => s.write(&t.to_string_lossy()),
                        Value::Integer(_) | Value::Number(_) => s.write(&a.to_string()?),
                        other => {
                            return Err(mlua::Error::runtime(format!(
                                "io.write: a string or a number expected, got {}",
                                other.type_name()
                            )));
                        }
                    }
                }
                Ok(())
            })?,
        )?;
        safe.set(
            "read",
            lua.create_function(|_, _: Variadic<Value>| -> mlua::Result<Value> { Ok(Value::Nil) })?,
        )?;
        g.set("io", safe)?;
        Ok(())
    }

    /// The hook that stops runs over their time, and `pcall`, `xpcall`,
    /// `coroutine.resume`/`wrap` that pass a stop on; `collectgarbage`
    /// limited to what cannot hurt.
    fn install_limits(&self, g: &Table) -> Result<(), ScriptError> {
        let lua = &self.lua;
        let s = Arc::clone(&self.shared);
        lua.set_global_hook(
            HookTriggers::new().every_nth_instruction(HOOK_EVERY),
            move |_, _| {
                let mut c = lock(&s.clock);
                let Some(deadline) = c.deadline else {
                    return Ok(VmState::Continue);
                };
                let now = Instant::now();
                if now < deadline {
                    return Ok(VmState::Continue);
                }
                c.over = true;
                drop(c);
                if now > deadline + GRACE {
                    Err(mlua::Error::runtime(s.limit_message()))
                } else {
                    Ok(VmState::Yield)
                }
            },
        )?;
        let s = Arc::clone(&self.shared);
        let over = lua.create_function(move |_, ()| Ok(lock(&s.clock).over))?;
        let s = Arc::clone(&self.shared);
        let message = lua.create_function(move |_, ()| Ok(s.limit_message()))?;
        lua.load(
            r#"
            local over, message = ...
            local raw_pcall, raw_xpcall = pcall, xpcall
            local raw_resume, create = coroutine.resume, coroutine.create
            local raw_gc = collectgarbage
            local pack, unpack = table.pack, table.unpack
            local function pass(r)
                if over() then error(message(), 0) end
                return unpack(r, 1, r.n)
            end
            pcall = function(...) return pass(pack(raw_pcall(...))) end
            xpcall = function(...) return pass(pack(raw_xpcall(...))) end
            coroutine.resume = function(...) return pass(pack(raw_resume(...))) end
            coroutine.wrap = function(f)
                local co = create(f)
                return function(...)
                    local r = pack(raw_resume(co, ...))
                    if over() then error(message(), 0) end
                    if not r[1] then error(r[2], 0) end
                    return unpack(r, 2, r.n)
                end
            end
            local allowed = { count = true, collect = true, step = true, isrunning = true }
            collectgarbage = function(opt, ...)
                opt = opt or "collect"
                if not allowed[opt] then
                    error("collectgarbage('" .. tostring(opt) .. "') is not available to scripts", 2)
                end
                return raw_gc(opt, ...)
            end
            "#,
        )
        .set_name("=limits")
        .call::<()>((over, message))?;
        let _ = g;
        Ok(())
    }

    /// `require(name)`: `name.lua` (dots are folders; or `name/init.lua`)
    /// from the scripts folders, compiled as text in an environment of
    /// its own, once.
    fn install_require(&self, g: &Table) -> Result<(), ScriptError> {
        let lua = &self.lua;
        let loaded = lua.create_table()?;
        let s = Arc::clone(&self.shared);
        let require = lua.create_function(move |lua, name: String| {
            let cached: Value = loaded.get(name.as_str())?;
            if !cached.is_nil() {
                return Ok(cached);
            }
            let plain = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
                && !name.starts_with('.')
                && !name.ends_with('.')
                && !name.contains("..");
            if !plain {
                return Err(mlua::Error::runtime(format!(
                    "require: '{name}' is not a module name (letters, digits, _ - and dots)"
                )));
            }
            let rel = name.replace('.', "/");
            let folders = lock(&s.folders).modules.clone();
            for root in &folders {
                for candidate in [
                    root.join(format!("{rel}.lua")),
                    root.join(&rel).join("init.lua"),
                ] {
                    let Ok(source) = std::fs::read_to_string(&candidate) else {
                        continue;
                    };
                    let shown = candidate
                        .strip_prefix(root)
                        .unwrap_or(&candidate)
                        .to_string_lossy()
                        .into_owned();
                    let make: Function = lua.named_registry_value(ENV_MAKER_KEY)?;
                    let env: Table = make.call(())?;
                    let chunk = lua
                        .load(source.as_str())
                        .set_name(format!("@{shown}"))
                        .set_mode(ChunkMode::Text)
                        .set_environment(env)
                        .into_function()?;
                    let value: Value = chunk.call(name.as_str())?;
                    let value = if value.is_nil() {
                        Value::Boolean(true)
                    } else {
                        value
                    };
                    loaded.set(name.as_str(), value.clone())?;
                    return Ok(value);
                }
            }
            Err(mlua::Error::runtime(format!(
                "require: no module '{name}' in the scripts folders"
            )))
        })?;
        g.set("require", require)?;
        Ok(())
    }
}
