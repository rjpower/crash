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
const PYTHON_DRIVER_EXEC: &str = r#"
_g = globals()
_g['__name__'] = '__main__'
_g['__file__'] = __USER_FILE
try:
    exec(compile(__USER_SRC, __USER_FILE, 'exec'), _g)
except SystemExit as _se:
    __EXIT = _se.code if isinstance(_se.code, int) else (0 if _se.code is None else 1)
except BaseException:
    import traceback as _tb
    _tb.print_exc()
    __EXIT = 1
"#;

// The Python driver that runs a file as a pytest module.
const PYTHON_DRIVER_PYTEST: &str = r#"
__EXIT = 0
try:
    _g = globals()
    exec(compile(__USER_SRC, __USER_FILE, 'exec'), _g)
    import sys as _sys
    import types as _types
    _fail = 0
    _ran = 0
    _tmpc = [0]
    def _params(_f):
        try:
            c = _f.__code__
            return list(c.co_varnames[:c.co_argcount])
        except Exception:
            return []
    def _builtin_fixture(_name):
        if _name == 'tmp_path':
            _tmpc[0] += 1
            _p = '/tmp/pytest-' + str(_tmpc[0])
            os.makedirs(_p, exist_ok=True)
            return _pathlib.Path(_p)
        if _name == 'tmp_path_factory':
            class _TPF:
                def mktemp(self, _n, numbered=True):
                    _tmpc[0] += 1
                    _q = '/tmp/' + str(_n) + str(_tmpc[0])
                    os.makedirs(_q, exist_ok=True)
                    return _pathlib.Path(_q)
            return _TPF()
        if _name in ('capsys', 'capfd', 'capsysbinary'):
            class _Cap:
                def readouterr(self):
                    class _R: pass
                    _r = _R(); _r.out = ''; _r.err = ''
                    return _r
            return _Cap()
        if _name == 'monkeypatch':
            class _MP:
                def setattr(self, *a, **k): pass
                def delattr(self, *a, **k): pass
                def setenv(self, _k, _v, prepend=None): os.environ[_k] = str(_v)
                def delenv(self, _k, raising=True): os.environ.pop(_k, None)
                def chdir(self, _p): os.chdir(str(_p))
                def syspath_prepend(self, _p): sys.path.insert(0, str(_p))
                def setitem(self, *a, **k): pass
                def delitem(self, *a, **k): pass
                def undo(self): pass
            return _MP()
        if _name == 'request':
            class _Req:
                param = None
                def getfixturevalue(self, _n): return _resolve(_n, [], set())
            return _Req()
        return None
    def _resolve(_name, _fins, _seen):
        if _name in _seen:
            return None
        _fn = _g.get(_name)
        if _fn is not None and getattr(_fn, '_pytest_fixture', False):
            _kw = {}
            for _an in _params(_fn):
                if _an in ('self', 'request'):
                    if _an == 'request':
                        _kw[_an] = _builtin_fixture('request')
                    continue
                _kw[_an] = _resolve(_an, _fins, _seen | {_name})
            _v = _fn(**_kw)
            if isinstance(_v, _types.GeneratorType):
                _fins.append(_v)
                return next(_v)
            return _v
        return _builtin_fixture(_name)
    def _call_with_fixtures(_fn):
        _fins = []
        _kw = {}
        for _an in _params(_fn):
            if _an == 'self':
                continue
            _kw[_an] = _resolve(_an, _fins, set())
        try:
            _fn(**_kw)
        finally:
            for _gen in reversed(_fins):
                try:
                    next(_gen)
                except StopIteration:
                    pass
                except Exception:
                    pass
    def _run_one(_fn, _label):
        global _fail, _ran
        _ran += 1
        try:
            _call_with_fixtures(_fn)
        except BaseException as _e:
            _fail += 1
            import traceback as _tb
            _sys.stderr.write('FAILED ' + _label + ': ' + repr(_e) + '\n')
            _tb.print_exc()
    for _n in sorted([k for k in list(_g.keys()) if k.startswith('test_')]):
        _fn = _g[_n]
        if callable(_fn):
            _run_one(_fn, _n)
    for _cn in sorted([k for k in list(_g.keys()) if k.startswith('Test')]):
        _cls = _g[_cn]
        if isinstance(_cls, type):
            _inst = _cls()
            for _m in sorted([x for x in dir(_cls) if x.startswith('test_')]):
                _run_one(getattr(_inst, _m), _cn + '.' + _m)
    if _ran == 0:
        _fail = 1
        _sys.stderr.write('no tests ran\n')
    __EXIT = 1 if _fail else 0
