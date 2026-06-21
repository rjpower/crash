//! Embedded Python engine.
//!
//! Strategy: the VFS is serialized into the RustPython sandbox as a dict
//! (`__VFS_FILES: {path: bytes}` + `__VFS_DIRS`), and a pure-Python prelude installs an
//! `open()`/`os`/`pathlib`/`glob` shim over that dict plus a meta-path importer that loads
//! sibling `.py` modules out of the VFS (needed for `from grader import grade`). After
//! execution we read the dict back and reconcile changes into the real VFS. This keeps the
//! whole thing in-memory and deterministic — no real filesystem, no temp dirs.

use crate::interp::Interp;

type Out<'a> = &'a mut Vec<u8>;

fn ewln(err: Out, s: &str) {
    err.extend_from_slice(s.as_bytes());
    err.push(b'\n');
}

/// `python3 [opts] [script] [args]` (code may also arrive on stdin via a heredoc).
pub fn run_python(interp: &mut Interp, args: &[String], stdin: Vec<u8>, out: Out, err: Out) -> i32 {
    // Parse the python command line.
    let mut code: Option<String> = None;
    let mut script: Option<String> = None;
    let mut module: Option<String> = None;
    let mut prog_args: Vec<String> = Vec::new();
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-c" => {
                code = args.get(i + 1).cloned();
                prog_args = args.get(i + 2..).map(|s| s.to_vec()).unwrap_or_default();
                break;
            }
            "-m" => {
                module = args.get(i + 1).cloned();
                prog_args = args.get(i + 2..).map(|s| s.to_vec()).unwrap_or_default();
                break;
            }
            "-" => {
                script = Some("-".to_string());
                prog_args = args.get(i + 1..).map(|s| s.to_vec()).unwrap_or_default();
                break;
            }
            "-u" | "-B" | "-E" | "-s" | "-I" | "-O" | "-q" | "-X" => {
                if a == "-X" {
                    i += 1;
                }
            }
            s if s.starts_with('-') => {}
            s => {
                script = Some(s.to_string());
                prog_args = args.get(i + 1..).map(|s| s.to_vec()).unwrap_or_default();
                break;
            }
        }
        i += 1;
    }

    // `-m pytest <files>` → route to the pytest runner.
    if module.as_deref() == Some("pytest") {
        return run_pytest(interp, &prog_args, out, err);
    }
    if let Some(m) = module {
        interp.note_unsupported(&format!("python -m {m}"));
        ewln(err, &format!("python: module {m} not available in sandbox"));
        return 1;
    }

    let (user_src, file_name, argv0) = if let Some(c) = code {
        (c, "<string>".to_string(), "-c".to_string())
    } else if let Some(s) = script {
        if s == "-" {
            (String::from_utf8_lossy(&stdin).into_owned(), "<stdin>".to_string(), "-".to_string())
        } else {
            match interp.vfs.read_string(&interp.cwd, &s) {
                Ok(src) => (src, s.clone(), s.clone()),
                Err(_) => {
                    ewln(err, &format!("python: can't open file '{s}': No such file or directory"));
                    return 2;
                }
            }
        }
    } else if !stdin.is_empty() {
        // `python3 << EOF ... EOF`
        (String::from_utf8_lossy(&stdin).into_owned(), "<stdin>".to_string(), "-".to_string())
    } else {
        // interactive — nothing to do
        return 0;
    };

    let mut argv = vec![argv0];
    argv.extend(prog_args);
    let driver = PYTHON_DRIVER_EXEC.to_string();
    exec_program(interp, &user_src, &file_name, &argv, &driver, out, err)
}

