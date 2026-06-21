"""shellsim's deliberately-simple, pure-Python ``numpy``.

Not fast and not complete — a flat Python list + a shape tuple, row-major (C order). It covers
the basic array-creation / indexing / broadcasting / reduction / linalg surface that the target
tasks use (see eval/extract_api.py output). Anything outside that surface calls
``_shellsim_ood(...)`` (which the harness turns into a `low` trust verdict) instead of silently
returning a wrong answer.

It registers its own submodules (`numpy.random`, `numpy.linalg`, `numpy.testing`) in
``sys.modules`` so ``import numpy.linalg`` / ``from numpy.testing import ...`` resolve.
"""
import sys as _sys
import types as _types
import math as _math
import builtins as _bi

# Our module exposes np.sum / np.all / np.min / np.abs etc., which *shadow* the Python builtins
# at module scope. Internal code must therefore reach the real builtins through these aliases,
# never via the bare names, or it would recurse back into the numpy free functions.
_bsum = _bi.sum
_ball = _bi.all
_bany = _bi.any
_bmin = _bi.min
_bmax = _bi.max
_babs = _bi.abs

def _ood(msg):
    try:
        _shellsim_ood("numpy: " + msg)
    except Exception:
        pass


# --------------------------------------------------------------------------- dtypes
# Scalar types subclass the Python builtins so isinstance() and arithmetic just work; the
# dtype *object* on an array compares equal to whichever of these the user names.
class float64(float):
    __name__ = "float64"
class float32(float):
    __name__ = "float32"
class int64(int):
    __name__ = "int64"
class int32(int):
    __name__ = "int32"
class bool_(int):
    __name__ = "bool_"

class _NumberMeta(type):
    def __instancecheck__(cls, obj):
        return isinstance(obj, (int, float)) and not isinstance(obj, bool)
class number(metaclass=_NumberMeta):
    pass
class integer(metaclass=_NumberMeta):
    pass
class floating(metaclass=_NumberMeta):
    pass

_DTYPE_OF = {
    float64: "float64", float32: "float32", int64: "int64", int32: "int32",
    bool_: "bool", float: "float64", int: "int64", bool: "bool",
    "float64": "float64", "float32": "float32", "int64": "int64", "int32": "int32",
    "int": "int64", "float": "float64", "bool": "bool", "b": "bool", "f8": "float64",
    "i8": "int64", "i4": "int32", "f4": "float32",
}
_CASTER = {
    "float64": float, "float32": float, "int64": int, "int32": int, "bool": bool,
}

def _norm_dtype(dt):
    if dt is None:
        return None
    if isinstance(dt, dtype):
        return dt.name
    name = _DTYPE_OF.get(dt)
    if name is None and isinstance(dt, str):
        name = _DTYPE_OF.get(dt)
    if name is None:
        _ood("unknown dtype %r" % (dt,))
        return "float64"
    return name

class dtype:
    def __init__(self, name):
        self.name = _norm_dtype(name) or "float64"
    @property
    def kind(self):
        return {"float64": "f", "float32": "f", "int64": "i", "int32": "i", "bool": "b"}.get(self.name, "f")
    @property
    def type(self):
        return {"float64": float64, "float32": float32, "int64": int64, "int32": int32, "bool": bool_}[self.name]
    def __eq__(self, other):
        if isinstance(other, dtype):
            return self.name == other.name
        return self.name == _norm_dtype(other)
    def __hash__(self):
        return hash(self.name)
    def __repr__(self):
        return "dtype('%s')" % self.name
    def __str__(self):
        return self.name


# --------------------------------------------------------------------------- helpers
def _strides_for(shape):
    st = [1] * len(shape)
    for i in range(len(shape) - 2, -1, -1):
        st[i] = st[i + 1] * shape[i + 1]
    return st

def _size_of(shape):
    n = 1
    for s in shape:
        n *= s
    return n

def _is_arraylike(obj):
    """Duck-type a foreign 1-D sequence (e.g. a pandas Series/Index) we should treat as data,
    not as an opaque scalar. Excludes strings/bytes and our own scalar dtype subclasses."""
    return (not isinstance(obj, (ndarray, list, tuple, str, bytes))
            and not isinstance(obj, (int, float, bool))
            and hasattr(obj, "tolist") and callable(getattr(obj, "tolist")))

def _flat_from_nested(obj):
    """Return (flat_list, shape) from a (possibly nested) list/tuple/scalar/ndarray/Series."""
    if isinstance(obj, ndarray):
        return list(obj._data), tuple(obj._shape)
    if _is_arraylike(obj):
        try:
            return _flat_from_nested(list(obj.tolist()))
        except Exception:
            pass
    if isinstance(obj, (list, tuple)):
        if len(obj) == 0:
            return [], (0,)
        subs = [_flat_from_nested(x) for x in obj]
        sub_shape = subs[0][1]
        for _f, s in subs:
            if s != sub_shape:
                _ood("ragged/inhomogeneous array construction")
                break
        flat = []
        for f, _s in subs:
            flat.extend(f)
        return flat, (len(obj),) + sub_shape
    return [obj], ()

def _cast_list(data, name):
    c = _CASTER.get(name)
    if c is None:
        return data
    out = []
    for v in data:
        if v is None:
            out.append(v)
        elif isinstance(v, bool) and c is not bool:
            out.append(c(int(v)))
        else:
            try:
                out.append(c(v))
            except (TypeError, ValueError):
                out.append(v)
    return out

def _infer_dtype(flat):
    for v in flat:
        if isinstance(v, bool):
            continue
        if isinstance(v, float):
            return "float64"
    for v in flat:
        if isinstance(v, float) and not isinstance(v, bool):
            return "float64"
    allint = _ball(isinstance(v, (int,)) and not isinstance(v, bool) for v in flat) if flat else True
    if allint:
        return "int64"
    if flat and _ball(isinstance(v, bool) for v in flat):
        return "bool"
    return "float64"