except SystemExit as _se:
    __EXIT = _se.code if isinstance(_se.code, int) else (0 if _se.code is None else 1)
except BaseException:
    import traceback as _tb
    _tb.print_exc()
    __EXIT = 1
"#;

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
const PRELUDE: &str = r#"
import sys, io, os, types, builtins, posixpath
_VFS_FILES = __VFS_FILES

_VFS_CWD = __VFS_CWD_INIT
_VFS_DIRS = set(__VFS_DIRS_INIT)
_VFS_DIRS.add('/')

sys.argv = list(__ARGV)
sys.path = list(__PYPATH) + list(sys.path)

def _vfs_norm(p):
    if hasattr(p, '__fspath__'):
        p = p.__fspath__()
    p = str(p)
    if not p.startswith('/'):
        p = _VFS_CWD.rstrip('/') + '/' + p
    parts = []
    for c in p.split('/'):
        if c == '' or c == '.':
            continue
        if c == '..':
            if parts:
                parts.pop()
        else:
            parts.append(c)
    return '/' + '/'.join(parts)

def _vfs_isdir(p):
    p = _vfs_norm(p)
    return p in _VFS_DIRS

def _vfs_isfile(p):
    return _vfs_norm(p) in _VFS_FILES

def _vfs_exists(p):
    p = _vfs_norm(p)
    return p in _VFS_FILES or p in _VFS_DIRS

def _vfs_mkdirs(p, exist_ok=True):
    p = _vfs_norm(p)
    acc = ''
    for c in p.split('/'):
        if c == '':
            continue
        acc = acc + '/' + c
        _VFS_DIRS.add(acc)

def _vfs_listdir(p='.'):
    p = _vfs_norm(p)
    pref = p.rstrip('/') + '/'
    out = set()
    for k in list(_VFS_FILES.keys()) + list(_VFS_DIRS):
        if k.startswith(pref):
            rest = k[len(pref):]
            if rest and '/' not in rest:
                out.add(rest)
    return sorted(out)

class _VFile:
    def __init__(self, path, mode='r', encoding='utf-8'):
        self.path = path
        self.mode = mode
        self.encoding = encoding or 'utf-8'
        self.binary = 'b' in mode
        self.closed = False
        self._pos = 0
        if 'r' in mode or '+' in mode and 'w' not in mode:
            data = _VFS_FILES.get(path)
            if data is None:
                if 'r' in mode and 'w' not in mode and 'a' not in mode:
                    raise FileNotFoundError(2, 'No such file or directory: ' + path)
                data = b''
            self._buf = bytearray(data)
        else:
            self._buf = bytearray()
        if 'a' in mode:
            data = _VFS_FILES.get(path)
            if data:
                self._buf = bytearray(data)
            self._pos = len(self._buf)
        if 'w' in mode:
            self._buf = bytearray()
            _VFS_FILES[path] = b''
    def _enc(self, s):
        if self.binary:
            return bytes(s)
        return s.encode(self.encoding)
    def _dec(self, b):
        if self.binary:
            return bytes(b)
        return bytes(b).decode(self.encoding)
    def read(self, n=-1):
        if n is None or n < 0:
            data = self._buf[self._pos:]
            self._pos = len(self._buf)
        else:
            data = self._buf[self._pos:self._pos+n]
            self._pos += len(data)
        return self._dec(data)
    def readline(self):
        nl = self._buf.find(b'\n', self._pos)
        if nl == -1:
            return self.read()
        data = self._buf[self._pos:nl+1]
        self._pos = nl + 1
        return self._dec(data)
    def readlines(self):
        out = []
        while True:
            line = self.readline()
            if not line:
                break
            out.append(line)
        return out
    def __iter__(self):
        return self
    def write(self, data):
        b = self._enc(data)
        self._buf[self._pos:self._pos+len(b)] = b
        self._pos += len(b)
        _VFS_FILES[self.path] = bytes(self._buf)
        _vfs_mkdirs(posixpath.dirname(self.path))
        return len(data)
    def writelines(self, lines):
        for l in lines:
            self.write(l)
    def seek(self, pos, whence=0):
        if whence == 0:
            self._pos = pos
        elif whence == 1:
            self._pos += pos
        else:
            self._pos = len(self._buf) + pos
        return self._pos
    def tell(self):
        return self._pos
    def seekable(self):
        return True
    def readable(self):
        return ('r' in self.mode) or ('+' in self.mode)
    def writable(self):
        return ('w' in self.mode) or ('a' in self.mode) or ('+' in self.mode)
    def fileno(self):
        return -1
    def isatty(self):
        return False
    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line
    def flush(self):
        _VFS_FILES[self.path] = bytes(self._buf)
    def close(self):
        if not self.closed:
            if 'r' not in self.mode or '+' in self.mode or 'w' in self.mode or 'a' in self.mode:
                _VFS_FILES[self.path] = bytes(self._buf)
            self.closed = True
    def __enter__(self):
        return self
    def __exit__(self, *a):
        self.close()
        return False

