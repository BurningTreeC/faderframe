#![allow(clippy::unwrap_used)]

use crate::sandbox::Resumed;
use crate::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn sandbox() -> Sandbox {
    Sandbox::new(Limits::default()).unwrap()
}

/// Run `source` as a script named `test.lua` to its end.
fn run(sb: &Sandbox, source: &str) -> Result<mlua::MultiValue, ScriptError> {
    let env = sb.environment()?;
    let f = sb.compile(source, "test.lua", &env)?;
    let thread = sb.lua().create_thread(f).map_err(ScriptError::from)?;
    match sb.resume(&thread, ())? {
        Resumed::Finished(v) => Ok(v),
        Resumed::Yielded(_) => panic!("the script yielded"),
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "faderframe-script-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn nothing_runs_programs_or_reads_the_machine() {
    let sb = sandbox();
    for (code, what) in [
        ("os.execute('true')", "os.execute"),
        ("os.exit(1)", "os.exit"),
        ("os.getenv('HOME')", "os.getenv"),
        ("os.remove('/tmp/x')", "os.remove"),
        ("os.rename('/tmp/x', '/tmp/y')", "os.rename"),
        ("io.popen('ls')", "io.popen"),
        ("io.tmpfile()", "io.tmpfile"),
        ("io.input('/etc/passwd')", "io.input"),
        ("dofile('/etc/passwd')", "dofile"),
        ("loadfile('/etc/passwd')", "loadfile"),
        ("string.dump(print)", "string.dump"),
        ("package.loadlib('x', 'y')", "package"),
        ("debug.getinfo(1)", "debug"),
    ] {
        let e = run(&sb, code).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Runtime, "{what}: {e}");
        assert_eq!(e.line, Some(1), "{what}: {e}");
    }
    // The clock and dates are there.
    let v = run(
        &sb,
        "return os.time() > 0 and os.clock() >= 0 and os.date('%Y') ~= nil",
    )
    .unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Boolean(true)));
    // No binary chunks, through `load` either.
    let v = run(&sb, "return load('\\27Lua')").unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Nil));
    // collectgarbage only counts and collects.
    assert!(run(&sb, "collectgarbage('stop')").is_err());
    assert!(run(&sb, "return collectgarbage('count')").is_ok());
}

#[test]
fn files_open_only_inside_the_allowed_folders() {
    let sb = sandbox();
    let inside = scratch("inside");
    let outside = scratch("outside");
    std::fs::write(outside.join("secret.txt"), "no").unwrap();
    // Nothing allowed yet.
    let e = run(&sb, "io.open('a.txt', 'w')").unwrap_err();
    assert!(e.message.contains("no folder"), "{e}");
    sb.set_folders(Folders {
        files: vec![inside.clone()],
        modules: Vec::new(),
    });
    // A relative path is in the first folder.
    run(
        &sb,
        "local f = assert(io.open('notes.txt', 'w')); f:write('one\\ntwo\\n'); f:close()",
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(inside.join("notes.txt")).unwrap(),
        "one\ntwo\n"
    );
    let v = run(
        &sb,
        "local n = 0; for l in io.lines('notes.txt') do n = n + 1 end; return n",
    )
    .unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Integer(2)));
    // Outside, by an absolute path or by climbing out.
    let secret = outside.join("secret.txt").to_string_lossy().into_owned();
    let climb = format!(
        "../{}/secret.txt",
        outside.file_name().unwrap().to_string_lossy()
    );
    for path in [secret.clone(), climb, "/etc/passwd".into()] {
        let code = format!("io.open({}, 'r')", literal::string(&path));
        let e = run(&sb, &code).unwrap_err();
        assert!(e.message.contains("outside the folders"), "{path}: {e}");
    }
    let code = format!("io.lines({})", literal::string(&secret));
    assert!(run(&sb, &code).is_err());
    // A link inside that points outside is outside.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("secret.txt"), inside.join("link.txt")).unwrap();
        let e = run(&sb, "io.open('link.txt', 'r')").unwrap_err();
        assert!(e.message.contains("outside the folders"), "{e}");
    }
    let _ = std::fs::remove_dir_all(&inside);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn a_run_over_its_time_is_stopped_whatever_catches_it() {
    let sb = Sandbox::new(Limits {
        time: Duration::from_millis(150),
        ..Limits::default()
    })
    .unwrap();
    for code in [
        "while true do end",
        "while true do pcall(function() while true do end end) end",
        "while true do local ok = xpcall(function() while true do end end, function(e) return e end) end",
        "local co = coroutine.wrap(function() while true do end end) while true do pcall(co) end",
        "while true do coroutine.resume(coroutine.create(function() while true do end end)) end",
    ] {
        let t = Instant::now();
        let e = run(&sb, code).unwrap_err();
        assert_eq!(e.kind, ErrorKind::TimeLimit, "{code}: {e}");
        assert!(e.message.contains("time limit"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5), "{code}");
    }
    // Inside a C function that cannot yield, it fails once past the grace.
    let t = Instant::now();
    let e = run(
        &sb,
        "local t = {3, 2, 1}; table.sort(t, function(a, b) while true do end end)",
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::TimeLimit, "{e}");
    assert!(t.elapsed() < Duration::from_secs(5));
    // The next run has its own time.
    assert!(run(&sb, "local n = 0; for i = 1, 1000 do n = n + i end").is_ok());
}

