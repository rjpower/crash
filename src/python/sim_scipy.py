"""shellsim's tiny ``scipy`` — just enough ``scipy.stats`` to cover the measured surface.

We implement only the pieces whose formulas are exact and validatable against real scipy: the
t-distribution (via the regularized incomplete beta function) backing ``ttest_ind`` and
``t.interval``, plus simple descriptive helpers. Genuinely hard / approximate tests
(``shapiro``, ``levene``, ``mannwhitneyu``) call ``_shellsim_ood(...)`` and raise, so we never
emit a p-value we can't stand behind.
"""
import sys as _sys
import types as _types
import math as _math

def _ood(msg):
    try:
        _shellsim_ood("scipy: " + msg)
    except Exception:
        pass

def _vals(x):
    if hasattr(x, "_data"):
        return [float(v) for v in x._data]
    return [float(v) for v in x]


# ---- regularized incomplete beta (Numerical Recipes betai) ----
def _betacf(a, b, x):
    MAXIT, EPS, FPMIN = 200, 3e-12, 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c = 1.0
    d = 1.0 - qab * x / qap
    if abs(d) < FPMIN:
        d = FPMIN
    d = 1.0 / d
    h = d
    for m in range(1, MAXIT + 1):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        if abs(d) < FPMIN: d = FPMIN
        c = 1.0 + aa / c
        if abs(c) < FPMIN: c = FPMIN
        d = 1.0 / d
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        if abs(d) < FPMIN: d = FPMIN
        c = 1.0 + aa / c
        if abs(c) < FPMIN: c = FPMIN
        d = 1.0 / d
        de = d * c
        h *= de
        if abs(de - 1.0) < EPS:
            break
    return h

def _betai(a, b, x):
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    lbeta = _math.lgamma(a + b) - _math.lgamma(a) - _math.lgamma(b)
    bt = _math.exp(lbeta + a * _math.log(x) + b * _math.log(1.0 - x))
    if x < (a + 1.0) / (a + b + 2.0):
        return bt * _betacf(a, b, x) / a
    return 1.0 - bt * _betacf(b, a, 1.0 - x) / b


class _StudentT:
    """Two-sided / one-sided helpers for Student's t with `df` degrees of freedom."""
    def cdf(self, t, df):
        x = df / (df + t * t)
        ib = 0.5 * _betai(df / 2.0, 0.5, x)
        return 1.0 - ib if t > 0 else ib
    def sf(self, t, df):
        return 1.0 - self.cdf(t, df)
    def ppf(self, p, df):
        # bisection on the monotone cdf
        lo, hi = -1e4, 1e4
        for _ in range(200):
            mid = (lo + hi) / 2.0
            if self.cdf(mid, df) < p:
                lo = mid
            else:
                hi = mid
        return (lo + hi) / 2.0
    def interval(self, confidence, df, loc=0.0, scale=1.0):
        alpha = 1.0 - confidence
        q = self.ppf(1.0 - alpha / 2.0, df)
        return (loc - q * scale, loc + q * scale)

t = _StudentT()