def _vfs_open(path, mode='r', buffering=-1, encoding=None, errors=None, newline=None, closefd=True, opener=None):
    p = _vfs_norm(path)
    if ('w' in mode or 'a' in mode or '+' in mode):
        _vfs_mkdirs(posixpath.dirname(p))
    return _VFile(p, mode, encoding)

builtins.open = _vfs_open
io.open = _vfs_open

# ---- patch os ----
os.getcwd = lambda: _VFS_CWD
def _os_chdir(p):
    global _VFS_CWD
    _VFS_CWD = _vfs_norm(p)
os.chdir = _os_chdir
os.listdir = _vfs_listdir
os.makedirs = lambda p, mode=0o777, exist_ok=False: _vfs_mkdirs(p)
os.mkdir = lambda p, mode=0o777: _vfs_mkdirs(p)
def _os_remove(p):
    p = _vfs_norm(p)
    if p in _VFS_FILES:
        del _VFS_FILES[p]
    else:
        raise FileNotFoundError(2, 'No such file: ' + p)
os.remove = _os_remove
os.unlink = _os_remove
def _os_rmdir(p):
    p = _vfs_norm(p)
    _VFS_DIRS.discard(p)
os.rmdir = _os_rmdir
def _os_rename(a, b):
    a = _vfs_norm(a); b = _vfs_norm(b)
    if a in _VFS_FILES:
        _VFS_FILES[b] = _VFS_FILES.pop(a)
    elif a in _VFS_DIRS:
        # move subtree
        for k in list(_VFS_FILES.keys()):
            if k == a or k.startswith(a + '/'):
                _VFS_FILES[b + k[len(a):]] = _VFS_FILES.pop(k)
        for k in list(_VFS_DIRS):
            if k == a or k.startswith(a + '/'):
                _VFS_DIRS.discard(k); _VFS_DIRS.add(b + k[len(a):])
os.rename = _os_rename
os.replace = _os_rename
def _os_walk(top):
    top = _vfs_norm(top)
    dirs = []
    files = []
    pref = top.rstrip('/') + '/'
    seen = set()
    for k in list(_VFS_FILES.keys()):
        if k.startswith(pref):
            rest = k[len(pref):]
            if '/' not in rest:
                files.append(rest)
    for k in list(_VFS_DIRS):
        if k.startswith(pref):
            rest = k[len(pref):]
            if rest and '/' not in rest:
                dirs.append(rest)
    yield (top, sorted(set(dirs)), sorted(files))
    for d in sorted(set(dirs)):
        for x in _os_walk(pref + d):
            yield x
os.walk = _os_walk
os.environ = dict(__VFS_ENV)
os.getenv = lambda k, d=None: __VFS_ENV.get(k, d)
def _os_makedirs_exist(p, exist_ok=False):
    _vfs_mkdirs(p)
os.makedirs = _os_makedirs_exist

# os.path
os.path.exists = _vfs_exists
os.path.isfile = _vfs_isfile
os.path.isdir = _vfs_isdir
os.path.lexists = _vfs_exists
os.path.getsize = lambda p: len(_VFS_FILES.get(_vfs_norm(p), b''))
_real_abspath = os.path.abspath
os.path.abspath = lambda p='.': _vfs_norm(p)

class _stat_result:
    def __init__(self, size):
        self.st_size = size
        self.st_mode = 0o100644
        self.st_mtime = 0
        self.st_ctime = 0