#[test]
fn memory_has_a_limit() {
    let sb = Sandbox::new(Limits {
        memory: 16 << 20,
        ..Limits::default()
    })
    .unwrap();
    let e = run(
        &sb,
        "local t = {} for i = 1, 1e9 do t[i] = ('x'):rep(1000) .. i end",
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    // Collected again, the state goes on working.
    run(&sb, "collectgarbage()").unwrap();
    assert!(run(&sb, "return 1 + 1").is_ok());
}

#[test]
fn errors_say_where_they_happened() {
    let sb = sandbox();
    let e = run(&sb, "local a = 1\nlocal b = = 2\n").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert_eq!(
        (e.chunk.as_deref(), e.line),
        (Some("test.lua"), Some(2)),
        "{e}"
    );
    let e = run(&sb, "local a = 1\n\nlocal b = nil\nreturn b.x\n").unwrap_err();
    assert_eq!(
        (e.chunk.as_deref(), e.line),
        (Some("test.lua"), Some(4)),
        "{e}"
    );
    assert!(e.message.contains("attempt to index"), "{e}");
    assert_eq!(e.to_string(), format!("test.lua:4: {}", e.message));
    let e = run(&sb, "\n\nerror('no such track')").unwrap_err();
    assert_eq!(e.line, Some(3));
    assert_eq!(e.message, "no such track");
    // An error a Rust function raises is placed by the traceback.
    let f = sb
        .lua()
        .create_function(|_, ()| -> mlua::Result<()> { Err(mlua::Error::runtime("refused")) })
        .unwrap();
    sb.set_global("refuse", f).unwrap();
    let e = run(&sb, "local x = 1\nlocal y = 2\nrefuse()\n").unwrap_err();
    assert_eq!(
        (e.chunk.as_deref(), e.line),
        (Some("test.lua"), Some(3)),
        "{e}"
    );
    assert!(e.message.contains("refused"), "{e}");
}

#[test]
fn every_run_has_its_own_globals_and_frozen_tables_stay_as_they_are() {
    let sb = sandbox();
    run(&sb, "counter = 41").unwrap();
    let v = run(&sb, "return counter").unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Nil));
    let api = sb.lua().create_table().unwrap();
    api.set("answer", 42).unwrap();
    let frozen = sb.freeze(api, "ff").unwrap();
    sb.set_global("ff", frozen).unwrap();
    let e = run(&sb, "ff.answer = 1").unwrap_err();
    assert!(e.message.contains("read-only"), "{e}");
    let v = run(
        &sb,
        "local n = 0 for k, v in pairs(ff) do n = n + v end return n",
    )
    .unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Integer(42)));
    // `load` compiles into the run's own environment.
    let v = run(&sb, "x = 5; return load('return x * 2')()").unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Integer(10)));
}

#[test]
fn print_and_io_write_reach_the_console() {
    let sb = sandbox();
    run(
        &sb,
        "print('a', 1, true, nil) io.write('b', 2) io.write('c\\n')",
    )
    .unwrap();
    assert_eq!(sb.take_output(), vec!["a\t1\ttrue\tnil", "b2c"]);
    assert!(sb.take_output().is_empty());
}