/// `pytest <files>` — load each test module from the VFS and run its `test_*` functions and
/// `Test*` classes. The verifier modules themselves write `/logs/verifier/reward.txt`.
pub fn run_pytest(interp: &mut Interp, args: &[String], out: Out, err: Out) -> i32 {
    // accept only real test targets (.py files or directories), so flag-values like the path
    // after `--ctrf` aren't mistaken for test files.
    let files: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .filter(|a| a.ends_with(".py") || !a.contains('.'))
        .cloned()
        .collect();
    let targets = if files.is_empty() { vec![".".to_string()] } else { files };
    let mut overall = 0;
    for t in &targets {
        // resolve to concrete test files
        let test_files = collect_test_files(interp, t);
        if test_files.is_empty() {
            ewln(err, &format!("pytest: no tests found at {t}"));
            overall = overall.max(4);
            continue;
        }
        for tf in test_files {
            let src = match interp.vfs.read_string(&interp.cwd, &tf) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let argv = vec!["pytest".to_string(), tf.clone()];
            let rc = exec_program(interp, &src, &tf, &argv, PYTHON_DRIVER_PYTEST, out, err);
            if rc != 0 {
                overall = 1;
            }
        }
    }
    overall
}

fn collect_test_files(interp: &Interp, target: &str) -> Vec<String> {
    let abs = crate::vfs::resolve_against(&interp.cwd, target);
    if interp.vfs.is_file("/", &abs) {
        return vec![abs];
    }
    if interp.vfs.is_dir("/", &abs) {
        let mut v: Vec<String> = interp
            .vfs
            .walk(&abs)
            .into_iter()
            .filter(|p| {
                let b = crate::vfs::basename(p);
                interp.vfs.is_file("/", p) && (b.starts_with("test_") || b.ends_with("_test.py")) && b.ends_with(".py")
            })
            .collect();
        v.sort();
        return v;
    }
    Vec::new()
}

#[cfg(not(feature = "python"))]
fn exec_program(
    interp: &mut Interp,
    _src: &str,
    _file: &str,
    _argv: &[String],
    _driver: &str,
    _out: Out,
    err: Out,
) -> i32 {
    interp.note_unsupported("python(engine-not-built)");
    ewln(err, "python: interpreter not compiled in (build with --features python)");
    1
}

#[cfg(feature = "python")]
fn exec_program(
    interp: &mut Interp,
    src: &str,
    file: &str,
    argv: &[String],
    driver: &str,
    out: Out,
    err: Out,
) -> i32 {
    imp::exec_program(interp, src, file, argv, driver, out, err)
}

// The Python driver that runs an ordinary script.
// NOTE: this Python lives in a real .py file (syntax-checked by the `python_files_compile`
// test) and is embedded verbatim. Do not edit the Python here — edit driver_exec.py.
const PYTHON_DRIVER_EXEC: &str = include_str!("driver_exec.py");

// The Python driver that runs a file as a pytest module.
// Source of truth: src/python/driver_pytest.py (embedded verbatim via include_str!).
const PYTHON_DRIVER_PYTEST: &str = include_str!("driver_pytest.py");

#[cfg(feature = "python")]
mod imp {
    use super::*;
    use rustpython_vm as vm;
    use std::collections::{HashMap, HashSet};