def _os_stat(p):
    p = _vfs_norm(p)
    if p in _VFS_FILES:
        return _stat_result(len(_VFS_FILES[p]))
    if p in _VFS_DIRS:
        r = _stat_result(0); r.st_mode = 0o040755; return r
    raise FileNotFoundError(2, 'No such file: ' + p)
os.stat = _os_stat

# ---- shutil basics ----
try:
    import shutil
    def _sh_copy(a, b):
        a = _vfs_norm(a); b = _vfs_norm(b)
        if b in _VFS_DIRS:
            b = b.rstrip('/') + '/' + posixpath.basename(a)
        _VFS_FILES[b] = _VFS_FILES.get(a, b'')
        return b
    shutil.copy = _sh_copy
    shutil.copyfile = _sh_copy
    shutil.copy2 = _sh_copy
    def _sh_rmtree(p, ignore_errors=False):
        p = _vfs_norm(p)
        for k in list(_VFS_FILES.keys()):
            if k == p or k.startswith(p + '/'):
                del _VFS_FILES[k]
        for k in list(_VFS_DIRS):
            if k == p or k.startswith(p + '/'):
                _VFS_DIRS.discard(k)
    shutil.rmtree = _sh_rmtree
    def _sh_move(a, b):
        _os_rename(a, b)
    shutil.move = _sh_move
except Exception:
    pass

# ---- glob ----
try:
    import glob as _glob
    import fnmatch as _fn
    def _vfs_glob(pat, recursive=False):
        pat = _vfs_norm(pat)
        cands = list(_VFS_FILES.keys()) + [d for d in _VFS_DIRS]
        if '**' in pat:
            rx = pat.replace('**', '*')
            return sorted([c for c in cands if _fn.fnmatch(c, rx) or _fn.fnmatch(c, pat)])
        return sorted([c for c in cands if _fn.fnmatch(c, pat)])
    _glob.glob = _vfs_glob
    _glob.iglob = _vfs_glob
except Exception:
    pass

