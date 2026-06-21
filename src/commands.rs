//! Built-in commands and coreutils, implemented natively against the VFS.
//!
//! Every command has the signature `(interp, argv, stdin) -> (writes to out/err, status)`.
//! Builtins that must mutate interpreter state (cd, export, set, …) do so directly.

use crate::interp::Interp;

type Out<'a> = &'a mut Vec<u8>;

pub fn run(interp: &mut Interp, argv: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    let cmd = argv[0].as_str();
    let args = &argv[1..];
    match cmd {
        // ---- shell builtins (mutate interp) ----
        "cd" => bi_cd(interp, args, err),
        "pwd" => {
            wln(out, &interp.cwd);
            0
        }
        "export" => bi_export(interp, args),
        "unset" => {
            for a in args {
                interp.vars.remove(a);
                interp.exported.remove(a);
                interp.funcs.remove(a);
            }
            0
        }
        "set" => bi_set(interp, args),
        "declare" | "typeset" | "local" | "readonly" => bi_declare(interp, args),
        "source" | "." => bi_source(interp, args, out, err),
        "eval" => {
            let src = args.join(" ");
            interp.run_script_into(&src, out, err)
        }
        "exit" => {
            let code = args.first().and_then(|s| s.parse().ok()).unwrap_or(interp.last_status);
            interp.exiting = Some(code);
            code
        }
        "return" => {
            let code = args.first().and_then(|s| s.parse().ok()).unwrap_or(interp.last_status);
            interp.returning = Some(code);
            code
        }
        "break" => {
            interp.loop_break = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            0
        }
        "continue" => {
            interp.loop_continue = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            0
        }
        "shift" => {
            let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            for _ in 0..n {
                if interp.positional.is_empty() {
                    break;
                }
                interp.positional.remove(0);
            }
            0
        }
        "true" | ":" => 0,
        "false" => 1,
        "test" | "[" => bi_test(interp, cmd, args, err),
        "[[" => bi_test(interp, "[[", args, err),
        "read" => bi_read(interp, args, stdin),
        "trap" | "wait" | "jobs" | "disown" | "umask" | "ulimit" | "hash" | "complete" | "shopt"
        | "bind" | "history" | "exec" => 0,
        "kill" | "killall" | "pkill" => 0, // no real processes to signal
        "type" | "command" | "which" => bi_which(interp, args, out),
        "alias" | "unalias" => 0,
        "getopts" => 1, // signal "no more options" — scripts usually guard on this
        "let" => bi_let(interp, args),
        "mapfile" | "readarray" => 0,
        "pushd" | "popd" | "dirs" => 0,

        // ---- time / scheduling (virtual clock, never blocks) ----
        "sleep" => {
            let secs = args.first().map(|s| parse_duration(s)).unwrap_or(0);
            interp.clock.sleep_ms(secs);
            0
        }
        "usleep" => {
            let us: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            interp.clock.sleep_ms(us / 1000);
            0
        }
        "timeout" => {
            // timeout DURATION CMD ... : we never actually time out (virtual clock), just run cmd
            let rest: Vec<String> = args.iter().skip(1).cloned().collect();
            if rest.is_empty() {
                return 0;
            }
            run(interp, &rest, stdin, out, err)
        }
        "date" => bi_date(interp, args, out),
        "sync" => 0,

        // ---- output ----
        "echo" => bi_echo(args, out),
        "printf" => bi_printf(interp, args, out),
        "cat" => bi_cat(interp, args, stdin, out, err),
        "tac" => bi_tac(interp, args, stdin, out),
        "tee" => bi_tee(interp, args, stdin, out),
        "yes" => {
            let s = if args.is_empty() { "y".to_string() } else { args.join(" ") };
            for _ in 0..1000 {
                wln(out, &s);
            }
            0
        }

        // ---- filesystem ----
        "ls" => bi_ls(interp, args, out, err),
        "mkdir" => bi_mkdir(interp, args, err),
        "rmdir" => bi_rmdir(interp, args, err),
        "rm" => bi_rm(interp, args, err),
        "cp" => bi_cp(interp, args, err),
        "mv" => bi_mv(interp, args, err),
        "touch" => bi_touch(interp, args, err),
        "ln" => bi_ln(interp, args, err),
        "chmod" => bi_chmod(interp, args, err),
        "chown" | "chgrp" => bi_chown(interp, args, err),
        "basename" => bi_basename(args, out),
        "dirname" => bi_dirname(args, out),
        "realpath" | "readlink" => bi_realpath(interp, cmd, args, out, err),
        "stat" => bi_stat(interp, args, out, err),
        "find" => bi_find(interp, args, out, err),
        "du" => {
            wln(out, &format!("0\t{}", args.last().cloned().unwrap_or_else(|| ".".into())));
            0
        }
        "mktemp" => bi_mktemp(interp, args, out),
        "file" => bi_file(interp, args, out),

        // ---- text processing ----
        "head" => bi_head(interp, args, stdin, out, err),
        "tail" => bi_tail(interp, args, stdin, out, err),
        "wc" => bi_wc(interp, args, stdin, out, err),
        "sort" => bi_sort(interp, args, stdin, out, err),
        "uniq" => bi_uniq(interp, args, stdin, out, err),
        "cut" => bi_cut(interp, args, stdin, out, err),
        "tr" => bi_tr(args, stdin, out),
        "rev" => bi_rev(interp, args, stdin, out),
        "grep" | "egrep" | "fgrep" => bi_grep(interp, cmd, args, stdin, out, err),
        "sed" => bi_sed(interp, args, stdin, out, err),
        "nl" => bi_nl(interp, args, stdin, out),
        "seq" => bi_seq(args, out),
        "paste" => bi_paste(interp, args, stdin, out),
        "head_tail_placeholder" => 0,
        "fold" | "fmt" | "expand" | "unexpand" | "column" | "pr" => passthrough(interp, args, stdin, out),
        "xargs" => bi_xargs(interp, args, stdin, out, err),
        "comm" => bi_comm(interp, args, out, err),
        "diff" => bi_diff(interp, args, out, err),
        "cmp" => bi_cmp(interp, args, err),

        // ---- hashing / encoding ----
        "sha256sum" => bi_hash(interp, "sha256", args, stdin, out),
        "sha1sum" => bi_hash(interp, "sha1", args, stdin, out),
        "sha512sum" => bi_hash(interp, "sha512", args, stdin, out),
        "md5sum" => bi_hash(interp, "md5", args, stdin, out),
        "cksum" => bi_hash(interp, "crc32", args, stdin, out),
        "base64" => bi_base64(interp, args, stdin, out, err),
        "base32" => bi_base64(interp, args, stdin, out, err),
        "xxd" | "hexdump" | "od" => bi_hexdump(interp, args, stdin, out),
        "strings" => bi_strings(interp, args, stdin, out),

        // ---- arithmetic ----
        "expr" => bi_expr(args, out),
        "bc" => bi_bc(interp, stdin, out),
        "factor" => 0,

        // ---- network (virtual) ----
        "curl" => crate::netcmd::curl(interp, args, out, err),
        "wget" => crate::netcmd::wget(interp, args, out, err),

        // ---- interpreters ----
        "python3" | "python" | "python3.11" | "python3.12" | "python3.13" => {
            // run_python expects argv[0] to be the program name (it skips it)
            crate::python::run_python(interp, argv, stdin, out, err)
        }
        "jq" => crate::jqcmd::jq(interp, args, stdin, out, err),
        "pytest" | "py.test" => crate::python::run_pytest(interp, args, out, err),

        // ---- virtual network control (wire up fake URLs for curl/wget) ----
        "net" => bi_net(interp, args, out, err),

        // ---- nested shells / uv-launched verifiers ----
        "sh" | "bash" | "dash" | "zsh" => bi_sh(interp, args, stdin, out, err),
        "uv" | "uvx" | "uvenv" => bi_uv(interp, args, stdin, out, err),

        // ---- package managers / build (out of scope: record as unsupported) ----
        "pip" | "pip3" | "apt" | "apt-get" | "npm" | "node" | "cargo" | "make"
        | "gcc" | "g++" | "cc" | "clang" | "mvn" | "gradle" | "javac" | "java" | "docker"
        | "systemctl" | "service" | "uvicorn" | "gunicorn" | "flask" => {
            interp.note_unsupported(cmd);
            // Pretend success for env-build commands so scripts proceed; real work is absent.
            0
        }

        other => {
            // maybe it's an executable script in the VFS
            if let Some(code) = try_exec_script(interp, other, args, &stdin, out, err) {
                return code;
            }
            interp.note_unsupported(other);
            ewln(err, &format!("{other}: command not found"));
            127
        }
    }
}

// ============ helpers ============

fn wln(out: Out, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(b'\n');
}
fn w(out: Out, s: &str) {
    out.extend_from_slice(s.as_bytes());
}
fn ewln(err: Out, s: &str) {
    err.extend_from_slice(s.as_bytes());
    err.push(b'\n');
}

fn split_flags<'a>(args: &'a [String]) -> (Vec<char>, Vec<&'a String>, Vec<(&'a str, String)>) {
    // returns (single-char flags, positional operands, long/value flags)
    let mut flags = Vec::new();
    let mut ops = Vec::new();
    let mut long = Vec::new();
    let mut only_ops = false;
    for a in args {
        if only_ops {
            ops.push(a);
        } else if a == "--" {
            only_ops = true;
        } else if let Some(rest) = a.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                long.push((k, v.to_string()));
            } else {
                long.push((rest, String::new()));
            }
        } else if a.len() > 1 && a.starts_with('-') && !a[1..].chars().next().unwrap().is_ascii_digit() {
            for c in a[1..].chars() {
                flags.push(c);
            }
        } else {
            ops.push(a);
        }
    }
    (flags, ops, long)
}

