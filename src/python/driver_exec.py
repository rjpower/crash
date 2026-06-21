
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