# ---- pathlib (replace Path with a VFS-backed class) ----
import pathlib as _pathlib
class _VPath:
    def __init__(self, *parts):
        if len(parts) == 1 and isinstance(parts[0], _VPath):
            self._p = parts[0]._p
        else:
            joined = '/'.join(str(getattr(x, '_p', x)) for x in parts if str(getattr(x, '_p', x)) != '')
            self._p = joined if joined else '.'
    def __fspath__(self):
        return _vfs_norm(self._p)
    def __str__(self):
        return _vfs_norm(self._p)
    def __repr__(self):
        return "Path('%s')" % str(self)
    def __truediv__(self, other):
        return _VPath(str(self) + '/' + str(getattr(other, '_p', other)))
    def __eq__(self, other):
        return str(self) == str(other)
    def __hash__(self):
        return hash(str(self))
    @property
    def name(self):
        return posixpath.basename(str(self))
    @property
    def stem(self):
        n = self.name
        return n[:n.rfind('.')] if '.' in n else n
    @property
    def suffix(self):
        n = self.name
        return n[n.rfind('.'):] if '.' in n else ''
    @property
    def parent(self):
        return _VPath(posixpath.dirname(str(self)))
    @property
    def parts(self):
        return tuple(str(self).split('/'))
    def exists(self):
        return _vfs_exists(str(self))
    def is_file(self):
        return _vfs_isfile(str(self))
    def is_dir(self):
        return _vfs_isdir(str(self))
    def mkdir(self, mode=0o777, parents=False, exist_ok=False):
        _vfs_mkdirs(str(self))
    def read_text(self, encoding='utf-8', errors=None):
        with _vfs_open(str(self), 'r', encoding=encoding) as f:
            return f.read()
    def read_bytes(self):
        with _vfs_open(str(self), 'rb') as f:
            return f.read()
    def write_text(self, data, encoding='utf-8', errors=None):
        with _vfs_open(str(self), 'w', encoding=encoding) as f:
            return f.write(data)
    def write_bytes(self, data):
        with _vfs_open(str(self), 'wb') as f:
            return f.write(data)
    def joinpath(self, *others):
        r = self
        for o in others:
            r = r / o
        return r
    def glob(self, pattern):
        base = str(self).rstrip('/')
        import fnmatch as _fn
        pat = base + '/' + pattern
        for k in sorted(list(_VFS_FILES.keys()) + list(_VFS_DIRS)):
            if _fn.fnmatch(k, pat):
                yield _VPath(k)
    def iterdir(self):
        for n in _vfs_listdir(str(self)):
            yield _VPath(str(self) + '/' + n)
    def resolve(self):
        return _VPath(_vfs_norm(str(self)))
    def absolute(self):
        return self.resolve()
    def unlink(self, missing_ok=False):
        p = _vfs_norm(str(self))
        if p in _VFS_FILES:
            del _VFS_FILES[p]
        elif not missing_ok:
            raise FileNotFoundError(2, p)
    def with_suffix(self, suffix):
        return _VPath(str(self.parent) + '/' + self.stem + suffix)
    def with_name(self, name):
        return self.parent / name
    def relative_to(self, *other):
        base = _vfs_norm('/'.join(str(getattr(o, '_p', o)) for o in other))
        s = _vfs_norm(str(self))
        if base in ('.', ''):
            return _VPath(s.lstrip('/') or '.')
        if s == base:
            return _VPath('.')
        pref = base.rstrip('/') + '/'
        if s.startswith(pref):
            return _VPath(s[len(pref):])
        raise ValueError("%r is not in the subpath of %r" % (str(self), base))
    def is_relative_to(self, *other):
        try:
            self.relative_to(*other); return True
        except ValueError:
            return False
    def rglob(self, pattern):
        import fnmatch as _fn
        base = str(self).rstrip('/')
        for k in sorted(list(_VFS_FILES.keys()) + list(_VFS_DIRS)):
            if base == '' or k == base or k.startswith(base + '/'):
                if _fn.fnmatch(posixpath.basename(k), pattern) or _fn.fnmatch(k, base + '/' + pattern):
                    yield _VPath(k)
    def match(self, pattern):
        import fnmatch as _fn
        return _fn.fnmatch(str(self), pattern) or _fn.fnmatch(self.name, pattern)
    def as_posix(self):
        return str(self)
    def is_absolute(self):
        return str(self).startswith('/')
    def is_symlink(self):
        return False
    def samefile(self, other):
        return _vfs_norm(str(self)) == _vfs_norm(str(getattr(other, '_p', other)))
    def rename(self, target):
        t = _vfs_norm(str(getattr(target, '_p', target)))
        s = _vfs_norm(str(self))
        if s in _VFS_FILES:
            _VFS_FILES[t] = _VFS_FILES.pop(s)
        return _VPath(t)
    def replace(self, target):
        return self.rename(target)
    def stat(self):
        return os.stat(str(self))
    def chmod(self, mode):
        pass
    def touch(self, mode=0o666, exist_ok=True):
        p = _vfs_norm(str(self))
        if p not in _VFS_FILES:
            _VFS_FILES[p] = b''
    @classmethod
    def cwd(cls):
        return _VPath(os.getcwd())
    @classmethod
    def home(cls):
        return _VPath(os.environ.get('HOME', '/root'))
_pathlib.Path = _VPath
_pathlib.PosixPath = _VPath
_pathlib.PurePath = _VPath
_pathlib.PurePosixPath = _VPath

# ---- pydantic stub (graders subclass BaseModel) ----
_pydantic = types.ModuleType('pydantic')
class _BaseModel:
    def __init__(self, **kwargs):
        cls = type(self)
        ann = {}
        for base in reversed(cls.__mro__):
            ann.update(getattr(base, '__annotations__', {}) or {})
        # class-level defaults
        for k in ann:
            if hasattr(cls, k):
                setattr(self, k, getattr(cls, k))
            else:
                setattr(self, k, None)
        for k, v in kwargs.items():
            setattr(self, k, v)
    def dict(self, *a, **k):
        return {kk: getattr(self, kk) for kk in self._fields()}
    def model_dump(self, *a, **k):
        return self.dict()
    def json(self, *a, **k):
        import json as _j
        return _j.dumps(self.dict())
    def model_dump_json(self, *a, **k):
        return self.json()
    def _fields(self):
        ann = {}
        for base in reversed(type(self).__mro__):
            ann.update(getattr(base, '__annotations__', {}) or {})
        return list(ann.keys())
    def __eq__(self, other):
        return isinstance(other, _BaseModel) and self.dict() == other.dict()
    def __repr__(self):
        return type(self).__name__ + '(' + ', '.join('%s=%r' % (k, getattr(self, k, None)) for k in self._fields()) + ')'