fn parse_duration(s: &str) -> u64 {
    // returns milliseconds; supports s/m/h/d suffix and fractional seconds
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400_000.0)
    } else {
        (s, 1000.0)
    };
    (num.parse::<f64>().unwrap_or(0.0) * mult) as u64
}

fn lines_of(s: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(s).lines().map(|l| l.to_string()).collect()
}

/// Read each operand file (or stdin when "-") and return concatenated bytes plus any error.
fn read_inputs(interp: &Interp, files: &[&String], stdin: &[u8]) -> (Vec<u8>, Vec<String>) {
    let mut data = Vec::new();
    let mut errors = Vec::new();
    if files.is_empty() {
        data.extend_from_slice(stdin);
    } else {
        for f in files {
            if f.as_str() == "-" {
                data.extend_from_slice(stdin);
            } else {
                match interp.vfs.read(&interp.cwd, f) {
                    Ok(d) => data.extend(d),
                    Err(e) => errors.push(format!("{f}: {e}")),
                }
            }
        }
    }
    (data, errors)
}

// ============ builtins ============

fn bi_cd(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let target = match args.first().map(|s| s.as_str()) {
        None | Some("~") => interp.get_var("HOME").unwrap_or_else(|| "/".into()),
        Some("-") => interp.get_var("OLDPWD").unwrap_or_else(|| interp.cwd.clone()),
        Some(p) => p.to_string(),
    };
    let abs = crate::vfs::resolve_against(&interp.cwd, &target);
    if interp.vfs.is_dir("/", &abs) {
        let old = interp.cwd.clone();
        interp.set_var("OLDPWD", old);
        interp.cwd = interp.vfs.realpath(&abs, true).unwrap_or(abs);
        interp.set_var("PWD", interp.cwd.clone());
        0
    } else {
        ewln(err, &format!("cd: {target}: No such file or directory"));
        1
    }
}

fn bi_export(interp: &mut Interp, args: &[String]) -> i32 {
    for a in args {
        if let Some((k, v)) = a.split_once('=') {
            interp.set_var(k, v);
            interp.export(k);
        } else {
            interp.export(a);
        }
    }
    0
}

fn bi_set(interp: &mut Interp, args: &[String]) -> i32 {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-e" => interp.opt_errexit = true,
            "+e" => interp.opt_errexit = false,
            "-u" => interp.opt_nounset = true,
            "+u" => interp.opt_nounset = false,
            "-x" => interp.opt_xtrace = true,
            "+x" => interp.opt_xtrace = false,
            "-o" => {
                if let Some(opt) = args.get(i + 1) {
                    match opt.as_str() {
                        "pipefail" => interp.opt_pipefail = true,
                        "errexit" => interp.opt_errexit = true,
                        "nounset" => interp.opt_nounset = true,
                        _ => {}
                    }
                    i += 1;
                }
            }
            "+o" => {
                if let Some(opt) = args.get(i + 1) {
                    if opt == "pipefail" {
                        interp.opt_pipefail = false;
                    }
                    i += 1;
                }
            }
            s if s.starts_with("-euo") || s.starts_with("-eu") => {
                interp.opt_errexit = true;
                interp.opt_nounset = true;
                if s.contains('x') {
                    interp.opt_xtrace = true;
                }
            }
            s if !s.starts_with('-') && !s.starts_with('+') => {
                // set positional params
                interp.positional = args[i..].to_vec();
                break;
            }
            _ => {}
        }
        i += 1;
    }
    0
}

fn bi_declare(interp: &mut Interp, args: &[String]) -> i32 {
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        if let Some((k, v)) = a.split_once('=') {
            let v = v.trim_matches('"').trim_matches('\'');
            interp.set_var(k, v);
        }
    }
    0
}

fn bi_source(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    let Some(path) = args.first() else { return 0 };
    match interp.vfs.read_string(&interp.cwd, path) {
        Ok(src) => interp.run_script_into(&src, out, err),
        Err(_) => {
            ewln(err, &format!("source: {path}: No such file or directory"));
            1
        }
    }
}

fn bi_let(interp: &mut Interp, args: &[String]) -> i32 {
    let mut last = 0i64;
    for a in args {
        if let Some((name, expr)) = a.split_once('=') {
            let v = crate::expand::eval_arith(interp, expr);
            interp.set_var(name, v.to_string());
            last = v;
        } else {
            last = crate::expand::eval_arith(interp, a);
        }
    }
    if last == 0 {
        1
    } else {
        0
    }
}

fn bi_which(interp: &mut Interp, args: &[String], out: Out) -> i32 {
    let known = KNOWN_COMMANDS;
    let mut ok = true;
    for a in args {
        if interp.funcs.contains_key(a) {
            wln(out, &format!("{a} is a function"));
        } else if known.contains(&a.as_str()) {
            wln(out, &format!("/usr/bin/{a}"));
        } else {
            ok = false;
        }
    }
    if ok {
        0
    } else {
        1
    }
}

fn bi_read(interp: &mut Interp, args: &[String], stdin: Vec<u8>) -> i32 {
    let (_flags, ops, _long) = split_flags(args);
    // Obtain a line: from explicit stdin (pipe) if present, else from the persistent input
    // cursor (set up by a `< file` redirect on an enclosing loop).
    let line: String = if !stdin.is_empty() {
        String::from_utf8_lossy(&stdin).lines().next().unwrap_or("").to_string()
    } else if interp.input_pos < interp.input_stream.len() {
        let rest = &interp.input_stream[interp.input_pos..];
        let nl = rest.iter().position(|&b| b == b'\n');
        let (line_bytes, adv) = match nl {
            Some(i) => (&rest[..i], i + 1),
            None => (rest, rest.len()),
        };
        let l = String::from_utf8_lossy(line_bytes).into_owned();
        interp.input_pos += adv;
        l
    } else {
        return 1; // EOF
    };

    let ifs = interp.get_var("IFS").unwrap_or_else(|| " \t\n".to_string());
    if ops.is_empty() {
        interp.set_var("REPLY", line);
    } else if ifs.is_empty() {
        interp.set_var(&ops[0], line);
        for v in &ops[1..] {
            interp.set_var(v, "");
        }
    } else {
        let parts: Vec<&str> = line.split(|c| ifs.contains(c)).filter(|s| !s.is_empty()).collect();
        for (i, var) in ops.iter().enumerate() {
            if i == ops.len() - 1 {
                interp.set_var(var, parts[i..].join(" "));
            } else {
                interp.set_var(var, parts.get(i).copied().unwrap_or(""));
            }
        }
    }
    0
}

/// `net` — register fake URLs / probe the virtual network from a script.
///   net route <url-pattern> [status] [body...]   register a static response (`*` globs)
///   net route-file <url-pattern> <vfs-path>      serve a VFS file as the response body
///   net listen <host:port>                       mark a service as up
///   net log                                      print the request log
fn bi_net(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    match args.first().map(|s| s.as_str()) {
        Some("route") => {
            let Some(pattern) = args.get(1) else {
                ewln(err, "net route: missing url pattern");
                return 2;
            };
            let status = args.get(2).and_then(|s| s.parse::<u16>().ok()).unwrap_or(200);
            let body = if args.len() > 3 { args[3..].join(" ") } else { String::new() };
            interp.net.route_static(pattern, status, body.into_bytes());
            0
        }
        Some("route-file") => {
            match (args.get(1), args.get(2)) {
                (Some(pattern), Some(path)) => {
                    let abs = crate::vfs::resolve_against(&interp.cwd, path);
                    interp.net.route_vfs(pattern, &abs);
                    0
                }
                _ => {
                    ewln(err, "net route-file: usage: net route-file <pattern> <vfs-path>");
                    2
                }
            }
        }
        Some("listen") => {
            if let Some(hp) = args.get(1) {
                interp.net.listen(hp);
            }
            0
        }
        Some("log") => {
            for (m, u) in &interp.net.log {
                wln(out, &format!("{m} {u}"));
            }
            0
        }
        _ => {
            ewln(err, "net: usage: net route|route-file|listen|log …");
            2
        }
    }
}

/// `sh`/`bash -c "…"` or a script file — run it through our own interpreter.
fn bi_sh(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                let src = args.get(i + 1).cloned().unwrap_or_default();
                // `sh -c SCRIPT [name [args…]]`: name is $0, the rest are $1+
                let extra = args.get(i + 3..).map(|s| s.to_vec()).unwrap_or_default();
                let saved = std::mem::replace(&mut interp.positional, extra);
                let code = interp.run_script_into(&src, out, err);
                interp.positional = saved;
                interp.exiting = None;
                return code;
            }
            "-o" => {
                i += 2;
            }
            s if s.starts_with('-') => {
                i += 1;
            }
            s => {
                if let Ok(src) = interp.vfs.read_string(&interp.cwd, s) {
                    let extra = args.get(i + 1..).map(|x| x.to_vec()).unwrap_or_default();
                    let saved = std::mem::replace(&mut interp.positional, extra);
                    let code = interp.run_script_into(&src, out, err);
                    interp.positional = saved;
                    interp.exiting = None;
                    return code;
                }
                ewln(err, &format!("{}: {}: No such file or directory", args[0], s));
                return 127;
            }
        }
    }
    // no -c and no file → run stdin as a script
    let src = String::from_utf8_lossy(&stdin).into_owned();
    let code = interp.run_script_into(&src, out, err);
    interp.exiting = None;
    code
}

