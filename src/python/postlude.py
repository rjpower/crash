
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