_pydantic.BaseModel = _BaseModel
def _pyd_field(default=None, **kwargs):
    if default is None and 'default_factory' in kwargs:
        try:
            return kwargs['default_factory']()
        except Exception:
            return None
    return default
_pydantic.Field = _pyd_field
def _pyd_validator(*a, **k):
    def deco(f):
        return f
    return deco
_pydantic.validator = _pyd_validator
_pydantic.field_validator = _pyd_validator
_pydantic.root_validator = _pyd_validator
_pydantic.model_validator = _pyd_validator
class _ValidationError(Exception):
    pass
_pydantic.ValidationError = _ValidationError
_pydantic.BaseSettings = _BaseModel
sys.modules['pydantic'] = _pydantic

# ---- pytest stub ----
_pytest = types.ModuleType('pytest')
class _Raises:
    def __init__(self, exc, match=None):
        self.exc = exc
        self.match = match
        self.value = None
    def __enter__(self):
        return self
    def __exit__(self, et, ev, tb):
        if et is None:
            raise AssertionError('DID NOT RAISE ' + repr(self.exc))
        self.value = ev
        ok = issubclass(et, self.exc) if isinstance(self.exc, type) else et in tuple(self.exc)
        return ok
def _pt_raises(exc, *a, **k):
    return _Raises(exc, k.get('match'))
_pytest.raises = _pt_raises
class _Approx:
    def __init__(self, v, rel=1e-6, abs=1e-12):
        self.v = v; self.rel = rel; self.abs = abs
    def __eq__(self, other):
        try:
            return abs(float(other) - float(self.v)) <= max(self.abs, self.rel * abs(float(self.v)))
        except Exception:
            return False
    def __repr__(self):
        return 'approx(%r)' % self.v
_pytest.approx = lambda v, rel=1e-6, abs=1e-12: _Approx(v, rel, abs)
def _pt_skip(reason=''):
    raise _PtSkip(reason)
class _PtSkip(Exception):
    pass
_pytest.skip = _pt_skip
_pytest.xfail = lambda *a, **k: None
def _pt_fail(msg='', pytrace=True):
    raise AssertionError(msg)
_pytest.fail = _pt_fail
class _Mark:
    def __getattr__(self, name):
        def deco(*a, **k):
            if len(a) == 1 and callable(a[0]) and not k:
                return a[0]
            def wrap(f):
                return f
            return wrap
        return deco
_pytest.mark = _Mark()
def _pt_fixture(*a, **k):
    def _reg(f):
        try:
            f._pytest_fixture = True
        except Exception:
            pass
        return f
    if len(a) == 1 and callable(a[0]) and not k:
        return _reg(a[0])
    def deco(f):
        return _reg(f)
    return deco
_pytest.fixture = _pt_fixture
_pytest.main = lambda *a, **k: 0
def _pt_param(*values, **k):
    return values
_pytest.param = _pt_param
sys.modules['pytest'] = _pytest

# ---- subprocess stub: route `python ...` invocations back through THIS vm, in-process,
# operating on the same VFS. No real processes are spawned; timeouts are ignored (no blocking).
sys.executable = 'python3'
_subprocess = types.ModuleType('subprocess')
_subprocess.PIPE = -1
_subprocess.STDOUT = -2
_subprocess.DEVNULL = -3
class _CalledProcessError(Exception):
    def __init__(self, returncode, cmd, output=None, stderr=None):
        self.returncode = returncode; self.cmd = cmd
        self.output = output; self.stdout = output; self.stderr = stderr
        super().__init__("Command %r returned non-zero exit status %d." % (cmd, returncode))
class _TimeoutExpired(Exception):
    def __init__(self, cmd, timeout, output=None, stderr=None):
        self.cmd = cmd; self.timeout = timeout; self.output = output; self.stderr = stderr
class _CompletedProcess:
    def __init__(self, args, returncode, stdout=None, stderr=None):
        self.args = args; self.returncode = returncode
        self.stdout = stdout; self.stderr = stderr
    def __repr__(self):
        return "CompletedProcess(args=%r, returncode=%r)" % (self.args, self.returncode)
    def check_returncode(self):
        if self.returncode:
            raise _CalledProcessError(self.returncode, self.args, self.stdout, self.stderr)