    pub fn exec_program(
        interp: &mut Interp,
        src: &str,
        file: &str,
        argv: &[String],
        driver: &str,
        out: Out,
        err: Out,
    ) -> i32 {
        // Snapshot VFS files and dirs (so we can diff afterwards).
        let (files_before, dirs_before) = snapshot(interp);
        let env = interp.child_env();
        let cwd = interp.cwd.clone();
        let pypath = build_pypath(interp, file, &env);

        // A brand-new interpreter per invocation. This guarantees perfect isolation between
        // programs (no leftover user globals, `sys.modules`, `builtins`/stdlib mutations, or
        // cwd bleed from a previous program). With the `freeze-stdlib` feature the stdlib is
        // baked into the binary, so `init_stdlib()` is cheap — reusing a warm interpreter was
        // measured to save little wall-time while opening real cross-program leakage (e.g. a
        // task mutating `builtins`); correctness wins, so we keep this fresh per call.
        let interp_py = rustpython::InterpreterConfig::new().init_stdlib().interpreter();
        let mut exit_code = 0i32;
        let mut stdout_s = String::new();
        let mut stderr_s = String::new();
        let mut files_after: HashMap<String, Vec<u8>> = files_before.clone();
        let mut dirs_after: Vec<String> = dirs_before.iter().cloned().collect();

        interp_py.enter(|vm| {
            let scope = vm.new_scope_with_builtins();
            let g = &scope.globals;

            // ---- inject bridge globals ----
            let files_dict = vm.ctx.new_dict();
            for (k, v) in &files_before {
                let _ = files_dict.set_item(k, vm.ctx.new_bytes(v.clone()).into(), vm);
            }
            let _ = g.set_item("__VFS_FILES", files_dict.into(), vm);

            let dirs_set: Vec<vm::PyObjectRef> =
                dirs_before.iter().map(|d| vm.ctx.new_str(d.clone()).into()).collect();
            let _ = g.set_item("__VFS_DIRS_INIT", vm.ctx.new_list(dirs_set).into(), vm);

            let _ = g.set_item("__VFS_CWD_INIT", vm.ctx.new_str(cwd.clone()).into(), vm);

            let env_dict = vm.ctx.new_dict();
            for (k, v) in &env {
                let _ = env_dict.set_item(k.as_str(), vm.ctx.new_str(v.clone()).into(), vm);
            }
            let _ = g.set_item("__VFS_ENV", env_dict.into(), vm);

            let pp: Vec<vm::PyObjectRef> =
                pypath.iter().map(|d| vm.ctx.new_str(d.clone()).into()).collect();
            let _ = g.set_item("__PYPATH", vm.ctx.new_list(pp).into(), vm);

            let av: Vec<vm::PyObjectRef> =
                argv.iter().map(|a| vm.ctx.new_str(a.clone()).into()).collect();
            let _ = g.set_item("__ARGV", vm.ctx.new_list(av).into(), vm);

            let _ = g.set_item("__USER_SRC", vm.ctx.new_str(src.to_owned()).into(), vm);
            let _ = g.set_item("__USER_FILE", vm.ctx.new_str(file.to_owned()).into(), vm);

            // ---- run prelude, then driver ----
            let program = format!("{PRELUDE}\n{driver}\n{POSTLUDE}");
            match vm.compile(&program, vm::compiler::Mode::Exec, "<sandbox>".to_owned()) {
                Ok(codeobj) => {
                    if let Err(e) = vm.run_code_obj(codeobj, scope.clone()) {
                        // a hard error escaped our try/except (e.g. prelude bug)
                        let mut s = String::new();
                        vm.write_exception(&mut s, &e).ok();
                        stderr_s.push_str(&s);
                        exit_code = 1;
                    }
                }
                Err(e) => {
                    stderr_s.push_str(&format!("SyntaxError: {e}\n"));
                    exit_code = 1;
                }
            }

            // ---- read back results (everything is marshaled as a string) ----
            let getstr = |name: &str| -> Option<String> {
                g.get_item(name, vm)
                    .ok()
                    .and_then(|o| o.downcast::<vm::builtins::PyStr>().ok())
                    .map(|s| s.as_str().to_owned())
            };
            if let Some(s) = getstr("__STDOUT") {
                stdout_s = s;
            }
            if let Some(s) = getstr("__STDERR") {
                stderr_s.push_str(&s);
            }
            if let Some(s) = getstr("__EXIT_S") {
                if let Ok(n) = s.trim().parse::<i32>() {
                    exit_code = n;
                }
            }
            if let Some(s) = getstr("__VFS_DIRS_S") {
                dirs_after = s.lines().map(|l| l.to_string()).filter(|l| !l.is_empty()).collect();
            }
            if let Some(json) = getstr("__VFS_DUMP_JSON") {
                if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(&json) {
                    let mut out = HashMap::new();
                    for (k, v) in map {
                        if let Some(b64) = v.as_str() {
                            if let Some(d) = crate::hashes::base64_decode(b64) {
                                out.insert(k, d);
                            }
                        }
                    }
                    files_after = out;
                }
            }
        });

        // ---- reconcile changes back into the VFS ----
        reconcile(interp, &files_before, &files_after, &dirs_before, &dirs_after);

        out.extend_from_slice(stdout_s.as_bytes());
        err.extend_from_slice(stderr_s.as_bytes());
        exit_code
    }

