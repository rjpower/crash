
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