# --------------------------------------------------------------------------- ndarray
class ndarray:
    def __init__(self, data, shape, dt="float64"):
        self._data = data
        self._shape = tuple(shape)
        self._dtype = dt

    # ---- basic attributes ----
    @property
    def shape(self):
        return self._shape
    @property
    def ndim(self):
        return len(self._shape)
    @property
    def size(self):
        return _size_of(self._shape)
    @property
    def dtype(self):
        return dtype(self._dtype)
    @property
    def T(self):
        return self.transpose()
    @property
    def flat(self):
        return iter(self._data)
    def __len__(self):
        if not self._shape:
            raise TypeError("len() of unsized object")
        return self._shape[0]

    # ---- conversion ----
    def astype(self, dt, copy=True):
        name = _norm_dtype(dt) or self._dtype
        return ndarray(_cast_list(self._data, name), self._shape, name)
    def copy(self):
        return ndarray(list(self._data), self._shape, self._dtype)
    def tolist(self):
        return _to_nested(self._data, self._shape)
    def item(self, *idx):
        if not idx:
            if len(self._data) != 1:
                raise ValueError("can only convert an array of size 1 to a Python scalar")
            return self._data[0]
        return self[idx] if len(idx) > 1 else self[idx[0]]
    def fill(self, value):
        self._data = [value] * len(self._data)
    def ravel(self):
        return ndarray(list(self._data), (len(self._data),), self._dtype)
    def flatten(self):
        return self.ravel()
    def reshape(self, *shape):
        if len(shape) == 1 and isinstance(shape[0], (tuple, list)):
            shape = tuple(shape[0])
        shape = list(shape)
        n = len(self._data)
        if -1 in shape:
            known = 1
            for s in shape:
                if s != -1:
                    known *= s
            shape[shape.index(-1)] = n // known if known else 0
        if _size_of(shape) != n:
            raise ValueError("cannot reshape array of size %d into shape %s" % (n, tuple(shape)))
        return ndarray(list(self._data), tuple(shape), self._dtype)
    def transpose(self, *axes):
        if self.ndim < 2:
            return self.copy()
        if not axes:
            axes = tuple(range(self.ndim))[::-1]
        elif len(axes) == 1 and isinstance(axes[0], (tuple, list)):
            axes = tuple(axes[0])
        new_shape = tuple(self._shape[a] for a in axes)
        old_st = _strides_for(self._shape)
        out = [None] * len(self._data)
        new_st = _strides_for(new_shape)
        for flat in range(len(self._data)):
            midx = _unravel(flat, new_shape)
            old_idx = [0] * self.ndim
            for new_ax, old_ax in enumerate(axes):
                old_idx[old_ax] = midx[new_ax]
            o = _bsum(i * s for i, s in zip(old_idx, old_st))
            out[flat] = self._data[o]
        return ndarray(out, new_shape, self._dtype)

    # ---- indexing ----
    def __getitem__(self, key):
        return _getitem(self, key)
    def __setitem__(self, key, value):
        _setitem(self, key, value)
    def __iter__(self):
        if self.ndim == 0:
            raise TypeError("iteration over a 0-d array")
        if self.ndim == 1:
            return iter(self._data)
        return (self[i] for i in range(self._shape[0]))

    # ---- arithmetic / comparison ----
    def __add__(self, o): return _binop(self, o, lambda a, b: a + b)
    def __radd__(self, o): return _binop(o, self, lambda a, b: a + b)
    def __sub__(self, o): return _binop(self, o, lambda a, b: a - b)
    def __rsub__(self, o): return _binop(o, self, lambda a, b: a - b)
    def __mul__(self, o): return _binop(self, o, lambda a, b: a * b)
    def __rmul__(self, o): return _binop(o, self, lambda a, b: a * b)
    def __truediv__(self, o): return _binop(self, o, lambda a, b: a / b, force_float=True)
    def __rtruediv__(self, o): return _binop(o, self, lambda a, b: a / b, force_float=True)
    def __floordiv__(self, o): return _binop(self, o, lambda a, b: a // b)
    def __rfloordiv__(self, o): return _binop(o, self, lambda a, b: a // b)
    def __mod__(self, o): return _binop(self, o, lambda a, b: a % b)
    def __pow__(self, o): return _binop(self, o, lambda a, b: a ** b)
    def __matmul__(self, o): return dot(self, o)
    def __neg__(self): return ndarray([-v for v in self._data], self._shape, self._dtype)
    def __abs__(self): return ndarray([_babs(v) for v in self._data], self._shape, self._dtype)
    def __pos__(self): return self.copy()

    def __lt__(self, o): return _binop(self, o, lambda a, b: a < b, out_dtype="bool")
    def __le__(self, o): return _binop(self, o, lambda a, b: a <= b, out_dtype="bool")
    def __gt__(self, o): return _binop(self, o, lambda a, b: a > b, out_dtype="bool")
    def __ge__(self, o): return _binop(self, o, lambda a, b: a >= b, out_dtype="bool")
    def __eq__(self, o): return _binop(self, o, lambda a, b: a == b, out_dtype="bool")
    def __ne__(self, o): return _binop(self, o, lambda a, b: a != b, out_dtype="bool")
    def __invert__(self):
        return ndarray([not v for v in self._data], self._shape, "bool")
    def __and__(self, o): return _binop(self, o, lambda a, b: bool(a) and bool(b), out_dtype="bool")
    def __or__(self, o): return _binop(self, o, lambda a, b: bool(a) or bool(b), out_dtype="bool")

    def __iadd__(self, o): r = self + o; self._data = r._data; self._shape = r._shape; return self
    def __isub__(self, o): r = self - o; self._data = r._data; self._shape = r._shape; return self
    def __imul__(self, o): r = self * o; self._data = r._data; self._shape = r._shape; return self

    def __bool__(self):
        if len(self._data) == 1:
            return bool(self._data[0])
        if len(self._data) == 0:
            return False
        raise ValueError("The truth value of an array with more than one element is ambiguous. Use a.any() or a.all()")
    def __float__(self):
        if len(self._data) == 1:
            return float(self._data[0])
        raise TypeError("only size-1 arrays can be converted to Python scalars")
    def __int__(self):
        if len(self._data) == 1:
            return int(self._data[0])
        raise TypeError("only size-1 arrays can be converted to Python scalars")

    # ---- reductions ----
    def sum(self, axis=None, dtype=None): return _reduce(self, axis, lambda xs: _bsum(xs), 0)
    def prod(self, axis=None): return _reduce(self, axis, lambda xs: _prod(xs), 1)
    def mean(self, axis=None): return _reduce(self, axis, lambda xs: (_bsum(xs) / len(xs)) if xs else float("nan"), float("nan"), force_float=True)
    def min(self, axis=None): return _reduce(self, axis, lambda xs: _bmin(xs), None)
    def max(self, axis=None): return _reduce(self, axis, lambda xs: _bmax(xs), None)
    def std(self, axis=None, ddof=0): return _reduce(self, axis, lambda xs: _std(xs, ddof), float("nan"), force_float=True)
    def var(self, axis=None, ddof=0): return _reduce(self, axis, lambda xs: _var(xs, ddof), float("nan"), force_float=True)
    def all(self, axis=None): return _reduce(self, axis, lambda xs: _ball(xs), True, out_dtype="bool")
    def any(self, axis=None): return _reduce(self, axis, lambda xs: _bany(xs), False, out_dtype="bool")
    def argmin(self, axis=None):
        if axis is not None: _ood("argmin(axis=...)")
        return int(_bmin(range(len(self._data)), key=lambda i: self._data[i]))
    def argmax(self, axis=None):
        if axis is not None: _ood("argmax(axis=...)")
        return int(_bmax(range(len(self._data)), key=lambda i: self._data[i]))
    def argsort(self, axis=-1, kind=None):
        if self.ndim != 1:
            _ood("argsort on >1d")
        idx = sorted(range(len(self._data)), key=lambda i: self._data[i])
        return ndarray([int(i) for i in idx], (len(idx),), "int64")
    def round(self, decimals=0):
        return ndarray([round(v, decimals) for v in self._data], self._shape, self._dtype)
    def clip(self, lo=None, hi=None):
        # bounds may be scalars OR arrays (np.clip(x, 0, per_element_max)) -> broadcast via the
        # element-wise maximum/minimum helpers.
        r = self
        if lo is not None:
            r = maximum(r, lo)
        if hi is not None:
            r = minimum(r, hi)
        return r
    def dot(self, o):
        return dot(self, o)
    def nonzero(self):
        idx = [i for i, v in enumerate(self._data) if v]
        return (ndarray(idx, (len(idx),), "int64"),)

    def __repr__(self):
        return "array(" + repr(self.tolist()) + ")"
    def __str__(self):
        return str(self.tolist())
    __hash__ = None


def _to_nested(flat, shape):
    if not shape:
        return flat[0]
    if len(shape) == 1:
        return list(flat[: shape[0]])
    st = _strides_for(shape)
    return [_to_nested(flat[i * st[0]:(i + 1) * st[0]], shape[1:]) for i in range(shape[0])]

def _unravel(flat, shape):
    idx = []
    st = _strides_for(shape)
    for s in st:
        idx.append(flat // s)
        flat %= s
    return tuple(idx)

def _prod(xs):
    p = 1
    for x in xs:
        p *= x
    return p

def _var(xs, ddof=0):
    n = len(xs)
    if n - ddof <= 0:
        return float("nan")
    m = _bsum(xs) / n
    return _bsum((x - m) ** 2 for x in xs) / (n - ddof)

def _std(xs, ddof=0):
    v = _var(xs, ddof)
    return _math.sqrt(v) if v == v else v


# --------------------------------------------------------------------------- broadcasting
def _broadcast_shapes(a, b):
    ra, rb = list(a)[::-1], list(b)[::-1]
    out = []
    for i in range(_bmax(len(ra), len(rb))):
        da = ra[i] if i < len(ra) else 1
        db = rb[i] if i < len(rb) else 1
        # broadcasting a size-1 dim against size N yields N (incl. N == 0); only equal-or-1 pairs
        # are compatible.
        if da == db:
            out.append(da)
        elif da == 1:
            out.append(db)
        elif db == 1:
            out.append(da)
        else:
            _ood("incompatible broadcast %s vs %s" % (a, b))
            out.append(_bmax(da, db))
    return tuple(out[::-1])

def _broadcast_index(midx, shape):
    """Map an output multi-index to a flat index into an operand of `shape` (size-1 dims repeat)."""
    off = len(midx) - len(shape)
    st = _strides_for(shape)
    flat = 0
    for ax in range(len(shape)):
        i = midx[off + ax]
        if shape[ax] == 1:
            i = 0
        flat += i * st[ax]
    return flat

def _binop(a, b, op, out_dtype=None, force_float=False):
    aa, ash = _operand(a)
    bb, bsh = _operand(b)
    # Fast paths for the overwhelmingly common cases — equal shapes and scalar operands — avoid
    # the per-element _unravel + _broadcast_index machinery (a large hot-loop win, e.g. `A += w*x`).
    if ash == bsh:
        out = [op(x, y) for x, y in zip(aa, bb)]
        shape = ash
    elif bsh == ():
        y = bb[0]
        out = [op(x, y) for x in aa]
        shape = ash
    elif ash == ():
        x = aa[0]
        out = [op(x, y) for y in bb]
        shape = bsh
    else:
        shape = _broadcast_shapes(ash, bsh)
        n = _size_of(shape)
        out = [None] * n
        for flat in range(n):
            midx = _unravel(flat, shape)
            out[flat] = op(aa[_broadcast_index(midx, ash)], bb[_broadcast_index(midx, bsh)])
    if out_dtype is None:
        out_dtype = "float64" if (force_float or _any_float(out)) else _infer_dtype(out)
    if shape == ():
        return out[0]
    return ndarray(out, shape, out_dtype)

def _operand(x):
    if isinstance(x, ndarray):
        return x._data, x._shape
    if isinstance(x, (list, tuple)) or _is_arraylike(x):
        f, s = _flat_from_nested(x)
        return f, s
    return [x], ()

def _any_float(xs):
    for v in xs:
        if isinstance(v, float) and not isinstance(v, bool):
            return True
    return False


def _reduce(arr, axis, fn, empty, out_dtype=None, force_float=False):
    if axis is None:
        if not arr._data:
            return empty
        r = fn(list(arr._data))
        return r
    # multi-axis reduction, e.g. np.max(x, axis=(0, 1)) in pooling / batchnorm.
    if isinstance(axis, (tuple, list)):
        axes = sorted((a + arr.ndim if a < 0 else a) for a in axis)
        shape = arr._shape
        st = _strides_for(shape)
        kept = [i for i in range(arr.ndim) if i not in axes]
        new_shape = tuple(shape[i] for i in kept)
        red_shape = tuple(shape[a] for a in axes)
        out = []
        for flat in range(_size_of(new_shape) if new_shape else 1):
            kidx = list(_unravel(flat, new_shape)) if new_shape else []
            base = [0] * arr.ndim
            for pos, i in enumerate(kept):
                base[i] = kidx[pos]
            xs = []
            for rflat in range(_size_of(red_shape) if red_shape else 1):
                ridx = list(_unravel(rflat, red_shape)) if red_shape else []
                full = list(base)
                for pos, a in enumerate(axes):
                    full[a] = ridx[pos]
                xs.append(arr._data[_bsum(i * s for i, s in zip(full, st))])
            out.append(fn(xs))
        if not new_shape:
            return out[0]
        if out_dtype is None:
            out_dtype = "float64" if force_float or _any_float(out) else arr._dtype
        return ndarray(out, new_shape, out_dtype)
    if axis < 0:
        axis += arr.ndim
    shape = arr._shape
    new_shape = tuple(s for i, s in enumerate(shape) if i != axis)
    st = _strides_for(shape)
    out = []
    new_st = _strides_for(new_shape) if new_shape else [1]
    for flat in range(_size_of(new_shape)):
        midx = list(_unravel(flat, new_shape)) if new_shape else []
        full = midx[:axis] + [0] + midx[axis:]
        xs = []
        for k in range(shape[axis]):
            full[axis] = k
            xs.append(arr._data[_bsum(i * s for i, s in zip(full, st))])
        out.append(fn(xs))
    if not new_shape:
        return out[0]
    if out_dtype is None:
        out_dtype = "float64" if force_float or _any_float(out) else arr._dtype
    return ndarray(out, new_shape, out_dtype)


# --------------------------------------------------------------------------- get/set
def _getitem(arr, key):
    # boolean-mask or integer-array fancy indexing (single key)
    if isinstance(key, ndarray):
        if key._dtype == "bool":
            if key._shape == arr._shape:
                sel = [v for v, m in zip(arr._data, key._data) if m]
                return ndarray(sel, (len(sel),), arr._dtype)
            if len(key._shape) == 1 and arr.ndim >= 1 and key._shape[0] == arr._shape[0]:
                rows = [i for i, m in enumerate(key._data) if m]
                return _take_rows(arr, rows)
            _ood("boolean index shape mismatch")
            return arr.copy()
        else:
            return _take_rows(arr, [int(i) for i in key._data])
    if isinstance(key, list):
        return _take_rows(arr, [int(i) for i in key])
    if not isinstance(key, tuple):
        key = (key,)
    # expand a single Ellipsis
    if _bany(k is Ellipsis for k in key):
        n_real = _bsum(1 for k in key if k is not Ellipsis and k is not None)
        fill = arr.ndim - n_real
        new = []
        for k in key:
            if k is Ellipsis:
                new.extend([slice(None)] * fill)
            else:
                new.append(k)
        key = tuple(new)
    # pad with full slices
    key = key + (slice(None),) * (arr.ndim - len(key))
    axis_indices = []
    drop = []
    for ax, k in enumerate(key):
        n = arr._shape[ax]
        if isinstance(k, slice):
            axis_indices.append(list(range(*k.indices(n))))
            drop.append(False)
        else:
            ki = int(k)
            if ki < 0:
                ki += n
            axis_indices.append([ki])
            drop.append(True)
    st = _strides_for(arr._shape)
    out = []
    out_shape = [len(ix) for ix, d in zip(axis_indices, drop) if not d]
    for combo in _cartesian(axis_indices):
        out.append(arr._data[_bsum(i * s for i, s in zip(combo, st))])
    if _ball(drop):
        return out[0]
    return ndarray(out, tuple(out_shape), arr._dtype)

def _take_rows(arr, rows):
    if arr.ndim == 1:
        return ndarray([arr._data[i] for i in rows], (len(rows),), arr._dtype)
    row_size = _size_of(arr._shape[1:])
    out = []
    for r in rows:
        out.extend(arr._data[r * row_size:(r + 1) * row_size])
    return ndarray(out, (len(rows),) + arr._shape[1:], arr._dtype)

def _cartesian(lists):
    if not lists:
        yield ()
        return
    head = lists[0]
    for h in head:
        for rest in _cartesian(lists[1:]):
            yield (h,) + rest

def _setitem(arr, key, value):
    # boolean mask assignment
    if isinstance(key, ndarray) and key._dtype == "bool":
        vals = value._data if isinstance(value, ndarray) else None
        j = 0
        for i, m in enumerate(key._data):
            if m:
                arr._data[i] = (vals[j] if vals is not None else value)
                j += 1
        return
    if isinstance(key, ndarray):  # int fancy
        key = [int(i) for i in key._data]
    if isinstance(key, list):
        vals = value._data if isinstance(value, ndarray) else None
        for j, i in enumerate(key):
            arr._data[int(i)] = vals[j] if vals is not None else value
        return
    if not isinstance(key, tuple):
        key = (key,)
    if _bany(k is Ellipsis for k in key):
        n_real = _bsum(1 for k in key if k is not Ellipsis)
        fill = arr.ndim - n_real
        new = []
        for k in key:
            if k is Ellipsis:
                new.extend([slice(None)] * fill)
            else:
                new.append(k)
        key = tuple(new)
    key = key + (slice(None),) * (arr.ndim - len(key))
    axis_indices = []
    for ax, k in enumerate(key):
        n = arr._shape[ax]
        if isinstance(k, slice):
            axis_indices.append(list(range(*k.indices(n))))
        else:
            ki = int(k)
            if ki < 0:
                ki += n
            axis_indices.append([ki])
    st = _strides_for(arr._shape)
    targets = [_bsum(i * s for i, s in zip(combo, st)) for combo in _cartesian(axis_indices)]
    if isinstance(value, ndarray):
        vals = value._data
        if len(vals) == 1:
            for t in targets:
                arr._data[t] = vals[0]
        else:
            for t, v in zip(targets, vals):
                arr._data[t] = v
    else:
        for t in targets:
            arr._data[t] = value


# --------------------------------------------------------------------------- creation
def array(obj, dtype=None, copy=True, ndmin=0):
    if isinstance(obj, ndarray):
        a = obj.copy()
    else:
        flat, shape = _flat_from_nested(obj)
        name = _norm_dtype(dtype) if dtype is not None else _infer_dtype(flat)
        a = ndarray(_cast_list(flat, name), shape, name)
    if dtype is not None:
        a = a.astype(dtype)
    while a.ndim < ndmin:
        a._shape = (1,) + a._shape
    return a

def asarray(obj, dtype=None):
    if isinstance(obj, ndarray) and dtype is None:
        return obj
    return array(obj, dtype=dtype)

def _filled(shape, value, dtype):
    if isinstance(shape, int):
        shape = (shape,)
    shape = tuple(shape)
    name = _norm_dtype(dtype) or ("float64" if isinstance(value, float) else "int64")
    return ndarray([value] * _size_of(shape), shape, name)

def zeros(shape, dtype="float64"):
    return _filled(shape, 0.0 if _norm_dtype(dtype) in ("float64", "float32") else 0, dtype)
def ones(shape, dtype="float64"):
    return _filled(shape, 1.0 if _norm_dtype(dtype) in ("float64", "float32") else 1, dtype)
def full(shape, value, dtype=None):
    return _filled(shape, value, dtype)
def empty(shape, dtype="float64"):
    return zeros(shape, dtype)
def zeros_like(a, dtype=None):
    return zeros(a._shape, dtype or a._dtype)
def ones_like(a, dtype=None):
    return ones(a._shape, dtype or a._dtype)
def full_like(a, value, dtype=None):
    return full(a._shape, value, dtype or a._dtype)

def arange(*args, dtype=None):
    if len(args) == 1:
        start, stop, step = 0, args[0], 1
    elif len(args) == 2:
        start, stop, step = args[0], args[1], 1
    else:
        start, stop, step = args[0], args[1], args[2]
    out = []
    v = start
    if step > 0:
        while v < stop:
            out.append(v); v += step
    else:
        while v > stop:
            out.append(v); v += step
    name = _norm_dtype(dtype) if dtype else ("float64" if _bany(isinstance(x, float) for x in (start, stop, step)) else "int64")
    return ndarray(_cast_list(out, name), (len(out),), name)

def linspace(start, stop, num=50, endpoint=True, dtype=None):
    if num <= 0:
        return ndarray([], (0,), "float64")
    if num == 1:
        return ndarray([float(start)], (1,), "float64")
    div = (num - 1) if endpoint else num
    step = (stop - start) / div
    out = [float(start + i * step) for i in range(num)]
    if endpoint:
        out[-1] = float(stop)
    return ndarray(out, (num,), _norm_dtype(dtype) or "float64")

def eye(n, m=None, dtype="float64"):
    m = n if m is None else m
    data = [0.0] * (n * m)
    for i in range(_bmin(n, m)):
        data[i * m + i] = 1.0
    return ndarray(data, (n, m), _norm_dtype(dtype) or "float64")

def identity(n, dtype="float64"):
    return eye(n, dtype=dtype)

def diag(v, k=0):
    a = asarray(v)
    if a.ndim == 1:
        n = a._shape[0] + _babs(k)
        out = zeros((n, n))
        for i in range(a._shape[0]):
            out[i + (0 if k >= 0 else -k), i + (k if k >= 0 else 0)] = a._data[i]
        return out
    if a.ndim == 2:
        n = _bmin(a._shape)
        return ndarray([a[i, i] for i in range(n)], (n,), a._dtype)
    _ood("diag of >2d")
    return a


# --------------------------------------------------------------------------- elementwise ufuncs
def _ew(x, fn, out_dtype="float64"):
    if isinstance(x, ndarray):
        return ndarray([fn(v) for v in x._data], x._shape, out_dtype)
    if isinstance(x, (list, tuple)):
        return _ew(array(x), fn, out_dtype)
    return fn(x)

def sqrt(x): return _ew(x, _math.sqrt)
def exp(x): return _ew(x, _math.exp)
def log(x): return _ew(x, _math.log)
def log2(x): return _ew(x, _math.log2)
def log10(x): return _ew(x, _math.log10)
def log1p(x): return _ew(x, _math.log1p)
def sin(x): return _ew(x, _math.sin)
def cos(x): return _ew(x, _math.cos)
def tan(x): return _ew(x, _math.tan)
def tanh(x): return _ew(x, _math.tanh)
def floor(x): return _ew(x, _math.floor)
def ceil(x): return _ew(x, _math.ceil)
def sign(x): return _ew(x, lambda v: (v > 0) - (v < 0))
def absolute(x): return _ew(x, _babs, out_dtype=(x._dtype if isinstance(x, ndarray) else "float64"))
abs = absolute
def square(x): return _ew(x, lambda v: v * v)
def rint(x): return _ew(x, lambda v: float(round(v)))
def isnan(x): return _ew(x, lambda v: v != v, out_dtype="bool")
def isinf(x): return _ew(x, lambda v: v in (float("inf"), float("-inf")), out_dtype="bool")
def isfinite(x): return _ew(x, lambda v: v == v and v not in (float("inf"), float("-inf")), out_dtype="bool")
def maximum(a, b): return _binop(a, b, lambda x, y: x if x >= y else y)
def minimum(a, b): return _binop(a, b, lambda x, y: x if x <= y else y)
def power(a, b): return _binop(a, b, lambda x, y: x ** y)
def mod(a, b): return _binop(a, b, lambda x, y: x % y)


# --------------------------------------------------------------------------- reductions (free fns)
def _arr(x):
    return x if isinstance(x, ndarray) else array(x)

def sum(a, axis=None, dtype=None): return _arr(a).sum(axis=axis)
def prod(a, axis=None): return _arr(a).prod(axis=axis)
def mean(a, axis=None): return _arr(a).mean(axis=axis)
def std(a, axis=None, ddof=0): return _arr(a).std(axis=axis, ddof=ddof)
def var(a, axis=None, ddof=0): return _arr(a).var(axis=axis, ddof=ddof)
def amin(a, axis=None): return _arr(a).min(axis=axis)
def amax(a, axis=None): return _arr(a).max(axis=axis)
def median(a, axis=None):
    arr = _arr(a)
    if axis is not None:
        _ood("median(axis=...)")
    xs = sorted(arr._data)
    n = len(xs)
    if n == 0:
        return float("nan")
    return float(xs[n // 2]) if n % 2 else (xs[n // 2 - 1] + xs[n // 2]) / 2.0

def _percentile_one(xs, qf):
    """Linear-interpolated percentile (numpy's default method) of sorted-able data; qf in [0,1]."""
    if not xs:
        return float("nan")
    s = sorted(xs)
    n = len(s)
    rank = qf * (n - 1)
    lo = int(_math.floor(rank))
    hi = _bmin(lo + 1, n - 1)
    frac = rank - lo
    return float(s[lo] + frac * (s[hi] - s[lo]))

def percentile(a, q, axis=None):
    arr = _arr(a)
    if axis is not None:
        _ood("percentile(axis=...)")
    if isinstance(q, (list, tuple)) or (hasattr(q, "_data") and hasattr(q, "_shape")):
        qs = q._data if hasattr(q, "_data") else q
        return array([_percentile_one(arr._data, float(x) / 100.0) for x in qs])
    return _percentile_one(arr._data, float(q) / 100.0)

def quantile(a, q, axis=None):
    arr = _arr(a)
    if axis is not None:
        _ood("quantile(axis=...)")
    if isinstance(q, (list, tuple)) or (hasattr(q, "_data") and hasattr(q, "_shape")):
        qs = q._data if hasattr(q, "_data") else q
        return array([_percentile_one(arr._data, float(x)) for x in qs])
    return _percentile_one(arr._data, float(q))

def _min(a, axis=None): return _arr(a).min(axis=axis)
def _max(a, axis=None): return _arr(a).max(axis=axis)
min = amin
max = amax
def argmin(a, axis=None): return _arr(a).argmin(axis=axis)
def argmax(a, axis=None): return _arr(a).argmax(axis=axis)
def argsort(a, axis=-1, kind=None): return _arr(a).argsort(axis=axis, kind=kind)
def all(a, axis=None): return _arr(a).all(axis=axis)
def any(a, axis=None): return _arr(a).any(axis=axis)
def cumsum(a, axis=None):
    arr = _arr(a)
    if axis is not None and arr.ndim > 1:
        _ood("cumsum(axis=...) on >1d")
    out, acc = [], 0
    for v in arr._data:
        acc += v; out.append(acc)
    return ndarray(out, (len(out),), arr._dtype)
def clip(a, lo, hi): return _arr(a).clip(lo, hi)
def round_(a, decimals=0): return _arr(a).round(decimals)
around = round_

def where(cond, x=None, y=None):
    c = _arr(cond)
    if x is None and y is None:
        idx = [i for i, v in enumerate(c._data) if v]
        return (ndarray(idx, (len(idx),), "int64"),)
    xa, xsh = _operand(x)
    ya, ysh = _operand(y)
    out = []
    for flat in range(len(c._data)):
        midx = _unravel(flat, c._shape)
        xv = xa[0] if xsh == () else xa[_broadcast_index(midx, xsh)]
        yv = ya[0] if ysh == () else ya[_broadcast_index(midx, ysh)]
        out.append(xv if c._data[flat] else yv)
    return ndarray(out, c._shape, "float64" if _any_float(out) else _infer_dtype(out))

def concatenate(arrs, axis=0):
    arrs = [_arr(a) for a in arrs]
    if axis == 0 and (not arrs[0]._shape or arrs[0].ndim == 1):
        out = []
        for a in arrs:
            out.extend(a._data)
        return ndarray(out, (len(out),), arrs[0]._dtype)
    if arrs[0].ndim >= 1 and axis == 0:
        out = []
        rest = arrs[0]._shape[1:]
        total = 0
        for a in arrs:
            out.extend(a._data); total += a._shape[0]
        return ndarray(out, (total,) + rest, arrs[0]._dtype)
    _ood("concatenate(axis=%r) unsupported" % axis)
    return arrs[0]

def stack(arrs, axis=0):
    arrs = [_arr(a) for a in arrs]
    if axis == 0:
        out = []
        for a in arrs:
            out.extend(a._data)
        return ndarray(out, (len(arrs),) + arrs[0]._shape, arrs[0]._dtype)
    _ood("stack(axis=%r)" % axis)
    return arrs[0]

def vstack(arrs): return stack(arrs, 0)
def hstack(arrs): return concatenate(arrs, 0)

def dot(a, b):
    a, b = _arr(a), _arr(b)
    if a.ndim == 1 and b.ndim == 1:
        return _bsum(x * y for x, y in zip(a._data, b._data))
    if a.ndim == 2 and b.ndim == 1:
        m, k = a._shape
        return ndarray([_bsum(a[i, t] * b._data[t] for t in range(k)) for i in range(m)], (m,), "float64")
    if a.ndim == 1 and b.ndim == 2:
        k, n = b._shape
        return ndarray([_bsum(a._data[t] * b[t, j] for t in range(k)) for j in range(n)], (n,), "float64")
    if a.ndim == 2 and b.ndim == 2:
        m, k = a._shape
        k2, n = b._shape
        out = [0.0] * (m * n)
        for i in range(m):
            for j in range(n):
                out[i * n + j] = _bsum(a[i, t] * b[t, j] for t in range(k))
        return ndarray(out, (m, n), "float64")
    _ood("dot of shapes %s,%s" % (a._shape, b._shape))
    return a
matmul = dot

def outer(a, b):
    a, b = _arr(a), _arr(b)
    out = [x * y for x in a._data for y in b._data]
    return ndarray(out, (len(a._data), len(b._data)), "float64")

def allclose(a, b, rtol=1e-05, atol=1e-08, equal_nan=False):
    aa, _ = _operand(a); bb, _ = _operand(b)
    if len(aa) == 1 and len(bb) > 1:
        aa = aa * len(bb)
    if len(bb) == 1 and len(aa) > 1:
        bb = bb * len(aa)
    for x, y in zip(aa, bb):
        if x != x and y != y:
            if equal_nan: continue
            return False
        if _babs(x - y) > atol + rtol * _babs(y):
            return False
    return True

def array_equal(a, b):
    a, b = _arr(a), _arr(b)
    return a._shape == b._shape and a._data == b._data

def isclose(a, b, rtol=1e-05, atol=1e-08):
    return _binop(a, b, lambda x, y: _babs(x - y) <= atol + rtol * _babs(y), out_dtype="bool")

def pad(arr, pad_width, mode="constant", constant_values=0):
    a = _arr(arr)
    if mode != "constant":
        _ood("pad(mode=%r)" % mode)
    # normalize pad_width to per-axis (before, after)
    if isinstance(pad_width, int):
        pw = [(pad_width, pad_width)] * a.ndim
    else:
        pw = [tuple(p) if isinstance(p, (list, tuple)) else (p, p) for p in pad_width]
        if len(pw) == 1:
            pw = pw * a.ndim
    new_shape = tuple(a._shape[ax] + pw[ax][0] + pw[ax][1] for ax in range(a.ndim))
    out = full(new_shape, constant_values)
    out._dtype = a._dtype
    st = _strides_for(new_shape)
    src_st = _strides_for(a._shape)
    for flat in range(len(a._data)):
        midx = _unravel(flat, a._shape)
        nidx = [midx[ax] + pw[ax][0] for ax in range(a.ndim)]
        out._data[_bsum(i * s for i, s in zip(nidx, st))] = a._data[flat]
    return out

def save(file, arr):
    # persist a .npy-ish blob into the VFS via the patched open(); good enough to "exist".
    name = file if str(file).endswith(".npy") else str(file) + ".npy"
    try:
        with open(name, "wb") as f:
            f.write(b"\x93NUMPY-shellsim")
    except Exception:
        pass

inf = float("inf")
nan = float("nan")
NaN = nan
Inf = inf
pi = _math.pi
e = _math.e
newaxis = None


# --------------------------------------------------------------------------- random
class _RNG:
    """A small deterministic PRNG (SplitMix64 → PCG-ish). NOT numpy's Mersenne Twister: tasks
    here only require determinism + plausible distribution shape, never bit-identical streams."""
    def __init__(self, seed=0):
        self.seed(seed)
    def seed(self, s=0):
        if s is None:
            s = 0
        self._s = (int(s) & 0xFFFFFFFFFFFFFFFF) ^ 0x9E3779B97F4A7C15
        if self._s == 0:
            self._s = 0x9E3779B97F4A7C15
        self._spare = None
    def _next_u64(self):
        self._s = (self._s + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        z = self._s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        return z ^ (z >> 31)
    def random(self):
        return (self._next_u64() >> 11) / float(1 << 53)
    def gauss(self):
        if self._spare is not None:
            g = self._spare; self._spare = None; return g
        u1 = self.random() or 1e-12
        u2 = self.random()
        r = _math.sqrt(-2.0 * _math.log(u1))
        self._spare = r * _math.sin(2 * _math.pi * u2)
        return r * _math.cos(2 * _math.pi * u2)
    def randint(self, lo, hi):
        return lo + int(self._next_u64() % (hi - lo))

def _shape_args(args):
    if len(args) == 1 and isinstance(args[0], (tuple, list)):
        return tuple(args[0])
    return tuple(args)

class Generator:
    def __init__(self, rng):
        self._rng = rng
    def random(self, size=None):
        return self._fill(size, self._rng.random)
    def standard_normal(self, size=None):
        return self._fill(size, self._rng.gauss)
    def normal(self, loc=0.0, scale=1.0, size=None):
        return self._fill(size, lambda: loc + scale * self._rng.gauss())
    def uniform(self, low=0.0, high=1.0, size=None):
        return self._fill(size, lambda: low + (high - low) * self._rng.random())
    def integers(self, low, high=None, size=None, endpoint=False):
        if high is None:
            low, high = 0, low
        if endpoint:
            high += 1
        return self._fill(size, lambda: self._rng.randint(low, high), dtype="int64")
    def _fill(self, size, gen, dtype="float64"):
        if size is None:
            return gen()
        shape = (size,) if isinstance(size, int) else tuple(size)
        return ndarray([gen() for _ in range(_size_of(shape))], shape, dtype)
    def shuffle(self, arr):
        data = arr._data if isinstance(arr, ndarray) else arr
        for i in range(len(data) - 1, 0, -1):
            j = self._rng.randint(0, i + 1)
            data[i], data[j] = data[j], data[i]
    def permutation(self, x):
        if isinstance(x, int):
            a = list(range(x))
        else:
            a = list(x._data if isinstance(x, ndarray) else x)
        for i in range(len(a) - 1, 0, -1):
            j = self._rng.randint(0, i + 1)
            a[i], a[j] = a[j], a[i]
        return ndarray(a, (len(a),), "int64")
    def choice(self, a, size=None, replace=True, p=None):
        pool = list(range(a)) if isinstance(a, int) else list(a._data if isinstance(a, ndarray) else a)
        if p is not None:
            _ood("rng.choice(p=...) weighted")
        if size is None:
            return pool[self._rng.randint(0, len(pool))]
        n = size if isinstance(size, int) else _size_of(tuple(size))
        out = []
        if replace:
            for _ in range(n):
                out.append(pool[self._rng.randint(0, len(pool))])
        else:
            pool2 = list(pool)
            for _ in range(_bmin(n, len(pool2))):
                j = self._rng.randint(0, len(pool2))
                out.append(pool2.pop(j))
        return ndarray(out, (len(out),), "int64" if _ball(isinstance(v, int) for v in out) else "float64")

def default_rng(seed=None):
    return Generator(_RNG(0 if seed is None else seed))

class RandomState:
    def __init__(self, seed=None):
        self._g = Generator(_RNG(0 if seed is None else seed))
    def seed(self, s=None):
        self._g = Generator(_RNG(0 if s is None else s))
    def rand(self, *args):
        return self._g.random(_shape_args(args) or None)
    def randn(self, *args):
        return self._g.standard_normal(_shape_args(args) or None)
    def randint(self, low, high=None, size=None):
        return self._g.integers(low, high, size)
    def uniform(self, low=0.0, high=1.0, size=None):
        return self._g.uniform(low, high, size)
    def normal(self, loc=0.0, scale=1.0, size=None):
        return self._g.normal(loc, scale, size)
    def shuffle(self, arr):
        self._g.shuffle(arr)
    def permutation(self, x):
        return self._g.permutation(x)
    def choice(self, a, size=None, replace=True, p=None):
        return self._g.choice(a, size, replace, p)

# legacy module-level np.random.* backed by a single global RandomState
_GLOBAL = RandomState(0)
_random_mod = _types.ModuleType("numpy.random")
def _r_seed(s=None): _GLOBAL.seed(s)
def _r_rand(*a): return _GLOBAL.rand(*a)
def _r_randn(*a): return _GLOBAL.randn(*a)
def _r_randint(low, high=None, size=None): return _GLOBAL.randint(low, high, size)
def _r_uniform(low=0.0, high=1.0, size=None): return _GLOBAL.uniform(low, high, size)
def _r_normal(loc=0.0, scale=1.0, size=None): return _GLOBAL.normal(loc, scale, size)
def _r_shuffle(a): _GLOBAL.shuffle(a)
def _r_permutation(x): return _GLOBAL.permutation(x)
def _r_choice(a, size=None, replace=True, p=None): return _GLOBAL.choice(a, size, replace, p)
_random_mod.seed = _r_seed
_random_mod.rand = _r_rand
_random_mod.randn = _r_randn
_random_mod.randint = _r_randint
_random_mod.uniform = _r_uniform
_random_mod.normal = _r_normal
_random_mod.shuffle = _r_shuffle
_random_mod.permutation = _r_permutation
_random_mod.choice = _r_choice
_random_mod.default_rng = default_rng
_random_mod.Generator = Generator
_random_mod.RandomState = RandomState
random = _random_mod


# --------------------------------------------------------------------------- linalg
def _solve(A, b):
    A = _arr(A); b = _arr(b)
    n = A._shape[0]
    M = [[float(A[i, j]) for j in range(n)] for i in range(n)]
    bb = [[float(x)] for x in b._data] if b.ndim == 1 else [[float(b[i, j]) for j in range(b._shape[1])] for i in range(n)]
    ncol = len(bb[0])
    # Gauss-Jordan with partial pivoting
    for col in range(n):
        piv = _bmax(range(col, n), key=lambda r: _babs(M[r][col]))
        if _babs(M[piv][col]) < 1e-15:
            _ood("linalg.solve: singular matrix")
            raise _LinAlgError("Singular matrix")
        M[col], M[piv] = M[piv], M[col]
        bb[col], bb[piv] = bb[piv], bb[col]
        d = M[col][col]
        M[col] = [v / d for v in M[col]]
        bb[col] = [v / d for v in bb[col]]
        for r in range(n):
            if r != col and M[r][col] != 0:
                f = M[r][col]
                M[r] = [M[r][k] - f * M[col][k] for k in range(n)]
                bb[r] = [bb[r][k] - f * bb[col][k] for k in range(ncol)]
    if b.ndim == 1:
        return ndarray([bb[i][0] for i in range(n)], (n,), "float64")
    return ndarray([bb[i][j] for i in range(n) for j in range(ncol)], (n, ncol), "float64")

def _inv(A):
    A = _arr(A)
    n = A._shape[0]
    return _solve(A, eye(n))

def _det(A):
    A = _arr(A)
    n = A._shape[0]
    M = [[float(A[i, j]) for j in range(n)] for i in range(n)]
    det = 1.0
    for col in range(n):
        piv = _bmax(range(col, n), key=lambda r: _babs(M[r][col]))
        if _babs(M[piv][col]) < 1e-15:
            return 0.0
        if piv != col:
            M[col], M[piv] = M[piv], M[col]; det = -det
        det *= M[col][col]
        for r in range(col + 1, n):
            f = M[r][col] / M[col][col]
            M[r] = [M[r][k] - f * M[col][k] for k in range(n)]
    return det

def _eigh(A):
    """Symmetric eigendecomposition via the cyclic Jacobi method. Ascending eigenvalues."""
    A = _arr(A)
    n = A._shape[0]
    a = [[float(A[i, j]) for j in range(n)] for i in range(n)]
    v = [[1.0 if i == j else 0.0 for j in range(n)] for i in range(n)]
    for _sweep in range(100):
        off = _bsum(a[i][j] ** 2 for i in range(n) for j in range(i + 1, n))
        if off < 1e-20:
            break
        for p in range(n):
            for q in range(p + 1, n):
                if _babs(a[p][q]) < 1e-18:
                    continue
                theta = (a[q][q] - a[p][p]) / (2 * a[p][q])
                t = (1 if theta >= 0 else -1) / (_babs(theta) + _math.sqrt(theta * theta + 1))
                c = 1 / _math.sqrt(t * t + 1)
                s = t * c
                for k in range(n):
                    akp, akq = a[k][p], a[k][q]
                    a[k][p] = c * akp - s * akq
                    a[k][q] = s * akp + c * akq
                for k in range(n):
                    apk, aqk = a[p][k], a[q][k]
                    a[p][k] = c * apk - s * aqk
                    a[q][k] = s * apk + c * aqk
                for k in range(n):
                    vkp, vkq = v[k][p], v[k][q]
                    v[k][p] = c * vkp - s * vkq
                    v[k][q] = s * vkp + c * vkq
    eigvals = [a[i][i] for i in range(n)]
    order = sorted(range(n), key=lambda i: eigvals[i])
    w = [eigvals[i] for i in order]
    vecs = [v[r][order[c]] for r in range(n) for c in range(n)]
    return ndarray(w, (n,), "float64"), ndarray(vecs, (n, n), "float64")

def _norm(x, ord=None, axis=None):
    a = _arr(x)
    if axis is None:
        return _math.sqrt(_bsum(float(v) * float(v) for v in a._data))
    _ood("linalg.norm(axis=...)")
    return 0.0

class _LinAlgError(Exception):
    pass

_linalg_mod = _types.ModuleType("numpy.linalg")
_linalg_mod.solve = _solve
_linalg_mod.inv = _inv
_linalg_mod.det = _det
_linalg_mod.eigh = _eigh
_linalg_mod.norm = _norm
_linalg_mod.LinAlgError = _LinAlgError
linalg = _linalg_mod
LinAlgError = _LinAlgError


# --------------------------------------------------------------------------- testing
def _assert_array_equal(a, b, err_msg=""):
    a, b = _arr(a), _arr(b)
    if a._shape != b._shape or a._data != b._data:
        raise AssertionError("Arrays are not equal%s\n x: %r\n y: %r" % (
            (": " + err_msg) if err_msg else "", a.tolist(), b.tolist()))

def _assert_allclose(a, b, rtol=1e-07, atol=0, err_msg="", equal_nan=True):
    if not allclose(a, b, rtol=rtol, atol=atol, equal_nan=equal_nan):
        aa, bb = _arr(a), _arr(b)
        raise AssertionError("Not equal to tolerance rtol=%g, atol=%g%s\n x: %r\n y: %r" % (
            rtol, atol, (": " + err_msg) if err_msg else "", aa.tolist(), bb.tolist()))

def _assert_almost_equal(a, b, decimal=7, err_msg=""):
    _assert_allclose(a, b, rtol=0, atol=1.5 * 10 ** (-decimal), err_msg=err_msg)

def _assert_array_almost_equal(a, b, decimal=6, err_msg=""):
    _assert_almost_equal(a, b, decimal=decimal, err_msg=err_msg)

def _assert_equal(a, b, err_msg=""):
    if isinstance(a, ndarray) or isinstance(b, ndarray):
        _assert_array_equal(a, b, err_msg)
    elif a != b:
        raise AssertionError("Items are not equal%s\n x: %r\n y: %r" % (
            (": " + err_msg) if err_msg else "", a, b))

_testing_mod = _types.ModuleType("numpy.testing")
_testing_mod.assert_array_equal = _assert_array_equal
_testing_mod.assert_allclose = _assert_allclose
_testing_mod.assert_almost_equal = _assert_almost_equal
_testing_mod.assert_array_almost_equal = _assert_array_almost_equal
_testing_mod.assert_equal = _assert_equal
testing = _testing_mod


# --------------------------------------------------------------------------- register submodules
__version__ = "1.26.0-shellsim"
_sys.modules["numpy.random"] = _random_mod
_sys.modules["numpy.linalg"] = _linalg_mod
_sys.modules["numpy.testing"] = _testing_mod