/// `uv` / `uvx` / `uv run` / `uv tool run`: we don't install anything, but we DO route an
/// embedded `pytest`/`python` invocation to our engines so verifiers that launch pytest via
/// uv still run. Everything else (pip install, venv, …) is a successful no-op.
fn bi_uv(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    if let Some(pos) = args.iter().position(|a| a == "pytest" || a.ends_with("/pytest")) {
        return crate::python::run_pytest(interp, &args[pos + 1..], out, err);
    }
    if let Some(pos) = args.iter().position(|a| a == "python" || a == "python3") {
        let mut argv = vec![args[pos].clone()];
        argv.extend(args[pos + 1..].iter().cloned());
        return crate::python::run_python(interp, &argv, stdin, out, err);
    }
    interp.note_unsupported(&args[0]);
    0
}

fn bi_test(interp: &mut Interp, cmd: &str, args: &[String], _err: Out) -> i32 {
    let mut a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    if (cmd == "[" || cmd == "[[") && a.last() == Some(&"]").or(Some(&"]]")) {
        a.pop();
    }
    if a.last() == Some(&"]") || a.last() == Some(&"]]") {
        a.pop();
    }
    let r = eval_test(interp, &a);
    if r {
        0
    } else {
        1
    }
}

fn eval_test(interp: &Interp, a: &[&str]) -> bool {
    match a.len() {
        0 => false,
        1 => !a[0].is_empty(),
        2 => {
            let (op, x) = (a[0], a[1]);
            match op {
                "-z" => x.is_empty(),
                "-n" => !x.is_empty(),
                "-e" | "-a" => interp.vfs.lexists(&interp.cwd, x),
                "-f" => interp.vfs.is_file(&interp.cwd, x),
                "-d" => interp.vfs.is_dir(&interp.cwd, x),
                "-s" => interp.vfs.read(&interp.cwd, x).map(|d| !d.is_empty()).unwrap_or(false),
                "-r" | "-w" | "-x" => interp.vfs.lexists(&interp.cwd, x),
                "-L" | "-h" => interp.vfs.is_symlink(&interp.cwd, x),
                "!" => !eval_test(interp, &a[1..]),
                _ => !op.is_empty(),
            }
        }
        3 => {
            let (x, op, y) = (a[0], a[1], a[2]);
            match op {
                "=" | "==" => glob_eq(y, x),
                "!=" => !glob_eq(y, x),
                "-eq" => num(x) == num(y),
                "-ne" => num(x) != num(y),
                "-lt" => num(x) < num(y),
                "-le" => num(x) <= num(y),
                "-gt" => num(x) > num(y),
                "-ge" => num(x) >= num(y),
                "<" => x < y,
                ">" => x > y,
                "-nt" => true,
                "-ot" => false,
                _ => false,
            }
        }
        _ => {
            // handle && || and ! and parens minimally: split on -a/-o
            if let Some(pos) = a.iter().position(|s| *s == "-a" || *s == "&&") {
                return eval_test(interp, &a[..pos]) && eval_test(interp, &a[pos + 1..]);
            }
            if let Some(pos) = a.iter().position(|s| *s == "-o" || *s == "||") {
                return eval_test(interp, &a[..pos]) || eval_test(interp, &a[pos + 1..]);
            }
            if a[0] == "!" {
                return !eval_test(interp, &a[1..]);
            }
            false
        }
    }
}

fn glob_eq(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') && !pattern.contains('[') {
        return pattern == text;
    }
    let mut re = String::from("^");
    for c in pattern.chars() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(c);
            }
            _ => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re).map(|r| r.is_match(text)).unwrap_or(false)
}

fn num(s: &str) -> i64 {
    s.trim().parse().unwrap_or(0)
}