#[test]
fn modules_come_from_the_scripts_folders() {
    let sb = sandbox();
    let dir = scratch("modules");
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(
        dir.join("lib/util.lua"),
        "local M = {}\nfunction M.twice(x) return 2 * x end\nreturn M\n",
    )
    .unwrap();
    std::fs::write(dir.join("broken.lua"), "\n\nlocal = 1\n").unwrap();
    sb.set_folders(Folders {
        files: Vec::new(),
        modules: vec![dir.clone()],
    });
    let v = run(&sb, "return require('lib.util').twice(21)").unwrap();
    assert_eq!(v.into_iter().next(), Some(mlua::Value::Integer(42)));
    let e = run(&sb, "require('nothing.here')").unwrap_err();
    assert!(e.message.contains("no module"), "{e}");
    let e = run(&sb, "require('../etc/passwd')").unwrap_err();
    assert!(e.message.contains("not a module name"), "{e}");
    let e = run(&sb, "require('broken')").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax, "{e}");
    assert_eq!(
        (e.chunk.as_deref(), e.line),
        (Some("broken.lua"), Some(3)),
        "{e}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_prompt_yields_and_the_answer_comes_back() {
    let sb = sandbox();
    let env = sb.environment().unwrap();
    let f = sb
        .compile(
            "local answer = coroutine.yield('ask') return answer .. '!'",
            "ask.lua",
            &env,
        )
        .unwrap();
    let thread = sb.lua().create_thread(f).unwrap();
    match sb.resume(&thread, ()).unwrap() {
        Resumed::Yielded(v) => assert_eq!(
            v.into_iter()
                .next()
                .and_then(|v| v.as_string().map(|s| s.to_string_lossy())),
            Some("ask".to_string())
        ),
        Resumed::Finished(_) => panic!("it should wait"),
    }
    match sb.resume(&thread, "yes").unwrap() {
        Resumed::Finished(v) => assert_eq!(
            v.into_iter()
                .next()
                .and_then(|v| v.as_string().map(|s| s.to_string_lossy())),
            Some("yes!".to_string())
        ),
        Resumed::Yielded(_) => panic!("it should end"),
    }
}

#[test]
fn headers_name_scripts_and_number_macros() {
    let src = "#!/usr/bin/env lua\n--! name: Tidy Up\n--! description: Colours and names\n--! macro: 3\n--! author: me\n\nprint('hi')\n--! not a header\n";
    let info = read_header(src);
    assert_eq!(info.name.as_deref(), Some("Tidy Up"));
    assert_eq!(info.description.as_deref(), Some("Colours and names"));
    assert_eq!(info.macro_number, Some(3));
    assert_eq!(info.other, vec![("author".into(), "me".into())]);
    let renamed = ScriptInfo {
        name: Some("Neat".into()),
        ..info
    };
    let out = header::replace_header(src, &renamed);
    assert!(
        out.starts_with("#!/usr/bin/env lua\n--! name: Neat\n"),
        "{out}"
    );
    assert!(out.contains("print('hi')\n--! not a header"), "{out}");
    assert_eq!(read_header(&out), renamed);
    assert_eq!(read_header("print(1)"), ScriptInfo::default());
}

#[test]
fn literals_read_back_as_written() {
    let sb = sandbox();
    let text = "a \"quoted\"\\ line\nwith\ttab and \u{1} and ü";
    let v = run(&sb, &format!("return {}", literal::string(text))).unwrap();
    assert_eq!(
        v.into_iter()
            .next()
            .and_then(|v| v.as_string().map(|s| s.to_string_lossy())),
        Some(text.to_string())
    );
    for x in [0.0, 3.0, -2.5, 0.1, 1e-7, 123456.789, 1e20] {
        let v = run(&sb, &format!("return {}", literal::number(x))).unwrap();
        let back = match v.into_iter().next() {
            Some(mlua::Value::Integer(i)) => i as f64,
            Some(mlua::Value::Number(n)) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(back, x);
    }
    assert_eq!(literal::number(3.0), "3");
    assert_eq!(
        literal::table(&[("volume", "-3".into()), ("end", "1".into())]),
        "{ volume = -3, [\"end\"] = 1 }"
    );
    assert!(literal::is_name("track_1") && !literal::is_name("1x") && !literal::is_name("end"));
}
