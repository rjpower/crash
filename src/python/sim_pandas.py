"""shellsim's deliberately-simple, pure-Python ``pandas``.

Column-oriented: a DataFrame is an ordered dict of column-lists plus an index list. Covers the
measured surface (read_csv/read_json, iloc/loc/iterrows/columns/values, groupby/merge, the common
reductions and cleaning ops). Unimplemented paths call ``_shellsim_ood(...)`` (→ `low` trust)
rather than guessing.
"""
import sys as _sys
import types as _types
import json as _json
import csv as _csv
import io as _io
import math as _math

try:
    import numpy as _np
except Exception:
    _np = None

def _ood(msg):
    try:
        _shellsim_ood("pandas: " + msg)
    except Exception:
        pass

_NA = float("nan")

def isna(x):
    if x is None:
        return True
    if isinstance(x, float):
        return x != x
    if isinstance(x, Series):
        return Series([isna(v) for v in x._data], list(x._index), x.name)
    if isinstance(x, DataFrame):
        return x._apply_elementwise(isna)
    return False

def notna(x):
    r = isna(x)
    if isinstance(r, bool):
        return not r
    if isinstance(r, Series):
        return Series([not v for v in r._data], list(r._index), r.name)
    return r

isnull = isna
notnull = notna


def _infer_cell(s):
    if s is None:
        return _NA
    t = s.strip() if isinstance(s, str) else s
    if t == "" or t is None:
        return _NA
    if not isinstance(t, str):
        return t
    try:
        return int(t)
    except ValueError:
        pass
    try:
        f = float(t)
        return f
    except ValueError:
        return s

def _coerce_column(vals):
    """Give a column a homogeneous pandas-ish dtype: all-int -> int, any-float -> float, else str."""
    nonnull = [v for v in vals if not (v is None or (isinstance(v, float) and v != v))]
    if not nonnull:
        return list(vals)
    if all(isinstance(v, bool) for v in nonnull):
        return list(vals)
    if all(isinstance(v, int) and not isinstance(v, bool) for v in nonnull):
        return list(vals)
    if all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in nonnull):
        return [float(v) if not (isinstance(v, float) and v != v) else v for v in vals]
    return [("" if (v is None or (isinstance(v, float) and v != v)) else str(v)) for v in vals]


# --------------------------------------------------------------------------- Series
class _StrAccessor:
    def __init__(self, s):
        self._s = s
    def _map(self, fn):
        return Series([(fn(v) if isinstance(v, str) else _NA) for v in self._s._data], list(self._s._index), self._s.name)
    def strip(self): return self._map(str.strip)
    def lower(self): return self._map(str.lower)
    def upper(self): return self._map(str.upper)
    def title(self): return self._map(str.title)
    def len(self): return Series([(len(v) if isinstance(v, str) else _NA) for v in self._s._data], list(self._s._index), self._s.name)
    def contains(self, pat, regex=True, na=False):
        import re as _re
        def f(v):
            if not isinstance(v, str):
                return na
            return bool(_re.search(pat, v)) if regex else (pat in v)
        return Series([f(v) for v in self._s._data], list(self._s._index), self._s.name)
    def replace(self, a, b, regex=False):
        import re as _re
        def f(v):
            if not isinstance(v, str): return v
            return _re.sub(a, b, v) if regex else v.replace(a, b)
        return self._map(f)
    def startswith(self, p): return Series([(v.startswith(p) if isinstance(v, str) else False) for v in self._s._data], list(self._s._index), self._s.name)
    def split(self, sep=None, expand=False):
        if expand:
            _ood("str.split(expand=True)")
        return Series([(v.split(sep) if isinstance(v, str) else v) for v in self._s._data], list(self._s._index), self._s.name)
    def get(self, i):
        return self._map(lambda v: v[i] if len(v) > i else _NA)

class _ILocS:
    def __init__(self, s): self._s = s
    def __getitem__(self, k):
        if isinstance(k, slice):
            return Series(self._s._data[k], self._s._index[k], self._s.name)
        return self._s._data[k]

class _LocS:
    def __init__(self, s): self._s = s
    def __getitem__(self, k):
        if isinstance(k, Series) and k._is_bool():
            return self._s[k]
        return self._s._data[self._s._index.index(k)]
    def __setitem__(self, k, v):
        self._s._data[self._s._index.index(k)] = v