_subprocess.CalledProcessError = _CalledProcessError
_subprocess.SubprocessError = _CalledProcessError
_subprocess.TimeoutExpired = _TimeoutExpired
_subprocess.CompletedProcess = _CompletedProcess

def _sp_split(args, shell):
    if isinstance(args, (list, tuple)):
        return [str(getattr(a, '_p', a)) for a in args]
    if shell:
        try:
            import shlex as _shlex
            return _shlex.split(str(args))
        except Exception:
            return str(args).split()
    return [str(args)]

def _sp_is_python(exe):
    b = posixpath.basename(str(exe))
    return b == 'python' or b == 'py' or b.startswith('python3') or b.startswith('python2')

def _sp_find_script(name):
    cands = [name]
    if not name.startswith('/'):
        cands.append(os.getcwd().rstrip('/') + '/' + name)
        for d in __PYPATH:
            cands.append(d.rstrip('/') + '/' + name)
    for c in cands:
        cc = _vfs_norm(c)
        if cc in _VFS_FILES:
            return cc
    return None

def _sp_run_python(parts, input_data, cwd):
    rest = parts[1:]
    code = None; module = None; script = None; sargs = []
    i = 0
    while i < len(rest):
        a = rest[i]
        if a == '-c':
            code = rest[i+1] if i+1 < len(rest) else ''; sargs = rest[i+2:]; break
        elif a == '-m':
            module = rest[i+1] if i+1 < len(rest) else ''; sargs = rest[i+2:]; break
        elif a == '-':
            script = '-'; sargs = rest[i+1:]; break
        elif a.startswith('-') and len(a) > 1:
            i += 1; continue
        else:
            script = a; sargs = rest[i+1:]; break
        i += 1
    old_argv = sys.argv; old_out = sys.stdout; old_err = sys.stderr; old_cwd = os.getcwd()
    out = io.StringIO(); err = io.StringIO(); rc = 0
    try:
        if cwd: os.chdir(cwd)
        sys.stdout = out; sys.stderr = err
        g = {'__name__': '__main__'}
        if code is not None:
            sys.argv = ['-c'] + sargs
            exec(compile(code, '<string>', 'exec'), g)
        elif module is not None:
            sys.argv = [module] + sargs
            __import__(module)
        elif script == '-':
            sys.argv = ['-'] + sargs
            src = input_data if isinstance(input_data, str) else ((input_data or b'').decode('utf-8', 'replace'))
            exec(compile(src, '<stdin>', 'exec'), g)
        elif script is not None:
            sp = _sp_find_script(script)
            if sp is None:
                err.write("python: can't open file %r: No such file or directory\n" % script); rc = 2
            else:
                g['__file__'] = sp
                sys.argv = [script] + sargs
                data = _VFS_FILES[sp]
                src = data.decode('utf-8') if isinstance(data, (bytes, bytearray)) else data
                exec(compile(src, sp, 'exec'), g)
    except SystemExit as e:
        rc = e.code if isinstance(e.code, int) else (0 if e.code is None else 1)
    except BaseException:
        import traceback as _tb
        _tb.print_exc(file=err); rc = 1
    finally:
        sys.stdout = old_out; sys.stderr = old_err; sys.argv = old_argv
        try: os.chdir(old_cwd)
        except Exception: pass
    return (rc, out.getvalue(), err.getvalue())

def _sp_exec(args, shell=False, input_data=None, cwd=None):
    parts = _sp_split(args, shell)
    if not parts:
        return (0, '', '')
    exe = parts[0]
    if _sp_is_python(exe):
        return _sp_run_python(parts, input_data, cwd)
    if exe.endswith('.py'):
        return _sp_run_python(['python'] + parts, input_data, cwd)
    # non-python external command: not simulated (record nothing, benign failure)
    return (127, '', posixpath.basename(exe) + ': command not found\n')

def _sp_norm(s, text, encoding):
    if s is None: return None
    if text or encoding: return s
    return s.encode(encoding or 'utf-8')