    fn snapshot(interp: &Interp) -> (HashMap<String, Vec<u8>>, HashSet<String>) {
        let mut files = HashMap::new();
        let mut dirs = HashSet::new();
        for (path, node) in interp.vfs.all_paths() {
            match &node.kind {
                crate::vfs::NodeKind::File(d) => {
                    files.insert(path.clone(), d.clone());
                }
                crate::vfs::NodeKind::Dir => {
                    dirs.insert(path.clone());
                }
                crate::vfs::NodeKind::Symlink(_) => {}
            }
        }
        (files, dirs)
    }

    fn build_pypath(interp: &Interp, file: &str, env: &HashMap<String, String>) -> Vec<String> {
        let mut p = Vec::new();
        p.push(interp.cwd.clone());
        if file.contains('/') {
            if let Some(d) = crate::vfs::parent_of(&crate::vfs::resolve_against(&interp.cwd, file)) {
                p.push(d);
            }
        }
        for common in ["/tests", "/app", "/workdir", "/workspace", "/solution", "/src"] {
            if interp.vfs.is_dir("/", common) {
                p.push(common.to_string());
            }
        }
        if let Some(pp) = env.get("PYTHONPATH") {
            for part in pp.split(':') {
                if !part.is_empty() {
                    p.push(part.to_string());
                }
            }
        }
        p.dedup();
        p
    }

    fn reconcile(
        interp: &mut Interp,
        before: &HashMap<String, Vec<u8>>,
        after: &HashMap<String, Vec<u8>>,
        dirs_before: &HashSet<String>,
        dirs_after: &[String],
    ) {
        // new dirs
        for d in dirs_after {
            if !dirs_before.contains(d) {
                interp.vfs.put_dir(d, 0o755);
            }
        }
        // writes / new files
        for (path, data) in after {
            match before.get(path) {
                Some(old) if old == data => {}
                _ => {
                    interp.vfs.put_file(path, data.clone(), 0o644);
                }
            }
        }
        // deletions
        for path in before.keys() {
            if !after.contains_key(path) {
                let _ = interp.vfs.remove_file("/", path);
            }
        }
    }
}

// ============ the Python prelude (pure-Python VFS shim) ============
#[cfg(feature = "python")]
const PRELUDE: &str = include_str!("prelude.py");

#[cfg(feature = "python")]
const POSTLUDE: &str = include_str!("postlude.py");

#[cfg(test)]
mod py_syntax_tests {
    //! Syntax-check the embedded Python sources (prelude / postlude / drivers) with the
    //! *real* CPython so a typo can't silently ship inside a Rust string literal. RustPython
    //! is lenient about some constructs, so we use the host `python3 -m py_compile`. The test
    //! is skipped (not failed) when `python3` is unavailable in the build/CI environment.
    use std::path::PathBuf;
    use std::process::Command;

    fn python3_available() -> bool {
        Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn python_files_compile() {
        if !python3_available() {
            eprintln!("python_files_compile: skipped (python3 not found)");
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/python");
        for name in ["prelude.py", "postlude.py", "driver_exec.py", "driver_pytest.py"] {
            let file = dir.join(name);
            assert!(file.exists(), "missing embedded Python file: {}", file.display());
            let out = Command::new("python3")
                .arg("-m")
                .arg("py_compile")
                .arg(&file)
                .output()
                .expect("failed to spawn python3");
            assert!(
                out.status.success(),
                "py_compile failed for {}:\n{}",
                file.display(),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