class Series:
    def __init__(self, data, index=None, name=None, dtype=None):
        if isinstance(data, dict):
            index = list(data.keys()); data = list(data.values())
        elif _np is not None and isinstance(data, _np.ndarray):
            data = list(data._data)
        else:
            data = list(data)
        self._data = data
        self._index = list(index) if index is not None else list(range(len(data)))
        self.name = name

    def _is_bool(self):
        return all(isinstance(v, bool) for v in self._data) and len(self._data) > 0

    # attributes
    @property
    def values(self):
        return _np.array(self._data) if _np is not None else list(self._data)
    @property
    def index(self):
        return _Index(self._index)
    @property
    def shape(self):
        return (len(self._data),)
    @property
    def size(self):
        return len(self._data)
    @property
    def dtype(self):
        return _np.array(self._data).dtype if _np is not None else "object"
    @property
    def str(self):
        return _StrAccessor(self)
    @property
    def iloc(self):
        return _ILocS(self)
    @property
    def loc(self):
        return _LocS(self)
    @property
    def empty(self):
        return len(self._data) == 0

    def __len__(self): return len(self._data)
    def __iter__(self): return iter(self._data)
    def __repr__(self): return "Series(%r)" % (self._data,)

    def __getitem__(self, k):
        if isinstance(k, Series) and k._is_bool():
            return Series([v for v, m in zip(self._data, k._data) if m],
                          [i for i, m in zip(self._index, k._data) if m], self.name)
        if isinstance(k, slice):
            return Series(self._data[k], self._index[k], self.name)
        if isinstance(k, list):
            return Series([self._data[self._index.index(x)] for x in k], k, self.name)
        if k in self._index:
            return self._data[self._index.index(k)]
        return self._data[k]
    def __setitem__(self, k, v):
        if k in self._index:
            self._data[self._index.index(k)] = v
        else:
            self._index.append(k); self._data.append(v)

    # reductions
    def _num(self):
        return [v for v in self._data if isinstance(v, (int, float)) and not (isinstance(v, float) and v != v)]
    def sum(self): return sum(self._num())
    def mean(self):
        n = self._num(); return (sum(n) / len(n)) if n else _NA
    def median(self):
        n = sorted(self._num())
        if not n: return _NA
        m = len(n); return float(n[m // 2]) if m % 2 else (n[m // 2 - 1] + n[m // 2]) / 2.0
    def std(self, ddof=1): return _math.sqrt(self.var(ddof))
    def var(self, ddof=1):
        n = self._num()
        if len(n) - ddof <= 0: return _NA
        m = sum(n) / len(n); return sum((x - m) ** 2 for x in n) / (len(n) - ddof)
    def min(self): return min(self._num()) if self._num() else _NA
    def max(self): return max(self._num()) if self._num() else _NA
    def count(self): return len(self._num()) if all(isinstance(v,(int,float)) for v in self._data) else sum(1 for v in self._data if not isna(v))
    def abs(self): return Series([abs(v) for v in self._data], list(self._index), self.name)
    def round(self, n=0): return Series([round(v, n) for v in self._data], list(self._index), self.name)
    def all(self, *a, **k): return all(bool(v) for v in self._data)
    def any(self, *a, **k): return any(bool(v) for v in self._data)
    def between(self, left, right, inclusive="both"):
        if inclusive == "both":
            f = lambda v: left <= v <= right
        elif inclusive == "neither":
            f = lambda v: left < v < right
        elif inclusive == "left":
            f = lambda v: left <= v < right
        else:  # "right"
            f = lambda v: left < v <= right
        return Series([f(v) for v in self._data], list(self._index), self.name)
    def sort_index(self, ascending=True):
        order = sorted(range(len(self._index)), key=lambda i: self._index[i], reverse=not ascending)
        return Series([self._data[i] for i in order], [self._index[i] for i in order], self.name)

    def unique(self):
        seen, out = set(), []
        for v in self._data:
            key = v if not (isinstance(v, float) and v != v) else "__nan__"
            if key not in seen:
                seen.add(key); out.append(v)
        return _np.array(out) if _np is not None else out
    def nunique(self, dropna=True):
        vals = [v for v in self._data if not (dropna and isna(v))]
        return len(set(vals))
    def value_counts(self, dropna=True, normalize=False):
        counts = {}
        for v in self._data:
            if dropna and isna(v): continue
            counts[v] = counts.get(v, 0) + 1
        items = sorted(counts.items(), key=lambda kv: (-kv[1],))
        idx = [k for k, _ in items]; data = [v for _, v in items]
        if normalize:
            tot = sum(data) or 1; data = [d / tot for d in data]
        return Series(data, idx, self.name)
    def tolist(self): return list(self._data)
    def to_list(self): return list(self._data)
    def fillna(self, value):
        return Series([(value if isna(v) else v) for v in self._data], list(self._index), self.name)
    def dropna(self):
        keep = [(v, i) for v, i in zip(self._data, self._index) if not isna(v)]
        return Series([v for v, _ in keep], [i for _, i in keep], self.name)
    def isna(self): return Series([isna(v) for v in self._data], list(self._index), self.name)
    isnull = isna
    def notna(self): return Series([not isna(v) for v in self._data], list(self._index), self.name)
    def isin(self, vals):
        vs = set(vals._data) if isinstance(vals, Series) else set(vals)
        return Series([v in vs for v in self._data], list(self._index), self.name)
    def astype(self, dt):
        import builtins as _b
        caster = {"int": int, "int64": int, "float": float, "float64": float, "str": str, "object": str, "bool": bool}.get(
            getattr(dt, "name", dt), None)
        if caster is None and callable(dt):
            caster = dt
        if caster is None:
            _ood("Series.astype(%r)" % (dt,)); return self
        return Series([caster(v) for v in self._data], list(self._index), self.name)
    def apply(self, fn): return Series([fn(v) for v in self._data], list(self._index), self.name)
    def map(self, fn):
        if isinstance(fn, dict):
            return Series([fn.get(v, _NA) for v in self._data], list(self._index), self.name)
        return self.apply(fn)
    def head(self, n=5): return Series(self._data[:n], self._index[:n], self.name)
    def tail(self, n=5): return Series(self._data[-n:], self._index[-n:], self.name)
    def reset_index(self, drop=False):
        if drop:
            return Series(list(self._data), list(range(len(self._data))), self.name)
        _ood("Series.reset_index(drop=False)"); return self
    def sort_values(self, ascending=True):
        order = sorted(range(len(self._data)), key=lambda i: self._data[i], reverse=not ascending)
        return Series([self._data[i] for i in order], [self._index[i] for i in order], self.name)
    def to_dict(self): return dict(zip(self._index, self._data))
    def copy(self): return Series(list(self._data), list(self._index), self.name)
    def equals(self, other): return isinstance(other, Series) and self._data == other._data

    # elementwise ops -> Series
    def _binop(self, o, op):
        if isinstance(o, Series):
            ovals = o._data
        elif isinstance(o, (list, tuple)):
            ovals = list(o)
        elif hasattr(o, "_data") and hasattr(o, "_shape"):  # numpy ndarray (sim)
            ovals = list(o._data)
        else:
            return Series([op(a, o) for a in self._data], list(self._index), self.name)
        return Series([op(a, b) for a, b in zip(self._data, ovals)], list(self._index), self.name)
    def __add__(self, o): return self._binop(o, lambda a, b: a + b)
    def __sub__(self, o): return self._binop(o, lambda a, b: a - b)
    def __mul__(self, o): return self._binop(o, lambda a, b: a * b)
    def __truediv__(self, o): return self._binop(o, lambda a, b: a / b)
    def __floordiv__(self, o): return self._binop(o, lambda a, b: a // b)
    def __mod__(self, o): return self._binop(o, lambda a, b: a % b)
    def __pow__(self, o): return self._binop(o, lambda a, b: a ** b)
    def __neg__(self): return Series([-v for v in self._data], list(self._index), self.name)
    def __abs__(self): return Series([abs(v) for v in self._data], list(self._index), self.name)
    # reflected ops (scalar OP Series)
    def __radd__(self, o): return self._binop(o, lambda a, b: b + a)
    def __rsub__(self, o): return self._binop(o, lambda a, b: b - a)
    def __rmul__(self, o): return self._binop(o, lambda a, b: b * a)
    def __rtruediv__(self, o): return self._binop(o, lambda a, b: b / a)
    def __rfloordiv__(self, o): return self._binop(o, lambda a, b: b // a)
    def __rpow__(self, o): return self._binop(o, lambda a, b: b ** a)
    def __gt__(self, o): return self._binop(o, lambda a, b: a > b)
    def __ge__(self, o): return self._binop(o, lambda a, b: a >= b)
    def __lt__(self, o): return self._binop(o, lambda a, b: a < b)
    def __le__(self, o): return self._binop(o, lambda a, b: a <= b)
    def __eq__(self, o): return self._binop(o, lambda a, b: a == b)
    def __ne__(self, o): return self._binop(o, lambda a, b: a != b)
    def __and__(self, o): return self._binop(o, lambda a, b: bool(a) and bool(b))
    def __or__(self, o): return self._binop(o, lambda a, b: bool(a) or bool(b))
    def __invert__(self): return Series([not v for v in self._data], list(self._index), self.name)
    __hash__ = None


class _Index(list):
    def tolist(self): return list(self)
    @property
    def values(self):
        return _np.array(list(self)) if _np is not None else list(self)
    @property
    def str(self):
        return _StrAccessor(Series(list(self)))


# --------------------------------------------------------------------------- DataFrame
class _ILoc:
    def __init__(self, df): self._df = df
    def __getitem__(self, key):
        df = self._df
        if isinstance(key, tuple):
            r, c = key
            cols = df._order
            if isinstance(r, int) and isinstance(c, int):
                return df._cols[cols[c]][r]
            rows = list(range(*r.indices(len(df)))) if isinstance(r, slice) else (r if isinstance(r, list) else [r])
            csel = cols[c] if isinstance(c, slice) else ([cols[c]] if isinstance(c, int) else c)
            return df._subset(rows, csel if isinstance(csel, list) else cols[c] if isinstance(c, slice) else csel)
        if isinstance(key, slice):
            rows = list(range(*key.indices(len(df))))
            return df._take(rows)
        if isinstance(key, int):
            return df._row_series(key)
        if isinstance(key, list):
            return df._take(key)
        _ood("iloc[%r]" % (key,)); return df

class _Loc:
    def __init__(self, df): self._df = df
    def __getitem__(self, key):
        df = self._df
        if isinstance(key, tuple):
            r, c = key
            rows = self._rows(r)
            if isinstance(c, str):
                col = df._cols[c]
                return Series([col[i] for i in rows], [df._index[i] for i in rows], c)
            cols = c if isinstance(c, list) else df._order
            return df._take(rows)[cols] if isinstance(c, list) else df._take(rows)
        rows = self._rows(key)
        return df._take(rows)
    def _rows(self, r):
        df = self._df
        if isinstance(r, Series) and r._is_bool():
            return [i for i, m in enumerate(r._data) if m]
        if isinstance(r, slice):
            return list(range(len(df)))
        if isinstance(r, list):
            return [df._index.index(x) for x in r]
        return [df._index.index(r)]
    def __setitem__(self, key, value):
        _ood("loc[...] assignment")


class DataFrame:
    def __init__(self, data=None, columns=None, index=None):
        self._cols = {}
        self._order = []
        if data is None:
            self._index = list(index) if index is not None else []
        elif isinstance(data, dict):
            n = 0
            for k, v in data.items():
                vv = list(v._data) if isinstance(v, Series) else (list(v._data) if (_np is not None and isinstance(v, _np.ndarray)) else list(v))
                self._cols[k] = vv; self._order.append(k); n = len(vv)
            self._index = list(index) if index is not None else list(range(n))
        elif isinstance(data, list):
            # list of dicts or list of rows
            if data and isinstance(data[0], dict):
                keys = []
                for row in data:
                    for k in row:
                        if k not in keys: keys.append(k)
                self._order = list(columns) if columns else keys
                for k in self._order:
                    self._cols[k] = [row.get(k, _NA) for row in data]
            else:
                self._order = list(columns) if columns else list(range(len(data[0]) if data else 0))
                for j, k in enumerate(self._order):
                    self._cols[k] = [row[j] for row in data]
            self._index = list(index) if index is not None else list(range(len(data)))
        else:
            _ood("DataFrame(data=%s)" % type(data).__name__)
            self._index = []
        if columns is not None and isinstance(data, dict):
            self._order = [c for c in columns]

    # ---- attributes ----
    @property
    def columns(self):
        return _Index(self._order)
    @columns.setter
    def columns(self, value):
        value = list(value)
        self._cols = {nk: self._cols[ok] for nk, ok in zip(value, self._order)}
        self._order = value
    @property
    def index(self):
        return _Index(self._index)
    @property
    def shape(self):
        return (len(self._index), len(self._order))
    @property
    def values(self):
        rows = [[self._cols[c][i] for c in self._order] for i in range(len(self._index))]
        return _np.array(rows) if _np is not None else rows
    @property
    def dtypes(self):
        return Series([(_np.array(self._cols[c]).dtype if _np is not None else "object") for c in self._order], list(self._order))
    @property
    def iloc(self): return _ILoc(self)
    @property
    def loc(self): return _Loc(self)
    @property
    def empty(self): return len(self._index) == 0
    @property
    def T(self):
        _ood("DataFrame.T"); return self

    def __len__(self): return len(self._index)
    def __repr__(self): return "DataFrame(%d rows x %d cols: %r)" % (len(self._index), len(self._order), self._order)
    def __contains__(self, k): return k in self._cols
    def __iter__(self): return iter(self._order)

    # ---- selection ----
    def __getitem__(self, key):
        if isinstance(key, str):
            return Series(self._cols[key], list(self._index), key)
        if isinstance(key, list):
            return self._select_cols(key)
        if isinstance(key, Series) and key._is_bool():
            rows = [i for i, m in enumerate(key._data) if m]
            return self._take(rows)
        if isinstance(key, slice):
            return self._take(list(range(*key.indices(len(self._index)))))
        _ood("df[%r]" % (key,)); return self
    def __setitem__(self, key, value):
        if isinstance(value, Series):
            vv = list(value._data)
        elif _np is not None and isinstance(value, _np.ndarray):
            vv = list(value._data)
        elif isinstance(value, (list, tuple)):
            vv = list(value)
        else:
            vv = [value] * len(self._index)
        if key not in self._cols:
            self._order.append(key)
        self._cols[key] = vv

    def get(self, key, default=None):
        if key in self._cols:
            return self[key]
        return default
    def _select_cols(self, cols):
        d = DataFrame()
        d._order = list(cols)
        d._cols = {c: list(self._cols[c]) for c in cols}
        d._index = list(self._index)
        return d
    def _take(self, rows):
        d = DataFrame()
        d._order = list(self._order)
        d._cols = {c: [self._cols[c][i] for i in rows] for c in self._order}
        d._index = [self._index[i] for i in rows]
        return d
    def _subset(self, rows, cols):
        if isinstance(cols, str):
            return Series([self._cols[cols][i] for i in rows], [self._index[i] for i in rows], cols)
        return self._select_cols(cols)._take(rows)
    def _row_series(self, i):
        return Series([self._cols[c][i] for c in self._order], list(self._order), self._index[i])
    def _apply_elementwise(self, fn):
        d = DataFrame()
        d._order = list(self._order)
        d._cols = {c: [fn(v) for v in self._cols[c]] for c in self._order}
        d._index = list(self._index)
        return d

    # ---- iteration ----
    def iterrows(self):
        for pos, idx in enumerate(self._index):
            yield idx, self._row_series(pos)
    def itertuples(self, index=True, name="Pandas"):
        for pos, idx in enumerate(self._index):
            vals = [self._cols[c][pos] for c in self._order]
            yield tuple(([idx] if index else []) + vals)

    # ---- reductions (per numeric column -> Series) ----
    def _numeric_cols(self):
        out = []
        for c in self._order:
            vals = [v for v in self._cols[c] if not isna(v)]
            if vals and all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in vals):
                out.append(c)
        return out
    def _col_reduce(self, fn, numeric_only=True):
        cols = self._numeric_cols() if numeric_only else self._order
        return Series([fn(Series(self._cols[c])) for c in cols], cols)
    def mean(self, numeric_only=True): return self._col_reduce(lambda s: s.mean())
    def std(self, ddof=1, numeric_only=True): return self._col_reduce(lambda s: s.std(ddof))
    def var(self, ddof=1, numeric_only=True): return self._col_reduce(lambda s: s.var(ddof))
    def median(self, numeric_only=True): return self._col_reduce(lambda s: s.median())
    def sum(self, numeric_only=False): return self._col_reduce(lambda s: s.sum(), numeric_only=numeric_only)
    def min(self, numeric_only=True): return self._col_reduce(lambda s: s.min())
    def max(self, numeric_only=True): return self._col_reduce(lambda s: s.max())
    def count(self): return Series([Series(self._cols[c]).count() for c in self._order], list(self._order))
    def nunique(self): return Series([Series(self._cols[c]).nunique() for c in self._order], list(self._order))

    # ---- cleaning / transforms ----
    def copy(self, deep=True):
        d = DataFrame(); d._order = list(self._order)
        d._cols = {c: list(self._cols[c]) for c in self._order}; d._index = list(self._index)
        return d
    def head(self, n=5): return self._take(list(range(min(n, len(self._index)))))
    def tail(self, n=5): return self._take(list(range(max(0, len(self._index) - n), len(self._index))))
    def fillna(self, value):
        d = self.copy()
        for c in d._order:
            d._cols[c] = [(value if isna(v) else v) for v in d._cols[c]]
        return d
    def dropna(self, subset=None):
        cols = subset if subset else self._order
        rows = [i for i in range(len(self._index)) if not any(isna(self._cols[c][i]) for c in cols)]
        return self._take(rows)
    def drop(self, labels=None, axis=0, columns=None):
        if columns is not None:
            cols = columns if isinstance(columns, list) else [columns]
            return self._select_cols([c for c in self._order if c not in cols])
        if axis == 1:
            cols = labels if isinstance(labels, list) else [labels]
            return self._select_cols([c for c in self._order if c not in cols])
        drop_idx = labels if isinstance(labels, list) else [labels]
        rows = [i for i, idx in enumerate(self._index) if idx not in drop_idx]
        return self._take(rows)
    def astype(self, dt):
        d = self.copy()
        if isinstance(dt, dict):
            for c, t in dt.items():
                d._cols[c] = Series(d._cols[c]).astype(t)._data
        else:
            for c in d._order:
                d._cols[c] = Series(d._cols[c]).astype(dt)._data
        return d
    def reset_index(self, drop=False):
        d = self.copy()
        if not drop:
            d._cols = {"index": list(self._index)}; d._order = ["index"] + list(self._order)
            for c in self._order:
                d._cols[c] = list(self._cols[c])
        d._index = list(range(len(self._index)))
        return d
    def rename(self, columns=None):
        if not columns:
            return self.copy()
        d = self.copy()
        d._order = [columns.get(c, c) for c in d._order]
        d._cols = {columns.get(c, c): v for c, v in d._cols.items()}
        return d
    def sort_values(self, by, ascending=True):
        by_list = by if isinstance(by, list) else [by]
        order = sorted(range(len(self._index)), key=lambda i: tuple(self._cols[c][i] for c in by_list), reverse=not ascending)
        return self._take(order)
    def isin(self, values):
        return self._apply_elementwise(lambda v: v in values)
    def equals(self, other):
        return isinstance(other, DataFrame) and self._order == other._order and \
            all(self._cols[c] == other._cols[c] for c in self._order)
    def select_dtypes(self, include=None, exclude=None):
        want_num = include is not None and any(
            (x is _np.number if _np is not None else False) or x in ("number", "float", "int", float, int)
            for x in (include if isinstance(include, list) else [include]))
        if want_num:
            return self._select_cols(self._numeric_cols())
        _ood("select_dtypes(include=%r)" % (include,)); return self
    def to_numpy(self): return self.values
    def groupby(self, by, as_index=True, sort=True):
        return _GroupBy(self, by, sort)
    def merge(self, right, how="inner", on=None, left_on=None, right_on=None, suffixes=("_x", "_y")):
        return merge(self, right, how=how, on=on, left_on=left_on, right_on=right_on, suffixes=suffixes)
    def apply(self, fn, axis=0):
        if axis in (1, "columns"):
            return Series([fn(self._row_series(i)) for i in range(len(self._index))], list(self._index))
        return Series([fn(Series(self._cols[c])) for c in self._order], list(self._order))
    def to_dict(self, orient="dict"):
        if orient == "records":
            return [dict((c, self._cols[c][i]) for c in self._order) for i in range(len(self._index))]
        if orient == "list":
            return {c: list(self._cols[c]) for c in self._order}
        return {c: dict(zip(self._index, self._cols[c])) for c in self._order}
    def to_csv(self, path_or_buf=None, index=True, sep=",", header=True):
        buf = _io.StringIO()
        w = _csv.writer(buf, lineterminator="\n")
        hdr = (["", ] if index else []) + list(self._order) if header else None
        if header:
            w.writerow((["" ] if index else []) + [str(c) for c in self._order])
        for pos, idx in enumerate(self._index):
            row = ([idx] if index else []) + [self._cols[c][pos] for c in self._order]
            w.writerow(["" if isna(v) else v for v in row])
        text = buf.getvalue()
        if path_or_buf is None:
            return text
        if hasattr(path_or_buf, "write"):
            path_or_buf.write(text); return None
        with open(path_or_buf, "w") as f:
            f.write(text)
        return None
    __hash__ = None


class _GroupBy:
    def __init__(self, df, by, sort=True):
        self._df = df
        self._by = by if isinstance(by, list) else [by]
        self._sort = sort
        groups = {}
        for i in range(len(df._index)):
            key = tuple(df._cols[c][i] for c in self._by)
            groups.setdefault(key, []).append(i)
        self._keys = sorted(groups.keys()) if sort else list(groups.keys())
        self._groups = groups
    def _agg_series(self, col, fn):
        idx = [k[0] if len(self._by) == 1 else k for k in self._keys]
        data = [fn(Series([self._df._cols[col][i] for i in self._groups[k]])) for k in self._keys]
        return Series(data, idx, col)
    def _agg_df(self, fn, numeric_only=True):
        cols = self._df._numeric_cols() if numeric_only else [c for c in self._df._order if c not in self._by]
        cols = [c for c in cols if c not in self._by]
        d = DataFrame()
        d._index = [k[0] if len(self._by) == 1 else k for k in self._keys]
        d._order = cols
        for c in cols:
            d._cols[c] = [fn(Series([self._df._cols[c][i] for i in self._groups[k]])) for k in self._keys]
        return d
    def __getitem__(self, col):
        gb = _GroupBy.__new__(_GroupBy)
        gb._df = self._df; gb._by = self._by; gb._sort = self._sort
        gb._keys = self._keys; gb._groups = self._groups; gb._single = col
        return gb
    def mean(self):
        return self._agg_series(self._single, lambda s: s.mean()) if getattr(self, "_single", None) else self._agg_df(lambda s: s.mean())
    def sum(self):
        return self._agg_series(self._single, lambda s: s.sum()) if getattr(self, "_single", None) else self._agg_df(lambda s: s.sum())
    def count(self):
        return self._agg_series(self._single, lambda s: s.count()) if getattr(self, "_single", None) else self._agg_df(lambda s: s.count(), numeric_only=False)
    def size(self):
        idx = [k[0] if len(self._by) == 1 else k for k in self._keys]
        return Series([len(self._groups[k]) for k in self._keys], idx)
    def min(self):
        return self._agg_series(self._single, lambda s: s.min()) if getattr(self, "_single", None) else self._agg_df(lambda s: s.min())
    def max(self):
        return self._agg_series(self._single, lambda s: s.max()) if getattr(self, "_single", None) else self._agg_df(lambda s: s.max())
    def agg(self, fn):
        _ood("groupby.agg(%r)" % (fn,)); return self._agg_df(lambda s: s.mean())
    def __iter__(self):
        for k in self._keys:
            yield (k[0] if len(self._by) == 1 else k), self._df._take(self._groups[k])


def merge(left, right, how="inner", on=None, left_on=None, right_on=None, suffixes=("_x", "_y")):
    if on is not None:
        left_on = right_on = (on if isinstance(on, list) else [on])
    elif left_on is not None:
        left_on = left_on if isinstance(left_on, list) else [left_on]
        right_on = right_on if isinstance(right_on, list) else [right_on]
    else:
        common = [c for c in left._order if c in right._cols]
        left_on = right_on = common
    rindex = {}
    for i in range(len(right._index)):
        key = tuple(right._cols[c][i] for c in right_on)
        rindex.setdefault(key, []).append(i)
    out_rows = []
    rcols = [c for c in right._order if c not in right_on]
    overlap = set(left._order) & set(rcols)
    for li in range(len(left._index)):
        key = tuple(left._cols[c][li] for c in left_on)
        matches = rindex.get(key, [])
        if not matches and how in ("left", "outer"):
            matches = [None]
        for ri in matches:
            row = {}
            for c in left._order:
                row[c + suffixes[0] if c in overlap else c] = left._cols[c][li]
            for c in rcols:
                row[c + suffixes[1] if c in overlap else c] = (right._cols[c][ri] if ri is not None else _NA)
            out_rows.append(row)
    return DataFrame(out_rows)

def concat(objs, axis=0, ignore_index=False):
    objs = [o for o in objs if o is not None]
    if not objs:
        return DataFrame()
    if axis == 0:
        cols = []
        for o in objs:
            for c in o._order:
                if c not in cols: cols.append(c)
        d = DataFrame(); d._order = cols; d._cols = {c: [] for c in cols}
        idx = []
        for o in objs:
            n = len(o._index)
            for c in cols:
                d._cols[c].extend(o._cols.get(c, [_NA] * n))
            idx.extend(o._index)
        d._index = list(range(len(idx))) if ignore_index else idx
        return d
    _ood("concat(axis=1)"); return objs[0]


# --------------------------------------------------------------------------- I/O
class _PandasErrors(_types.ModuleType):
    pass
class EmptyDataError(ValueError):
    pass
class ParserError(ValueError):
    pass

def _read_text(src):
    if hasattr(src, "read"):
        return src.read()
    with open(src, "r") as f:
        return f.read()

def read_csv(filepath_or_buffer, sep=",", header="infer", names=None, index_col=None,
             dtype=None, usecols=None, nrows=None, skiprows=None, encoding=None, **kw):
    text = _read_text(filepath_or_buffer)
    if not text.strip():
        raise EmptyDataError("No columns to parse from file")
    rows = list(_csv.reader(_io.StringIO(text), delimiter=sep))
    if skiprows:
        rows = rows[skiprows:]
    if not rows:
        raise EmptyDataError("No columns to parse from file")
    if names is not None:
        columns = list(names); body = rows
    elif header is None:
        columns = list(range(len(rows[0]))); body = rows
    else:
        columns = rows[0]; body = rows[1:]
    if nrows is not None:
        body = body[:nrows]
    d = DataFrame()
    d._order = list(columns)
    for j, c in enumerate(columns):
        col = [(_infer_cell(r[j]) if j < len(r) else _NA) for r in body]
        d._cols[c] = _coerce_column(col)
    d._index = list(range(len(body)))
    if index_col is not None:
        ic = columns[index_col] if isinstance(index_col, int) else index_col
        d._index = list(d._cols[ic]); d._order = [c for c in d._order if c != ic]
        del d._cols[ic]
    if dtype is not None and isinstance(dtype, dict):
        for c, t in dtype.items():
            if c in d._cols:
                d._cols[c] = Series(d._cols[c]).astype(t)._data
    return d

def read_json(path_or_buf, lines=False, orient=None):
    text = _read_text(path_or_buf)
    if lines:
        records = [_json.loads(l) for l in text.splitlines() if l.strip()]
        return DataFrame(records)
    obj = _json.loads(text)
    if isinstance(obj, list):
        return DataFrame(obj)
    if isinstance(obj, dict):
        # columns -> values
        return DataFrame(obj)
    _ood("read_json orient=%r" % orient)
    return DataFrame()

def read_sql_query(sql, con, **kw):
    # con is typically a sqlite3 connection (RustPython ships sqlite3); run + build a frame.
    try:
        cur = con.cursor()
        cur.execute(sql)
        rows = cur.fetchall()
        cols = [d[0] for d in cur.description] if cur.description else []
        data = {c: [r[i] for r in rows] for i, c in enumerate(cols)}
        return DataFrame(data)
    except Exception as e:
        _ood("read_sql_query: %s" % e)
        return DataFrame()
read_sql = read_sql_query

def to_numeric(arg, errors="raise"):
    def conv(v):
        try:
            f = float(v)
            return int(f) if f == int(f) and isinstance(v, (int, str)) and "." not in str(v) else f
        except (ValueError, TypeError):
            if errors == "coerce":
                return _NA
            if errors == "ignore":
                return v
            raise ValueError("Unable to parse %r" % (v,))
    if isinstance(arg, Series):
        return Series([conv(v) for v in arg._data], list(arg._index), arg.name)
    if isinstance(arg, (list, tuple)):
        return [conv(v) for v in arg]
    return conv(arg)


class Timestamp:
    def __init__(self, value=None, year=None, month=None, day=None, hour=0, minute=0, second=0):
        import datetime as _dt
        if isinstance(value, Timestamp):
            self._dt = value._dt
        elif isinstance(value, _dt.datetime):
            self._dt = value
        elif isinstance(value, str):
            self._dt = _parse_dt(value)
        elif year is not None:
            self._dt = _dt.datetime(year, month or 1, day or 1, hour, minute, second)
        elif value is not None:
            self._dt = _parse_dt(str(value))
        else:
            self._dt = _dt.datetime(1970, 1, 1)
    @property
    def year(self): return self._dt.year
    @property
    def month(self): return self._dt.month
    @property
    def day(self): return self._dt.day
    @property
    def hour(self): return self._dt.hour
    def __eq__(self, o): return isinstance(o, Timestamp) and self._dt == o._dt
    def __lt__(self, o): return self._dt < o._dt
    def __le__(self, o): return self._dt <= o._dt
    def __gt__(self, o): return self._dt > o._dt
    def __ge__(self, o): return self._dt >= o._dt
    def __sub__(self, o): return self._dt - o._dt
    def __hash__(self): return hash(self._dt)
    def __repr__(self): return "Timestamp('%s')" % self._dt.isoformat(sep=" ")
    def isoformat(self): return self._dt.isoformat()

def _parse_dt(s):
    import datetime as _dt
    s = s.strip()
    for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d", "%Y/%m/%d", "%m/%d/%Y", "%d-%m-%Y"):
        try:
            return _dt.datetime.strptime(s, fmt)
        except ValueError:
            continue
    _ood("to_datetime: unparseable %r" % s)
    return _dt.datetime(1970, 1, 1)

def to_datetime(arg, errors="raise", format=None):
    def conv(v):
        if isinstance(v, Timestamp):
            return v
        try:
            return Timestamp(v)
        except Exception:
            if errors == "coerce":
                return _NA
            raise
    if isinstance(arg, Series):
        return Series([conv(v) for v in arg._data], list(arg._index), arg.name)
    if isinstance(arg, (list, tuple)):
        return Series([conv(v) for v in arg])
    return conv(arg)

def date_range(*a, **k):
    _ood("date_range")
    return Series([])


# --------------------------------------------------------------------------- submodule registration
__version__ = "2.2.0-shellsim"
_errors_mod = _types.ModuleType("pandas.errors")
_errors_mod.EmptyDataError = EmptyDataError
_errors_mod.ParserError = ParserError
errors = _errors_mod
_sys.modules["pandas.errors"] = _errors_mod

_testing_mod = _types.ModuleType("pandas.testing")
def _assert_frame_equal(a, b, **k):
    if not a.equals(b):
        raise AssertionError("DataFrames are not equal")
def _assert_series_equal(a, b, **k):
    if not a.equals(b):
        raise AssertionError("Series are not equal")
_testing_mod.assert_frame_equal = _assert_frame_equal
_testing_mod.assert_series_equal = _assert_series_equal
testing = _testing_mod
_sys.modules["pandas.testing"] = _testing_mod