def _sp_run(args, **kw):
    capture = kw.get('capture_output', False)
    stdout = kw.get('stdout', None); stderr = kw.get('stderr', None)
    text = kw.get('text', False) or kw.get('universal_newlines', False)
    encoding = kw.get('encoding', None)
    inp = kw.get('input', None)
    cwd = kw.get('cwd', None)
    shell = kw.get('shell', False)
    check = kw.get('check', False)
    if cwd is not None: cwd = str(getattr(cwd, '_p', cwd))
    rc, o, e = _sp_exec(args, shell=shell, input_data=inp, cwd=cwd)
    want_out = capture or stdout == _subprocess.PIPE
    want_err = capture or stderr == _subprocess.PIPE
    if stderr == _subprocess.STDOUT:
        o = o + e; e = ''; want_err = False
    cp = _CompletedProcess(args, rc,
                           _sp_norm(o if want_out else None, text, encoding),
                           _sp_norm(e if want_err else None, text, encoding))
    if check and rc != 0:
        raise _CalledProcessError(rc, args, cp.stdout, cp.stderr)
    return cp

def _sp_call(args, **kw):
    kw.pop('capture_output', None)
    return _sp_run(args, **kw).returncode

def _sp_check_call(args, **kw):
    rc = _sp_call(args, **kw)
    if rc != 0: raise _CalledProcessError(rc, args)
    return 0

def _sp_check_output(args, **kw):
    kw.setdefault('capture_output', True); kw['check'] = True
    return _sp_run(args, **kw).stdout

def _sp_getstatusoutput(cmd):
    rc, o, e = _sp_exec(cmd, shell=True)
    return (rc, (o + e).rstrip('\n'))

class _Popen:
    def __init__(self, args, **kw):
        self._args = args; self._kw = kw; self.returncode = None
        self.stdin = io.StringIO(); self.stdout = None; self.stderr = None
    def communicate(self, input=None, timeout=None):
        kw = dict(self._kw)
        if input is not None: kw['input'] = input
        kw.setdefault('capture_output', True)
        cp = _sp_run(self._args, **kw)
        self.returncode = cp.returncode
        return (cp.stdout, cp.stderr)
    def wait(self, timeout=None):
        if self.returncode is None: self.communicate()
        return self.returncode
    def poll(self): return self.returncode
    def kill(self): self.returncode = -9
    def terminate(self): self.returncode = -15
    def __enter__(self): return self
    def __exit__(self, *a): return False

_subprocess.run = _sp_run
_subprocess.call = _sp_call
_subprocess.check_call = _sp_check_call
_subprocess.check_output = _sp_check_output
_subprocess.getoutput = lambda cmd: _sp_exec(cmd, shell=True)[1]
_subprocess.getstatusoutput = _sp_getstatusoutput
_subprocess.Popen = _Popen
sys.modules['subprocess'] = _subprocess

# ---- VFS-backed importer for sibling modules ----
def _preload_modules():
    loaded = {}
    for d in __PYPATH:
        d = d.rstrip('/')
        for path in list(_VFS_FILES.keys()):
            if not path.endswith('.py'):
                continue
            if not (path.startswith(d + '/') ):
                continue
            rel = path[len(d) + 1:]
            if '/' in rel:
                continue
            name = rel[:-3]
            if name == '__init__' or name in sys.modules or name in loaded:
                continue
            loaded[name] = path
    for name, path in loaded.items():
        mod = types.ModuleType(name)
        mod.__file__ = path
        sys.modules[name] = mod
    for name, path in loaded.items():
        try:
            data = _VFS_FILES[path]
            srctext = data.decode('utf-8') if isinstance(data, (bytes, bytearray)) else data
            exec(compile(srctext, path, 'exec'), sys.modules[name].__dict__)
        except Exception:
            pass
_preload_modules()

# ---- redirect stdout/stderr for capture ----
sys.stdout = io.StringIO()
sys.stderr = io.StringIO()
__EXIT = 0
"#;

#[cfg(feature = "python")]
const POSTLUDE: &str = r#"
try:
    __STDOUT = sys.stdout.getvalue()
    __STDERR = sys.stderr.getvalue()
except Exception:
    __STDOUT = ''
    __STDERR = ''
sys.stdout = sys.__stdout__
sys.stderr = sys.__stderr__
__EXIT_S = str(__EXIT)
__VFS_DIRS_S = '\n'.join(sorted(_VFS_DIRS))
import base64 as _b64, json as _json
_dump = {}
for _p, _d in _VFS_FILES.items():
    try:
        _dump[_p] = _b64.b64encode(bytes(_d)).decode('ascii')
    except Exception:
        pass
__VFS_DUMP_JSON = _json.dumps(_dump)
"#;