class _Norm:
    def cdf(self, x, loc=0.0, scale=1.0):
        return 0.5 * (1.0 + _math.erf((x - loc) / (scale * _math.sqrt(2.0))))
    def sf(self, x, loc=0.0, scale=1.0):
        return 1.0 - self.cdf(x, loc, scale)
    def pdf(self, x, loc=0.0, scale=1.0):
        z = (x - loc) / scale
        return _math.exp(-0.5 * z * z) / (scale * _math.sqrt(2 * _math.pi))
    def ppf(self, p, loc=0.0, scale=1.0):
        # Acklam's rational approximation
        a = [-3.969683028665376e+01, 2.209460984245205e+02, -2.759285104469687e+02,
             1.383577518672690e+02, -3.066479806614716e+01, 2.506628277459239e+00]
        b = [-5.447609879822406e+01, 1.615858368580409e+02, -1.556989798598866e+02,
             6.680131188771972e+01, -1.328068155288572e+01]
        c = [-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00,
             -2.549732539343734e+00, 4.374664141464968e+00, 2.938163982698783e+00]
        d = [7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00,
             3.754408661907416e+00]
        pl = 0.02425
        if p < pl:
            q = _math.sqrt(-2 * _math.log(p))
            z = (((((c[0]*q+c[1])*q+c[2])*q+c[3])*q+c[4])*q+c[5]) / ((((d[0]*q+d[1])*q+d[2])*q+d[3])*q+1)
        elif p <= 1 - pl:
            q = p - 0.5; r = q * q
            z = (((((a[0]*r+a[1])*r+a[2])*r+a[3])*r+a[4])*r+a[5])*q / (((((b[0]*r+b[1])*r+b[2])*r+b[3])*r+b[4])*r+1)
        else:
            q = _math.sqrt(-2 * _math.log(1 - p))
            z = -(((((c[0]*q+c[1])*q+c[2])*q+c[3])*q+c[4])*q+c[5]) / ((((d[0]*q+d[1])*q+d[2])*q+d[3])*q+1)
        return loc + scale * z

norm = _Norm()


def _apply_scalar(x, fn):
    """Map a scalar function over a scalar / list / sim-ndarray, returning the same flavor."""
    if hasattr(x, "_data") and hasattr(x, "_shape"):  # sim numpy ndarray
        import numpy as _np
        return _np.array([fn(v) for v in x._data]).reshape(*x._shape) if x._shape else fn(x._data[0])
    if isinstance(x, (list, tuple)):
        return [fn(v) for v in x]
    return fn(x)


class _LogNorm:
    """Lognormal distribution. cdf/pdf/sf expressed exactly via the standard normal (erf-based)
    so they reproduce scipy: X = scale * exp(s * Z) + loc, Z ~ N(0,1)."""
    def cdf(self, x, s, loc=0.0, scale=1.0):
        def one(v):
            v = v - loc
            if v <= 0:
                return 0.0
            return norm.cdf(_math.log(v / scale) / s)
        return _apply_scalar(x, one)
    def sf(self, x, s, loc=0.0, scale=1.0):
        return _apply_scalar(x, lambda v: 1.0 - self.cdf(v, s, loc, scale))
    def pdf(self, x, s, loc=0.0, scale=1.0):
        def one(v):
            v = v - loc
            if v <= 0:
                return 0.0
            z = _math.log(v / scale) / s
            return _math.exp(-0.5 * z * z) / (v * s * _math.sqrt(2 * _math.pi))
        return _apply_scalar(x, one)
    def ppf(self, q, s, loc=0.0, scale=1.0):
        return _apply_scalar(q, lambda p: loc + scale * _math.exp(s * norm.ppf(p)))

lognorm = _LogNorm()


class _Result(tuple):
    def __new__(cls, a, b, names=("statistic", "pvalue")):
        self = super().__new__(cls, (a, b))
        self._names = names
        return self
    @property
    def statistic(self): return self[0]
    @property
    def pvalue(self): return self[1]
    @property
    def correlation(self): return self[0]


def _mean(xs): return sum(xs) / len(xs)
def _var(xs, ddof=1):
    m = _mean(xs); n = len(xs)
    return sum((x - m) ** 2 for x in xs) / (n - ddof)

def ttest_ind(a, b, equal_var=True):
    a, b = _vals(a), _vals(b)
    na, nb = len(a), len(b)
    ma, mb = _mean(a), _mean(b)
    va, vb = _var(a), _var(b)
    if equal_var:
        sp2 = ((na - 1) * va + (nb - 1) * vb) / (na + nb - 2)
        se = _math.sqrt(sp2 * (1.0 / na + 1.0 / nb))
        df = na + nb - 2
    else:
        se = _math.sqrt(va / na + vb / nb)
        df = (va / na + vb / nb) ** 2 / ((va / na) ** 2 / (na - 1) + (vb / nb) ** 2 / (nb - 1))
    tstat = (ma - mb) / se
    p = 2.0 * t.sf(abs(tstat), df)
    return _Result(tstat, p)