fn bi_echo(args: &[String], out: Out) -> i32 {
    let mut newline = true;
    let mut interpret = false;
    let mut start = 0;
    for a in args {
        match a.as_str() {
            "-n" => {
                newline = false;
                start += 1;
            }
            "-e" => {
                interpret = true;
                start += 1;
            }
            "-E" => {
                interpret = false;
                start += 1;
            }
            "-ne" | "-en" => {
                newline = false;
                interpret = true;
                start += 1;
            }
            _ => break,
        }
    }
    let s = args[start..].join(" ");
    let s = if interpret { unescape(&s) } else { s };
    w(out, &s);
    if newline {
        out.push(b'\n');
    }
    0
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('0') => out.push('\0'),
                Some('a') => out.push('\u{7}'),
                Some('b') => out.push('\u{8}'),
                Some(o) => {
                    out.push('\\');
                    out.push(o);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn bi_printf(interp: &mut Interp, args: &[String], out: Out) -> i32 {
    let _ = interp;
    if args.is_empty() {
        return 0;
    }
    let fmt = &args[0];
    let rest = &args[1..];
    let result = printf_format(fmt, rest);
    w(out, &result);
    0
}

fn printf_format(fmt: &str, args: &[String]) -> String {
    let fmt = unescape(fmt);
    let mut out = String::new();
    let mut ai = 0;
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    // printf reuses the format string until args are exhausted
    loop {
        let start_ai = ai;
        while i < chars.len() {
            if chars[i] == '%' {
                if chars.get(i + 1) == Some(&'%') {
                    out.push('%');
                    i += 2;
                    continue;
                }
                // parse a conversion spec: %[-+ 0#][width][.prec][conv]
                let spec_start = i;
                i += 1;
                while i < chars.len() && "-+ 0#".contains(chars[i]) {
                    i += 1;
                }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '*') {
                    i += 1;
                }
                if i < chars.len() && chars[i] == '.' {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let conv = chars.get(i).copied().unwrap_or('s');
                let spec: String = chars[spec_start..=i.min(chars.len() - 1)].iter().collect();
                i += 1;
                let arg = args.get(ai).cloned().unwrap_or_default();
                ai += 1;
                out.push_str(&apply_conv(&spec, conv, &arg));
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        if ai >= args.len() || ai == start_ai {
            break;
        }
        i = 0;
    }
    out
}

fn apply_conv(spec: &str, conv: char, arg: &str) -> String {
    // minimal width/precision handling for the common cases
    let width: Option<usize> = spec
        .trim_start_matches('%')
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok();
    let left = spec.contains('-');
    let zero = spec.starts_with("%0") || spec.starts_with("%-0");
    let body = match conv {
        'd' | 'i' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            n.to_string()
        }
        'x' => format!("{:x}", arg.trim().parse::<i64>().unwrap_or(0)),
        'X' => format!("{:X}", arg.trim().parse::<i64>().unwrap_or(0)),
        'o' => format!("{:o}", arg.trim().parse::<i64>().unwrap_or(0)),
        'f' | 'F' => {
            let prec = spec.split('.').nth(1).and_then(|p| p.trim_end_matches(|c: char| c.is_alphabetic()).parse::<usize>().ok()).unwrap_or(6);
            format!("{:.*}", prec, arg.trim().parse::<f64>().unwrap_or(0.0))
        }
        's' => {
            if let Some(prec) = spec.split('.').nth(1).and_then(|p| p.trim_end_matches(|c: char| c.is_alphabetic()).parse::<usize>().ok()) {
                arg.chars().take(prec).collect()
            } else {
                arg.to_string()
            }
        }
        'c' => arg.chars().next().map(|c| c.to_string()).unwrap_or_default(),
        'b' => unescape(arg),
        _ => arg.to_string(),
    };
    if let Some(wd) = width {
        if body.len() < wd {
            let pad = if zero && !left { "0" } else { " " };
            let padding = pad.repeat(wd - body.len());
            return if left { format!("{body}{padding}") } else { format!("{padding}{body}") };
        }
    }
    body
}

fn bi_cat(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    let (flags, ops, _long) = split_flags(args);
    let number = flags.contains(&'n');
    let (data, errors) = read_inputs(interp, &ops, &stdin);
    if number {
        for (i, line) in String::from_utf8_lossy(&data).lines().enumerate() {
            wln(out, &format!("{:6}\t{}", i + 1, line));
        }
    } else {
        out.extend_from_slice(&data);
    }
    for e in &errors {
        ewln(err, &format!("cat: {e}"));
    }
    if errors.is_empty() {
        0
    } else {
        1
    }
}

fn bi_tac(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let lines = lines_of(&data);
    for l in lines.iter().rev() {
        wln(out, l);
    }
    0
}

fn bi_tee(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let append = flags.contains(&'a');
    let cwd = interp.cwd.clone();
    for f in &ops {
        if append {
            let _ = interp.vfs.append(&cwd, f, &stdin, 0o644);
        } else {
            let _ = interp.vfs.write(&cwd, f, &stdin, 0o644);
        }
    }
    out.extend_from_slice(&stdin);
    0
}

fn bi_ls(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let long = flags.contains(&'l');
    let all = flags.contains(&'a');
    let one = flags.contains(&'1') || long;
    let recursive = flags.contains(&'R');
    let paths: Vec<String> = if ops.is_empty() {
        vec![interp.cwd.clone()]
    } else {
        ops.iter().map(|s| s.to_string()).collect()
    };
    let mut status = 0;
    for p in &paths {
        if interp.vfs.is_dir(&interp.cwd, p) {
            let mut entries = match interp.vfs.list_dir(&interp.cwd, p) {
                Ok(e) => e,
                Err(e) => {
                    ewln(err, &format!("ls: {e}"));
                    status = 2;
                    continue;
                }
            };
            if all {
                entries.insert(0, "..".into());
                entries.insert(0, ".".into());
            }
            if paths.len() > 1 {
                wln(out, &format!("{p}:"));
            }
            emit_listing(interp, p, &entries, long, one, out);
            if recursive {
                for e in &entries {
                    if e == "." || e == ".." {
                        continue;
                    }
                    let sub = format!("{}/{}", p.trim_end_matches('/'), e);
                    if interp.vfs.is_dir(&interp.cwd, &sub) {
                        wln(out, "");
                        wln(out, &format!("{sub}:"));
                        if let Ok(se) = interp.vfs.list_dir(&interp.cwd, &sub) {
                            emit_listing(interp, &sub, &se, long, one, out);
                        }
                    }
                }
            }
        } else if interp.vfs.lexists(&interp.cwd, p) {
            wln(out, p);
        } else {
            ewln(err, &format!("ls: cannot access '{p}': No such file or directory"));
            status = 2;
        }
    }
    status
}

fn emit_listing(interp: &Interp, dir: &str, entries: &[String], long: bool, one: bool, out: Out) {
    if long {
        for e in entries {
            let full = if e == "." {
                dir.to_string()
            } else if e == ".." {
                crate::vfs::parent_of(&crate::vfs::resolve_against(&interp.cwd, dir)).unwrap_or_else(|| "/".into())
            } else {
                format!("{}/{}", dir.trim_end_matches('/'), e)
            };
            let (typ, mode, size) = match interp.vfs.metadata(&interp.cwd, &full, false) {
                Ok(n) => {
                    let t = match n.kind {
                        crate::vfs::NodeKind::Dir => 'd',
                        crate::vfs::NodeKind::Symlink(_) => 'l',
                        _ => '-',
                    };
                    let sz = match &n.kind {
                        crate::vfs::NodeKind::File(d) => d.len(),
                        _ => 0,
                    };
                    (t, n.mode, sz)
                }
                Err(_) => ('-', 0o644, 0),
            };
            wln(out, &format!("{}{} 1 root root {:>6} Jan  1 00:00 {}", typ, mode_str(mode), size, e));
        }
    } else if one {
        for e in entries {
            wln(out, e);
        }
    } else {
        wln(out, &entries.join("  "));
    }
}

fn mode_str(mode: u32) -> String {
    let bits = ['r', 'w', 'x'];
    let mut s = String::new();
    for shift in [6, 3, 0] {
        let g = (mode >> shift) & 0o7;
        for (i, b) in bits.iter().enumerate() {
            if g & (1 << (2 - i)) != 0 {
                s.push(*b);
            } else {
                s.push('-');
            }
        }
    }
    s
}

fn bi_mkdir(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let parents = flags.contains(&'p');
    let cwd = interp.cwd.clone();
    let mut status = 0;
    for d in &ops {
        let r = if parents {
            interp.vfs.mkdir_all(&cwd, d)
        } else {
            interp.vfs.mkdir(&cwd, d)
        };
        if let Err(e) = r {
            if !parents {
                ewln(err, &format!("mkdir: cannot create directory '{d}': {e}"));
                status = 1;
            }
        }
    }
    status
}

fn bi_rmdir(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let cwd = interp.cwd.clone();
    let mut status = 0;
    for d in &ops {
        if let Err(e) = interp.vfs.rmdir(&cwd, d) {
            ewln(err, &format!("rmdir: failed to remove '{d}': {e}"));
            status = 1;
        }
    }
    status
}

fn bi_rm(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let recursive = flags.contains(&'r') || flags.contains(&'R');
    let force = flags.contains(&'f');
    let cwd = interp.cwd.clone();
    let mut status = 0;
    for t in &ops {
        let r = if recursive {
            interp.vfs.remove_all(&cwd, t)
        } else {
            interp.vfs.remove_file(&cwd, t)
        };
        if let Err(e) = r {
            if !force {
                ewln(err, &format!("rm: cannot remove '{t}': {e}"));
                status = 1;
            }
        }
    }
    status
}

fn bi_cp(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let recursive = flags.contains(&'r') || flags.contains(&'R') || flags.contains(&'a');
    if ops.len() < 2 {
        ewln(err, "cp: missing destination operand");
        return 1;
    }
    let cwd = interp.cwd.clone();
    let dest = ops.last().unwrap();
    let sources = &ops[..ops.len() - 1];
    let mut status = 0;
    for s in sources {
        let r = if recursive {
            interp.vfs.copy_recursive(&cwd, s, dest)
        } else {
            interp.vfs.copy_file(&cwd, s, dest)
        };
        if let Err(e) = r {
            ewln(err, &format!("cp: cannot copy '{s}': {e}"));
            status = 1;
        }
    }
    status
}

fn bi_mv(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    if ops.len() < 2 {
        ewln(err, "mv: missing destination operand");
        return 1;
    }
    let cwd = interp.cwd.clone();
    let dest = ops.last().unwrap();
    let mut status = 0;
    for s in &ops[..ops.len() - 1] {
        if let Err(e) = interp.vfs.rename(&cwd, s, dest) {
            ewln(err, &format!("mv: cannot move '{s}': {e}"));
            status = 1;
        }
    }
    status
}

fn bi_touch(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let cwd = interp.cwd.clone();
    let now = interp.clock.unix_ms();
    let mut status = 0;
    for t in &ops {
        if let Err(e) = interp.vfs.touch(&cwd, t, now) {
            ewln(err, &format!("touch: {e}"));
            status = 1;
        }
    }
    status
}

fn bi_ln(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let symbolic = flags.contains(&'s');
    if ops.len() < 2 {
        ewln(err, "ln: missing operand");
        return 1;
    }
    let cwd = interp.cwd.clone();
    let (target, link) = (ops[0], ops[1]);
    let r = if symbolic {
        interp.vfs.symlink(&cwd, target, link)
    } else {
        interp.vfs.copy_file(&cwd, target, link)
    };
    if let Err(e) = r {
        ewln(err, &format!("ln: {e}"));
        1
    } else {
        0
    }
}

fn parse_mode(s: &str, cur: u32) -> u32 {
    if let Ok(oct) = u32::from_str_radix(s, 8) {
        if s.chars().all(|c| c.is_digit(8)) {
            return oct & 0o7777;
        }
    }
    // symbolic like u+x,g-w
    let mut mode = cur;
    for clause in s.split(',') {
        let (whoset, rest) = clause.split_at(clause.find(['+', '-', '=']).unwrap_or(0));
        if rest.is_empty() {
            continue;
        }
        let op = rest.chars().next().unwrap();
        let perms = &rest[1..];
        let mut mask = 0u32;
        for p in perms.chars() {
            mask |= match p {
                'r' => 0o444,
                'w' => 0o222,
                'x' => 0o111,
                _ => 0,
            };
        }
        let who_mask = if whoset.is_empty() || whoset.contains('a') {
            0o777
        } else {
            let mut m = 0;
            if whoset.contains('u') {
                m |= 0o700;
            }
            if whoset.contains('g') {
                m |= 0o070;
            }
            if whoset.contains('o') {
                m |= 0o007;
            }
            m
        };
        let bits = mask & who_mask;
        match op {
            '+' => mode |= bits,
            '-' => mode &= !bits,
            '=' => mode = (mode & !who_mask) | bits,
            _ => {}
        }
    }
    mode
}

fn bi_chmod(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let mut recursive = false;
    let mut mode_arg = None;
    let mut targets = Vec::new();
    for a in args {
        if a == "-R" || a == "--recursive" {
            recursive = true;
        } else if mode_arg.is_none() && (a.chars().all(|c| c.is_digit(8)) || a.contains(['+', '-', '='])) && !a.starts_with('/') {
            mode_arg = Some(a.clone());
        } else {
            targets.push(a.clone());
        }
    }
    let Some(mode_arg) = mode_arg else {
        ewln(err, "chmod: missing operand");
        return 1;
    };
    let cwd = interp.cwd.clone();
    let mut status = 0;
    for t in &targets {
        let paths = if recursive {
            let abs = crate::vfs::resolve_against(&cwd, t);
            interp.vfs.walk(&abs)
        } else {
            vec![crate::vfs::resolve_against(&cwd, t)]
        };
        for p in paths {
            let cur = interp.vfs.metadata("/", &p, true).map(|n| n.mode).unwrap_or(0o644);
            let m = parse_mode(&mode_arg, cur);
            if let Err(e) = interp.vfs.chmod("/", &p, m) {
                ewln(err, &format!("chmod: {e}"));
                status = 1;
            }
        }
    }
    status
}

fn bi_chown(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let recursive = flags.contains(&'R');
    if ops.is_empty() {
        return 0;
    }
    let spec = ops[0];
    let (uid, gid) = parse_owner(spec);
    let cwd = interp.cwd.clone();
    let mut status = 0;
    for t in &ops[1..] {
        let paths = if recursive {
            interp.vfs.walk(&crate::vfs::resolve_against(&cwd, t))
        } else {
            vec![crate::vfs::resolve_against(&cwd, t)]
        };
        for p in paths {
            if let Err(e) = interp.vfs.chown("/", &p, uid, gid) {
                ewln(err, &format!("chown: {e}"));
                status = 1;
            }
        }
    }
    status
}

fn parse_owner(spec: &str) -> (Option<u32>, Option<u32>) {
    let name_to_uid = |n: &str| -> Option<u32> {
        n.parse().ok().or(match n {
            "root" => Some(0),
            "" => None,
            _ => Some(1000),
        })
    };
    if let Some((u, g)) = spec.split_once(':') {
        (name_to_uid(u), name_to_uid(g))
    } else {
        (name_to_uid(spec), None)
    }
}

fn bi_basename(args: &[String], out: Out) -> i32 {
    let Some(p) = args.first() else { return 1 };
    let mut base = crate::vfs::basename(p.trim_end_matches('/')).to_string();
    if let Some(suffix) = args.get(1) {
        if base.ends_with(suffix.as_str()) && &base != suffix {
            base = base[..base.len() - suffix.len()].to_string();
        }
    }
    wln(out, &base);
    0
}

fn bi_dirname(args: &[String], out: Out) -> i32 {
    let Some(p) = args.first() else { return 1 };
    let p = p.trim_end_matches('/');
    let d = match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => p[..i].to_string(),
        None => ".".to_string(),
    };
    wln(out, &d);
    0
}

fn bi_realpath(interp: &mut Interp, cmd: &str, args: &[String], out: Out, err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    for p in &ops {
        if cmd == "readlink" {
            if flags.contains(&'f') {
                let abs = crate::vfs::resolve_against(&interp.cwd, p);
                match interp.vfs.realpath(&abs, true) {
                    Ok(r) => wln(out, &r),
                    Err(_) => return 1,
                }
            } else {
                match interp.vfs.read_link(&interp.cwd, p) {
                    Ok(t) => wln(out, &t),
                    Err(_) => {
                        ewln(err, &format!("readlink: {p}: Invalid argument"));
                        return 1;
                    }
                }
            }
        } else {
            let abs = crate::vfs::resolve_against(&interp.cwd, p);
            match interp.vfs.realpath(&abs, true) {
                Ok(r) => wln(out, &r),
                Err(_) => wln(out, &abs),
            }
        }
    }
    0
}

fn bi_stat(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    let (_f, ops, long) = split_flags(args);
    let fmt = long.iter().find(|(k, _)| *k == "format" || *k == "printf").map(|(_, v)| v.clone());
    // also handle -c FORMAT
    let mut format = fmt;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-c" || a == "--format" {
            format = it.next().cloned();
        } else if !a.starts_with('-') {
            files.push(a.clone());
        }
    }
    let _ = ops;
    for f in &files {
        match interp.vfs.metadata(&interp.cwd, f, true) {
            Ok(n) => {
                let size = match &n.kind {
                    crate::vfs::NodeKind::File(d) => d.len(),
                    _ => 0,
                };
                if let Some(fmt) = &format {
                    let s = fmt
                        .replace("%s", &size.to_string())
                        .replace("%n", f)
                        .replace("%a", &format!("{:o}", n.mode))
                        .replace("%U", "root")
                        .replace("%u", &n.uid.to_string())
                        .replace("%g", &n.gid.to_string())
                        .replace("%Y", &(n.mtime / 1000).to_string());
                    wln(out, &s);
                } else {
                    wln(out, &format!("  File: {f}\n  Size: {size}"));
                }
            }
            Err(e) => {
                ewln(err, &format!("stat: {e}"));
                return 1;
            }
        }
    }
    0
}

