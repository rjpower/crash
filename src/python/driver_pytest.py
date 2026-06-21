
__EXIT = 0
try:
    _g = globals()
    exec(compile(__USER_SRC, __USER_FILE, 'exec'), _g)
    import sys as _sys
    import types as _types
    _fail = 0
    _ran = 0
    _tmpc = [0]
    # Non-function-scoped fixtures (module/session/package/class) are instantiated once and
    # cached; their teardown runs only after every test, mirroring pytest. Single test file ==
    # single module, so we treat all non-function scopes as run-wide.
    _scope_cache = {}
    _session_fins = []
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
            _scope = getattr(_fn, '_pytest_scope', 'function')
            if _scope != 'function' and _name in _scope_cache:
                return _scope_cache[_name]
            _kw = {}
            for _an in _params(_fn):
                if _an in ('self', 'request'):
                    if _an == 'request':
                        _kw[_an] = _builtin_fixture('request')
                    continue
                _kw[_an] = _resolve(_an, _fins, _seen | {_name})
            _v = _fn(**_kw)
            if isinstance(_v, _types.GeneratorType):
                _gen = _v
                _v = next(_gen)
                (_session_fins if _scope != 'function' else _fins).append(_gen)
            if _scope != 'function':
                _scope_cache[_name] = _v
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
    def _lineno(_fn):
        try:
            return _fn.__code__.co_firstlineno
        except Exception:
            return 0
    # pytest collects in source-definition order, not alphabetical — some suites rely on an
    # earlier test producing state (output files, etc.) a later one reads.
    _tests = [k for k in list(_g.keys()) if k.startswith('test_') and callable(_g[k])]
    _tests.sort(key=lambda _k: _lineno(_g[_k]))
    for _n in _tests:
        _run_one(_g[_n], _n)
    _classes = [k for k in list(_g.keys()) if k.startswith('Test') and isinstance(_g[k], type)]
    _classes.sort(key=lambda _k: _lineno(_g[_k]))
    for _cn in _classes:
        _cls = _g[_cn]
        _inst = _cls()
        _methods = [x for x in dir(_cls) if x.startswith('test_')]
        _methods.sort(key=lambda _x: _lineno(getattr(_cls, _x, None)))
        for _m in _methods:
            _run_one(getattr(_inst, _m), _cn + '.' + _m)
    # tear down module/session-scoped fixtures once, after every test (reverse order).
    for _gen in reversed(_session_fins):
        try:
            next(_gen)
        except StopIteration:
            pass
        except Exception:
            pass
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