def ttest_1samp(a, popmean):
    a = _vals(a); n = len(a)
    m = _mean(a); se = _math.sqrt(_var(a) / n)
    tstat = (m - popmean) / se
    return _Result(tstat, 2.0 * t.sf(abs(tstat), n - 1))

def pearsonr(x, y):
    x, y = _vals(x), _vals(y)
    n = len(x); mx, my = _mean(x), _mean(y)
    cov = sum((xi - mx) * (yi - my) for xi, yi in zip(x, y))
    sx = _math.sqrt(sum((xi - mx) ** 2 for xi in x))
    sy = _math.sqrt(sum((yi - my) ** 2 for yi in y))
    r = cov / (sx * sy) if sx and sy else 0.0
    if n > 2 and abs(r) < 1.0:
        tstat = r * _math.sqrt((n - 2) / (1 - r * r))
        p = 2.0 * t.sf(abs(tstat), n - 2)
    else:
        p = 0.0
    return _Result(r, p)

def sem(a, ddof=1):
    a = _vals(a)
    return _math.sqrt(_var(a, ddof) / len(a))

def zscore(a, ddof=0):
    vals = _vals(a)
    m = _mean(vals); s = _math.sqrt(_var(vals, ddof))
    out = [(v - m) / s for v in vals]
    try:
        import numpy as _np
        return _np.array(out)
    except Exception:
        return out

def describe(a):
    a = _vals(a)
    return _types.SimpleNamespace(nobs=len(a), minmax=(min(a), max(a)), mean=_mean(a), variance=_var(a))

def _unsupported(name):
    def f(*a, **k):
        _ood("stats.%s not implemented (would need an unvalidated approximation)" % name)
        raise NotImplementedError("scipy.stats.%s is not implemented in shellsim" % name)
    return f

shapiro = _unsupported("shapiro")
levene = _unsupported("levene")
mannwhitneyu = _unsupported("mannwhitneyu")
kruskal = _unsupported("kruskal")
chi2_contingency = _unsupported("chi2_contingency")
f_oneway = _unsupported("f_oneway")
anderson = _unsupported("anderson")
ks_2samp = _unsupported("ks_2samp")

# ---- assemble scipy.stats submodule ----
_stats = _types.ModuleType("scipy.stats")
for _n in ("t", "norm", "lognorm", "ttest_ind", "ttest_1samp", "pearsonr", "sem", "zscore",
           "describe", "shapiro", "levene", "mannwhitneyu", "kruskal", "chi2_contingency",
           "f_oneway", "anderson", "ks_2samp"):
    setattr(_stats, _n, globals()[_n])
stats = _stats
_sys.modules["scipy.stats"] = _stats

# ---- scipy.linalg: delegate the dense routines to our numpy linalg (same math) ----
def _np_linalg():
    import numpy as _np
    return _np.linalg

_linalg = _types.ModuleType("scipy.linalg")
_linalg.inv = lambda a: _np_linalg().inv(a)
_linalg.solve = lambda a, b, **k: _np_linalg().solve(a, b)
_linalg.det = lambda a: _np_linalg().det(a)
_linalg.norm = lambda a, *p, **k: _np_linalg().norm(a, *p, **k)
def _linalg_pinv(a, *p, **k):
    _ln = _np_linalg()
    if hasattr(_ln, "pinv"):
        return _ln.pinv(a)
    _ood("linalg.pinv not implemented")
    raise NotImplementedError("scipy.linalg.pinv is not implemented in shellsim")
_linalg.pinv = _linalg_pinv
linalg = _linalg
_sys.modules["scipy.linalg"] = _linalg

__version__ = "1.13.0-shellsim"