fn bi_mktemp(interp: &mut Interp, args: &[String], out: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let dir = flags.contains(&'d');
    let tmpl = ops.first().map(|s| s.as_str()).unwrap_or("tmp.XXXXXX");
    // deterministic: use the clock to make a unique-ish suffix
    let n = interp.clock.now_ms();
    let suffix = format!("{:06}", n % 1_000_000);
    let name = tmpl.replace("XXXXXX", &suffix);
    let path = if name.starts_with('/') { name } else { format!("/tmp/{name}") };
    let _ = interp.vfs.mkdir_all("/", "/tmp");
    if dir {
        let _ = interp.vfs.mkdir_all("/", &path);
    } else {
        let _ = interp.vfs.write("/", &path, b"", 0o600);
    }
    interp.clock.tick(1);
    wln(out, &path);
    0
}

fn bi_file(interp: &mut Interp, args: &[String], out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    for p in &ops {
        let desc = match interp.vfs.read(&interp.cwd, p) {
            Ok(d) if d.is_empty() => "empty".to_string(),
            Ok(d) if d.iter().all(|b| b.is_ascii() || *b >= 0x80) && std::str::from_utf8(&d).is_ok() => "ASCII text".to_string(),
            Ok(_) => "data".to_string(),
            Err(_) if interp.vfs.is_dir(&interp.cwd, p) => "directory".to_string(),
            Err(_) => "cannot open".to_string(),
        };
        wln(out, &format!("{p}: {desc}"));
    }
    0
}

fn bi_find(interp: &mut Interp, args: &[String], out: Out, _err: Out) -> i32 {
    // supports: find [paths...] [-type f|d] [-name PAT] [-maxdepth N] [-path PAT]
    let mut paths = Vec::new();
    let mut typ: Option<char> = None;
    let mut name_pat: Option<String> = None;
    let mut path_pat: Option<String> = None;
    let mut maxdepth: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-type" => {
                typ = args.get(i + 1).and_then(|s| s.chars().next());
                i += 2;
            }
            "-name" => {
                name_pat = args.get(i + 1).cloned();
                i += 2;
            }
            "-path" => {
                path_pat = args.get(i + 1).cloned();
                i += 2;
            }
            "-maxdepth" => {
                maxdepth = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "-print" | "-print0" => {
                i += 1;
            }
            s if s.starts_with('-') => {
                i += 2; // skip unknown predicate + arg
            }
            s => {
                paths.push(s.to_string());
                i += 1;
            }
        }
    }
    if paths.is_empty() {
        paths.push(".".to_string());
    }
    for start in &paths {
        let abs = crate::vfs::resolve_against(&interp.cwd, start);
        let base_depth = abs.matches('/').count();
        let mut all = interp.vfs.walk(&abs);
        all.sort();
        for p in all {
            if let Some(md) = maxdepth {
                let depth = p.matches('/').count().saturating_sub(base_depth);
                if depth > md {
                    continue;
                }
            }
            let is_dir = matches!(interp.vfs.metadata("/", &p, false).map(|n| n.kind), Ok(crate::vfs::NodeKind::Dir));
            if let Some(t) = typ {
                let ok = match t {
                    'd' => is_dir,
                    'f' => interp.vfs.is_file("/", &p),
                    'l' => interp.vfs.is_symlink("/", &p),
                    _ => true,
                };
                if !ok {
                    continue;
                }
            }
            if let Some(pat) = &name_pat {
                if !glob_eq(pat, crate::vfs::basename(&p)) {
                    continue;
                }
            }
            if let Some(pat) = &path_pat {
                if !glob_eq(pat, &p) {
                    continue;
                }
            }
            // print relative to the start path the way find does
            let display = if start == "." {
                if p == abs {
                    ".".to_string()
                } else {
                    format!(".{}", &p[abs.len()..])
                }
            } else {
                let prefix = format!("{}/", interp.cwd.trim_end_matches('/'));
                if !start.starts_with('/') {
                    p.strip_prefix(&prefix).map(|s| s.to_string()).unwrap_or(p.clone())
                } else {
                    p.clone()
                }
            };
            wln(out, &display);
        }
    }
    0
}

// ---- text utils ----

fn bi_head(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let mut n = 10usize;
    let mut bytes: Option<usize> = None;
    let mut files = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-n" {
            n = it.next().and_then(|s| s.trim_start_matches('-').parse().ok()).unwrap_or(10);
        } else if let Some(v) = a.strip_prefix("-n") {
            n = v.trim_start_matches('-').parse().unwrap_or(10);
        } else if a == "-c" {
            bytes = it.next().and_then(|s| s.parse().ok());
        } else if let Some(v) = a.strip_prefix("-c") {
            bytes = v.parse().ok();
        } else if a.starts_with('-') && a.len() > 1 && a[1..].chars().all(|c| c.is_ascii_digit()) {
            n = a[1..].parse().unwrap_or(10);
        } else if !a.starts_with('-') || a == "-" {
            files.push(a);
        }
    }
    let (data, _e) = read_inputs(interp, &files, &stdin);
    if let Some(b) = bytes {
        out.extend_from_slice(&data[..b.min(data.len())]);
    } else {
        for (i, line) in String::from_utf8_lossy(&data).lines().enumerate() {
            if i >= n {
                break;
            }
            wln(out, line);
        }
    }
    0
}

fn bi_tail(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let mut n = 10usize;
    let mut from_start = false;
    let mut files = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-n" {
            let v = it.next().cloned().unwrap_or_default();
            from_start = v.starts_with('+');
            n = v.trim_start_matches('+').trim_start_matches('-').parse().unwrap_or(10);
        } else if let Some(v) = a.strip_prefix("-n") {
            from_start = v.starts_with('+');
            n = v.trim_start_matches('+').trim_start_matches('-').parse().unwrap_or(10);
        } else if a == "-f" || a == "-F" {
            // no follow in sim
        } else if !a.starts_with('-') || a == "-" {
            files.push(a);
        }
    }
    let (data, _e) = read_inputs(interp, &files, &stdin);
    let lines: Vec<&str> = String::from_utf8_lossy(&data).lines().map(|s| s.to_string()).collect::<Vec<_>>().leak().iter().map(|s| s.as_str()).collect();
    if from_start {
        for l in lines.iter().skip(n.saturating_sub(1)) {
            wln(out, l);
        }
    } else {
        let start = lines.len().saturating_sub(n);
        for l in &lines[start..] {
            wln(out, l);
        }
    }
    0
}

fn bi_wc(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let (cl, cw, cc) = (flags.contains(&'l'), flags.contains(&'w'), flags.contains(&'c') || flags.contains(&'m'));
    let none = !cl && !cw && !cc;
    let print_one = |data: &[u8], out: Out, label: &str| {
        let s = String::from_utf8_lossy(data);
        let lines = s.matches('\n').count();
        let words = s.split_whitespace().count();
        let chars = data.len();
        let mut parts = Vec::new();
        if cl || none {
            parts.push(format!("{:>7}", lines));
        }
        if cw || none {
            parts.push(format!("{:>7}", words));
        }
        if cc || none {
            parts.push(format!("{:>7}", chars));
        }
        let mut line = parts.join(" ");
        if !label.is_empty() {
            line.push(' ');
            line.push_str(label);
        }
        wln(out, line.trim_start());
    };
    if ops.is_empty() {
        print_one(&stdin, out, "");
    } else {
        for f in &ops {
            match interp.vfs.read(&interp.cwd, f) {
                Ok(d) => print_one(&d, out, f),
                Err(_) => return 1,
            }
        }
    }
    0
}

fn bi_sort(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let numeric = flags.contains(&'n');
    let reverse = flags.contains(&'r');
    let unique = flags.contains(&'u');
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let mut lines: Vec<String> = String::from_utf8_lossy(&data).lines().map(|s| s.to_string()).collect();
    if numeric {
        lines.sort_by(|a, b| {
            let pa: f64 = a.trim().split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let pb: f64 = b.trim().split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        lines.sort();
    }
    if reverse {
        lines.reverse();
    }
    if unique {
        lines.dedup();
    }
    for l in lines {
        wln(out, &l);
    }
    0
}

fn bi_uniq(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let count = flags.contains(&'c');
    let only_dup = flags.contains(&'d');
    let only_uniq = flags.contains(&'u');
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let lines: Vec<String> = String::from_utf8_lossy(&data).lines().map(|s| s.to_string()).collect();
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j] == lines[i] {
            j += 1;
        }
        let n = j - i;
        let show = (!only_dup && !only_uniq) || (only_dup && n > 1) || (only_uniq && n == 1);
        if show {
            if count {
                wln(out, &format!("{:>7} {}", n, lines[i]));
            } else {
                wln(out, &lines[i]);
            }
        }
        i = j;
    }
    0
}

fn bi_cut(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let mut delim = '\t';
    let mut fields: Option<String> = None;
    let mut chars_spec: Option<String> = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(d) = a.strip_prefix("-d") {
            delim = if d.is_empty() { it.next().and_then(|s| s.chars().next()).unwrap_or('\t') } else { d.chars().next().unwrap_or('\t') };
        } else if let Some(f) = a.strip_prefix("-f") {
            fields = Some(if f.is_empty() { it.next().cloned().unwrap_or_default() } else { f.to_string() });
        } else if let Some(c) = a.strip_prefix("-c") {
            chars_spec = Some(if c.is_empty() { it.next().cloned().unwrap_or_default() } else { c.to_string() });
        } else if !a.starts_with('-') {
            files.push(a);
        }
    }
    let (data, _e) = read_inputs(interp, &files, &stdin);
    let parse_ranges = |spec: &str, max: usize| -> Vec<usize> {
        let mut idx = Vec::new();
        for part in spec.split(',') {
            if let Some((a, b)) = part.split_once('-') {
                let lo: usize = a.parse().unwrap_or(1);
                let hi: usize = if b.is_empty() { max } else { b.parse().unwrap_or(max) };
                for k in lo..=hi.min(max) {
                    idx.push(k);
                }
            } else if let Ok(k) = part.parse() {
                idx.push(k);
            }
        }
        idx
    };
    for line in String::from_utf8_lossy(&data).lines() {
        if let Some(spec) = &fields {
            let parts: Vec<&str> = line.split(delim).collect();
            if !line.contains(delim) {
                wln(out, line);
                continue;
            }
            let idx = parse_ranges(spec, parts.len());
            let selected: Vec<&str> = idx.iter().filter_map(|k| parts.get(k - 1).copied()).collect();
            wln(out, &selected.join(&delim.to_string()));
        } else if let Some(spec) = &chars_spec {
            let chars: Vec<char> = line.chars().collect();
            let idx = parse_ranges(spec, chars.len());
            let selected: String = idx.iter().filter_map(|k| chars.get(k - 1)).collect();
            wln(out, &selected);
        }
    }
    0
}

fn bi_tr(args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let delete = flags.contains(&'d');
    let squeeze = flags.contains(&'s');
    let complement = flags.contains(&'c');
    let set1 = ops.first().map(|s| expand_tr_set(s)).unwrap_or_default();
    let set2 = ops.get(1).map(|s| expand_tr_set(s)).unwrap_or_default();
    let input = String::from_utf8_lossy(&stdin).into_owned();
    let mut result = String::new();
    if delete {
        for c in input.chars() {
            let in_set = set1.contains(&c);
            if in_set != complement {
                continue;
            }
            result.push(c);
        }
    } else {
        let mut last = None;
        for c in input.chars() {
            let mapped = if let Some(pos) = set1.iter().position(|x| *x == c) {
                set2.get(pos).copied().or_else(|| set2.last().copied()).unwrap_or(c)
            } else {
                c
            };
            if squeeze && Some(mapped) == last && set2.contains(&mapped) {
                continue;
            }
            result.push(mapped);
            last = Some(mapped);
        }
    }
    w(out, &result);
    0
}

fn expand_tr_set(s: &str) -> Vec<char> {
    // handle ranges like a-z and classes [:digit:] minimally
    let mut out = Vec::new();
    let s = s
        .replace("[:digit:]", "0123456789")
        .replace("[:lower:]", "abcdefghijklmnopqrstuvwxyz")
        .replace("[:upper:]", "ABCDEFGHIJKLMNOPQRSTUVWXYZ")
        .replace("[:alpha:]", "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
        .replace("[:space:]", " \t\n\r");
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            let (lo, hi) = (chars[i], chars[i + 2]);
            for c in lo..=hi {
                out.push(c);
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn bi_rev(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    for line in String::from_utf8_lossy(&data).lines() {
        wln(out, &line.chars().rev().collect::<String>());
    }
    0
}

fn bi_nl(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let mut n = 1;
    for line in String::from_utf8_lossy(&data).lines() {
        if line.is_empty() {
            wln(out, "");
        } else {
            wln(out, &format!("{:>6}\t{}", n, line));
            n += 1;
        }
    }
    0
}

fn bi_seq(args: &[String], out: Out) -> i32 {
    let nums: Vec<f64> = args.iter().filter_map(|a| a.parse().ok()).collect();
    let (start, step, end) = match nums.len() {
        1 => (1.0, 1.0, nums[0]),
        2 => (nums[0], 1.0, nums[1]),
        3 => (nums[0], nums[1], nums[2]),
        _ => return 1,
    };
    let mut x = start;
    let int = start.fract() == 0.0 && step.fract() == 0.0 && end.fract() == 0.0;
    if step > 0.0 {
        while x <= end + 1e-9 {
            wln(out, &fmt_num(x, int));
            x += step;
        }
    } else if step < 0.0 {
        while x >= end - 1e-9 {
            wln(out, &fmt_num(x, int));
            x += step;
        }
    }
    0
}

fn fmt_num(x: f64, int: bool) -> String {
    if int {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

fn bi_paste(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let mut delim = '\t';
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-d" {
            delim = it.next().and_then(|s| s.chars().next()).unwrap_or('\t');
        } else if let Some(d) = a.strip_prefix("-d") {
            delim = d.chars().next().unwrap_or('\t');
        } else {
            files.push(a.clone());
        }
    }
    let columns: Vec<Vec<String>> = files
        .iter()
        .map(|f| {
            if f == "-" {
                lines_of(&stdin)
            } else {
                interp.vfs.read(&interp.cwd, f).map(|d| lines_of(&d)).unwrap_or_default()
            }
        })
        .collect();
    let max = columns.iter().map(|c| c.len()).max().unwrap_or(0);
    for i in 0..max {
        let row: Vec<String> = columns.iter().map(|c| c.get(i).cloned().unwrap_or_default()).collect();
        wln(out, &row.join(&delim.to_string()));
    }
    0
}

fn passthrough(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    out.extend_from_slice(&data);
    0
}

fn bi_xargs(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    // xargs [-n N] [-I {}] cmd... : run cmd with stdin tokens appended
    let mut i = 0;
    let mut replace: Option<String> = None;
    let mut nper: Option<usize> = None;
    while i < args.len() {
        match args[i].as_str() {
            "-I" => {
                replace = args.get(i + 1).cloned();
                i += 2;
            }
            "-n" => {
                nper = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 2;
            }
            "-0" | "-r" => {
                i += 1;
            }
            _ => break,
        }
    }
    let cmd: Vec<String> = args[i..].to_vec();
    if cmd.is_empty() {
        return 0;
    }
    let tokens: Vec<String> = String::from_utf8_lossy(&stdin).split_whitespace().map(|s| s.to_string()).collect();
    let mut status = 0;
    let run_one = |interp: &mut Interp, argv: &[String], out: Out, err: Out| -> i32 {
        run(interp, argv, Vec::new(), out, err)
    };
    if let Some(ph) = replace {
        for t in &tokens {
            let argv: Vec<String> = cmd.iter().map(|c| c.replace(&ph, t)).collect();
            status = run_one(interp, &argv, out, err);
        }
    } else {
        let chunk = nper.unwrap_or(tokens.len().max(1));
        for batch in tokens.chunks(chunk.max(1)) {
            let mut argv = cmd.clone();
            argv.extend(batch.iter().cloned());
            status = run_one(interp, &argv, out, err);
        }
    }
    status
}

fn bi_comm(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    if ops.len() < 2 {
        ewln(err, "comm: missing operand");
        return 1;
    }
    let a = interp.vfs.read(&interp.cwd, ops[0]).map(|d| lines_of(&d)).unwrap_or_default();
    let b = interp.vfs.read(&interp.cwd, ops[1]).map(|d| lines_of(&d)).unwrap_or_default();
    let (s1, s2, s3) = (!flags.contains(&'1'), !flags.contains(&'2'), !flags.contains(&'3'));
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && (j >= b.len() || a[i] < b[j]) {
            if s1 {
                wln(out, &a[i]);
            }
            i += 1;
        } else if j < b.len() && (i >= a.len() || b[j] < a[i]) {
            if s2 {
                wln(out, &format!("\t{}", b[j]));
            }
            j += 1;
        } else {
            if s3 {
                wln(out, &format!("\t\t{}", a[i]));
            }
            i += 1;
            j += 1;
        }
    }
    0
}

fn bi_diff(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    if ops.len() < 2 {
        ewln(err, "diff: missing operand");
        return 2;
    }
    let a = interp.vfs.read_string(&interp.cwd, ops[0]).unwrap_or_default();
    let b = interp.vfs.read_string(&interp.cwd, ops[1]).unwrap_or_default();
    if a == b {
        0
    } else {
        // minimal unified-ish output (not a real LCS diff)
        let al: Vec<&str> = a.lines().collect();
        let bl: Vec<&str> = b.lines().collect();
        for (i, line) in al.iter().enumerate() {
            if bl.get(i) != Some(line) {
                wln(out, &format!("< {line}"));
            }
        }
        wln(out, "---");
        for (i, line) in bl.iter().enumerate() {
            if al.get(i) != Some(line) {
                wln(out, &format!("> {line}"));
            }
        }
        1
    }
}

fn bi_cmp(interp: &mut Interp, args: &[String], err: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    if ops.len() < 2 {
        return 2;
    }
    let a = interp.vfs.read(&interp.cwd, ops[0]).unwrap_or_default();
    let b = interp.vfs.read(&interp.cwd, ops[1]).unwrap_or_default();
    if a == b {
        0
    } else {
        ewln(err, &format!("{} {} differ", ops[0], ops[1]));
        1
    }
}

fn bi_grep(interp: &mut Interp, cmd: &str, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    let mut ignore_case = false;
    let mut invert = false;
    let mut count = false;
    let mut line_num = false;
    let mut files_with = false;
    let mut only_match = false;
    let mut recursive = false;
    let mut extended = cmd == "egrep";
    let mut fixed = cmd == "fgrep";
    let mut word = false;
    let mut quiet = false;
    let mut after = 0usize;
    let mut pattern: Option<String> = None;
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.starts_with('-') && a.len() > 1 && a != "-" {
            if let Some(p) = a.strip_prefix("-e") {
                if p.is_empty() {
                    pattern = it.next().cloned();
                } else {
                    pattern = Some(p.to_string());
                }
                continue;
            }
            if let Some(n) = a.strip_prefix("-A") {
                after = if n.is_empty() { it.next().and_then(|s| s.parse().ok()).unwrap_or(0) } else { n.parse().unwrap_or(0) };
                continue;
            }
            for c in a[1..].chars() {
                match c {
                    'i' => ignore_case = true,
                    'v' => invert = true,
                    'c' => count = true,
                    'n' => line_num = true,
                    'l' => files_with = true,
                    'o' => only_match = true,
                    'r' | 'R' => recursive = true,
                    'E' => extended = true,
                    'F' => fixed = true,
                    'w' => word = true,
                    'q' => quiet = true,
                    'h' | 's' | 'a' => {}
                    _ => {}
                }
            }
        } else if pattern.is_none() {
            pattern = Some(a.clone());
        } else {
            files.push(a.clone());
        }
    }
    let _ = extended;
    let Some(pat) = pattern else {
        ewln(err, "grep: no pattern");
        return 2;
    };
    let mut pat_re = if fixed { regex::escape(&pat) } else { pat.clone() };
    if word {
        pat_re = format!(r"\b(?:{pat_re})\b");
    }
    let re = match regex::RegexBuilder::new(&pat_re).case_insensitive(ignore_case).build() {
        Ok(r) => r,
        Err(_) => {
            // fall back to fixed-string
            regex::RegexBuilder::new(&regex::escape(&pat)).case_insensitive(ignore_case).build().unwrap()
        }
    };

    // gather (label, data)
    let mut inputs: Vec<(String, Vec<u8>)> = Vec::new();
    if files.is_empty() {
        inputs.push((String::new(), stdin));
    } else if recursive {
        for f in &files {
            let abs = crate::vfs::resolve_against(&interp.cwd, f);
            for p in interp.vfs.walk(&abs) {
                if interp.vfs.is_file("/", &p) {
                    inputs.push((p.clone(), interp.vfs.read("/", &p).unwrap_or_default()));
                }
            }
        }
    } else {
        for f in &files {
            match interp.vfs.read(&interp.cwd, f) {
                Ok(d) => inputs.push((f.clone(), d)),
                Err(e) => ewln(err, &format!("grep: {e}")),
            }
        }
    }
    let multi = inputs.len() > 1 || recursive;
    let mut total_matches = 0;
    for (label, data) in &inputs {
        let mut file_count = 0;
        let mut matched_file = false;
        for (lineno, line) in String::from_utf8_lossy(data).lines().enumerate() {
            let is_match = re.is_match(line) ^ invert;
            if is_match {
                matched_file = true;
                file_count += 1;
                total_matches += 1;
                if quiet || count || files_with {
                    continue;
                }
                let mut prefix = String::new();
                if multi && !label.is_empty() {
                    prefix.push_str(label);
                    prefix.push(':');
                }
                if line_num {
                    prefix.push_str(&format!("{}:", lineno + 1));
                }
                if only_match && !invert {
                    for m in re.find_iter(line) {
                        wln(out, &format!("{prefix}{}", m.as_str()));
                    }
                } else {
                    wln(out, &format!("{prefix}{line}"));
                }
                let _ = after;
            }
        }
        if count {
            if multi && !label.is_empty() {
                wln(out, &format!("{label}:{file_count}"));
            } else {
                wln(out, &file_count.to_string());
            }
        }
        if files_with && matched_file {
            wln(out, label);
        }
    }
    if total_matches > 0 {
        0
    } else {
        1
    }
}

fn bi_sed(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    let mut in_place = false;
    let mut quiet = false;
    let mut scripts: Vec<String> = Vec::new();
    let mut extended = false;
    let mut files = Vec::new();
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-i" || a.starts_with("-i") {
            in_place = true;
            if a.len() > 2 { /* suffix ignored */ }
        } else if a == "-n" {
            quiet = true;
        } else if a == "-r" || a == "-E" {
            extended = true;
        } else if a == "-e" {
            if let Some(s) = it.next() {
                scripts.push(s.clone());
            }
        } else if let Some(s) = a.strip_prefix("-e") {
            scripts.push(s.to_string());
        } else if a == "--" {
            continue;
        } else if scripts.is_empty() && !a.starts_with('-') {
            scripts.push(a.clone());
        } else {
            files.push(a.clone());
        }
    }
    let _ = extended;
    let commands: Vec<SedCmd> = scripts.iter().flat_map(|s| parse_sed_script(s)).collect();

    let process = |text: &str| -> String {
        let mut result = String::new();
        for line in text.split_inclusive('\n') {
            let had_nl = line.ends_with('\n');
            let mut content = line.trim_end_matches('\n').to_string();
            let mut deleted = false;
            let mut printed_extra = Vec::new();
            for cmd in &commands {
                match cmd {
                    SedCmd::Subst { re, rep, global, nth, print, ignore } => {
                        let _ = ignore;
                        content = sed_subst(re, rep, &content, *global, *nth);
                        if *print {
                            printed_extra.push(content.clone());
                        }
                    }
                    SedCmd::Delete => {
                        deleted = true;
                    }
                    SedCmd::Print => {
                        printed_extra.push(content.clone());
                    }
                }
            }
            if !quiet && !deleted {
                result.push_str(&content);
                if had_nl {
                    result.push('\n');
                }
            }
            for p in printed_extra {
                result.push_str(&p);
                result.push('\n');
            }
        }
        result
    };

    if in_place && !files.is_empty() {
        let cwd = interp.cwd.clone();
        for f in &files {
            match interp.vfs.read_string(&cwd, f) {
                Ok(text) => {
                    let new = process(&text);
                    let _ = interp.vfs.write(&cwd, f, new.as_bytes(), 0o644);
                }
                Err(e) => {
                    ewln(err, &format!("sed: can't read {f}: {e}"));
                    return 1;
                }
            }
        }
        0
    } else {
        let (data, _e) = read_inputs(interp, &files.iter().collect::<Vec<_>>(), &stdin);
        let text = String::from_utf8_lossy(&data);
        w(out, &process(&text));
        0
    }
}

enum SedCmd {
    Subst { re: regex::Regex, rep: String, global: bool, nth: usize, print: bool, ignore: bool },
    Delete,
    Print,
}

fn parse_sed_script(s: &str) -> Vec<SedCmd> {
    let mut cmds = Vec::new();
    for part in s.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // strip a leading line address like `3` or `/re/` (best-effort: ignore numeric/`$`)
        let body = part.trim_start_matches(|c: char| c.is_ascii_digit() || c == '$' || c == ',' || c == ' ');
        if let Some(rest) = body.strip_prefix('s') {
            if let Some(cmd) = parse_subst(rest) {
                cmds.push(cmd);
            }
        } else if body == "d" {
            cmds.push(SedCmd::Delete);
        } else if body == "p" {
            cmds.push(SedCmd::Print);
        }
    }
    cmds
}

fn parse_subst(rest: &str) -> Option<SedCmd> {
    let delim = rest.chars().next()?;
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 1;
    let mut fields = vec![String::new(), String::new(), String::new()];
    let mut fi = 0;
    while i < chars.len() && fi < 3 {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            // keep escapes; but \<delim> becomes literal delim
            if chars[i + 1] == delim {
                fields[fi].push(delim);
            } else {
                fields[fi].push('\\');
                fields[fi].push(chars[i + 1]);
            }
            i += 2;
            continue;
        }
        if c == delim {
            fi += 1;
            i += 1;
            continue;
        }
        fields[fi].push(c);
        i += 1;
    }
    let (pat, rep, flags) = (&fields[0], &fields[1], &fields[2]);
    let global = flags.contains('g');
    let ignore = flags.contains('i') || flags.contains('I');
    let print = flags.contains('p');
    let nth: usize = flags.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
    let re = regex::RegexBuilder::new(pat).case_insensitive(ignore).build().ok()?;
    // convert sed replacement backrefs \1 -> ${1}
    let rep = convert_sed_replacement(rep);
    Some(SedCmd::Subst { re, rep, global, nth, print, ignore })
}

fn convert_sed_replacement(rep: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = rep.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
            out.push_str(&format!("${{{}}}", chars[i + 1]));
            i += 2;
        } else if chars[i] == '&' {
            out.push_str("${0}");
            i += 1;
        } else if chars[i] == '$' {
            out.push_str("$$");
            i += 1;
        } else if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                c => out.push(c),
            }
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn sed_subst(re: &regex::Regex, rep: &str, text: &str, global: bool, nth: usize) -> String {
    if global && nth == 0 {
        re.replace_all(text, rep).into_owned()
    } else if nth > 0 {
        let mut count = 0;
        re.replace_all(text, |caps: &regex::Captures| {
            count += 1;
            if count == nth || (global && count >= nth) {
                expand_caps(rep, caps)
            } else {
                caps[0].to_string()
            }
        })
        .into_owned()
    } else {
        re.replace(text, rep).into_owned()
    }
}

fn expand_caps(rep: &str, caps: &regex::Captures) -> String {
    let mut out = String::new();
    caps.expand(rep, &mut out);
    out
}

// ---- hashing / encoding ----

fn bi_hash(interp: &mut Interp, algo: &str, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (flags, ops, _l) = split_flags(args);
    let check = flags.contains(&'c');
    let _ = check;
    let compute = |data: &[u8]| -> String {
        match algo {
            "sha256" => crate::hashes::sha256_hex(data),
            "sha1" => crate::hashes::sha1_hex(data),
            "sha512" => crate::hashes::sha512_hex(data),
            "md5" => crate::hashes::md5_hex(data),
            "crc32" => {
                let (c, n) = crate::hashes::cksum(data);
                return format!("{c} {n}");
            }
            _ => String::new(),
        }
    };
    if ops.is_empty() {
        wln(out, &format!("{}  -", compute(&stdin)));
    } else {
        for f in &ops {
            match interp.vfs.read(&interp.cwd, f) {
                Ok(d) => wln(out, &format!("{}  {}", compute(&d), f)),
                Err(_) => return 1,
            }
        }
    }
    0
}

fn bi_base64(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, _err: Out) -> i32 {
    let (flags, ops, long) = split_flags(args);
    let decode = flags.contains(&'d') || long.iter().any(|(k, _)| *k == "decode");
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    if decode {
        let s: String = String::from_utf8_lossy(&data).chars().filter(|c| !c.is_whitespace()).collect();
        match crate::hashes::base64_decode(&s) {
            Some(d) => out.extend_from_slice(&d),
            None => return 1,
        }
    } else {
        let encoded = crate::hashes::base64_encode(&data);
        wln(out, &encoded);
    }
    0
}

fn bi_hexdump(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
    wln(out, &hex);
    0
}

fn bi_strings(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out) -> i32 {
    let (_f, ops, _l) = split_flags(args);
    let (data, _e) = read_inputs(interp, &ops, &stdin);
    let mut cur = String::new();
    for &b in &data {
        if b.is_ascii_graphic() || b == b' ' {
            cur.push(b as char);
        } else {
            if cur.len() >= 4 {
                wln(out, &cur);
            }
            cur.clear();
        }
    }
    if cur.len() >= 4 {
        wln(out, &cur);
    }
    0
}

fn bi_expr(args: &[String], out: Out) -> i32 {
    // minimal: arithmetic and string length
    if args.len() == 2 && args[0] == "length" {
        wln(out, &args[1].chars().count().to_string());
        return 0;
    }
    let joined = args.join(" ");
    // try arithmetic
    let mut i = Interp::new();
    let v = crate::expand::eval_arith(&mut i, &joined);
    wln(out, &v.to_string());
    if v == 0 {
        1
    } else {
        0
    }
}

fn bi_bc(interp: &mut Interp, stdin: Vec<u8>, out: Out) -> i32 {
    for line in String::from_utf8_lossy(&stdin).lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v = crate::expand::eval_arith(interp, line);
        wln(out, &v.to_string());
    }
    0
}

fn bi_date(interp: &mut Interp, args: &[String], out: Out) -> i32 {
    let secs = interp.clock.unix_secs();
    // handle +FORMAT and %s
    if let Some(fmt) = args.iter().find(|a| a.starts_with('+')) {
        let f = &fmt[1..];
        if f.contains("%s") {
            wln(out, &f.replace("%s", &secs.to_string()));
        } else {
            wln(out, &format_date(secs, f));
        }
    } else {
        wln(out, &format_date(secs, "%a %b %e %H:%M:%S UTC %Y"));
    }
    0
}

fn format_date(secs: u64, fmt: &str) -> String {
    // convert unix secs to UTC fields (proleptic Gregorian)
    let days = (secs / 86400) as i64;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    fmt.replace("%Y", &format!("{y:04}"))
        .replace("%m", &format!("{mo:02}"))
        .replace("%d", &format!("{d:02}"))
        .replace("%H", &format!("{h:02}"))
        .replace("%M", &format!("{mi:02}"))
        .replace("%S", &format!("{s:02}"))
        .replace("%e", &format!("{d:2}"))
        .replace("%a", "Mon")
        .replace("%b", "Jan")
        .replace("%s", &secs.to_string())
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn try_exec_script(interp: &mut Interp, name: &str, args: &[String], stdin: &[u8], out: Out, err: Out) -> Option<i32> {
    // run a script file in the VFS if it exists and looks like a shell script
    let path = if name.contains('/') {
        crate::vfs::resolve_against(&interp.cwd, name)
    } else {
        return None;
    };
    let data = interp.vfs.read("/", &path).ok()?;
    let text = String::from_utf8_lossy(&data);
    let saved_pos = std::mem::replace(&mut interp.positional, args.to_vec());
    let first = text.lines().next().unwrap_or("");
    let code = if first.starts_with("#!") && (first.contains("python")) {
        // python script
        let mut a = vec![name.to_string()];
        a.extend(args.iter().cloned());
        crate::python::run_python(interp, &a, stdin.to_vec(), out, err)
    } else {
        interp.run_script_into(&text, out, err)
    };
    interp.positional = saved_pos;
    Some(code)
}

const KNOWN_COMMANDS: &[&str] = &[
    "cat", "echo", "printf", "ls", "mkdir", "rmdir", "rm", "cp", "mv", "touch", "ln", "chmod",
    "chown", "head", "tail", "sort", "uniq", "cut", "tr", "grep", "sed", "awk", "find", "seq",
    "wc", "cd", "pwd", "test", "true", "false", "basename", "dirname", "realpath", "readlink",
    "date", "sleep", "env", "export", "python3", "python", "jq", "base64", "sha256sum", "curl",
    "tee", "xargs", "diff", "comm", "sort", "stat", "file", "mktemp",
];

/// Provide `run_script_into` for nested execution (source, eval, scripts).
impl Interp {
    pub fn run_script_into(&mut self, src: &str, out: Out, err: Out) -> i32 {
        let ast = crate::shell::parse(src);
        let r = self.returning.take();
        let code = crate::exec::exec(self, &ast, Vec::new(), out, err);
        if r.is_some() {
            self.returning = r;
        }
        code
    }
}
