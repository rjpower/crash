"""shellsim's micro ``scikit-learn``.

Only the *deterministic, closed-form* estimators are real here — ``LinearRegression`` and
``Ridge`` (normal equations, matching sklearn's OLS / centered-ridge to float tolerance) — plus
the elementwise metrics. The stochastic / iterative models (``RandomForest*``, ``SVC``,
``LogisticRegression``, …) cannot be reproduced bit-for-bit, so they call ``_shellsim_ood(...)``
and raise rather than emit predictions we can't validate.
"""
import sys as _sys
import types as _types
import math as _math

try:
    import numpy as _np
except Exception:
    _np = None

def _ood(msg):
    try:
        _shellsim_ood("sklearn: " + msg)
    except Exception:
        pass

def _to_matrix(X):
    if _np is not None and isinstance(X, _np.ndarray):
        if X.ndim == 1:
            return [[float(v)] for v in X._data]
        return [[float(X[i, j]) for j in range(X._shape[1])] for i in range(X._shape[0])]
    if hasattr(X, "values"):  # DataFrame
        X = X.values
        return _to_matrix(X)
    return [[float(v) for v in row] for row in X]

def _to_vector(y):
    if _np is not None and isinstance(y, _np.ndarray):
        return [float(v) for v in y._data]
    if hasattr(y, "_data"):
        return [float(v) for v in y._data]
    return [float(v) for v in y]

def _matmemb(a):
    return _np.array(a) if _np is not None else a


# ---- linear algebra via numpy.linalg ----
def _solve_normal(Xm, yv, alpha=0.0, center=False):
    n = len(Xm)
    k = len(Xm[0]) if n else 0
    if center:
        xmean = [sum(Xm[i][j] for i in range(n)) / n for j in range(k)]
        ymean = sum(yv) / n
        Xc = [[Xm[i][j] - xmean[j] for j in range(k)] for i in range(n)]
        yc = [yv[i] - ymean for i in range(n)]
    else:
        Xc, yc, xmean, ymean = Xm, yv, [0.0] * k, 0.0
    # A = Xc^T Xc (+ alpha I);  b = Xc^T yc
    A = [[sum(Xc[r][i] * Xc[r][j] for r in range(n)) for j in range(k)] for i in range(k)]
    for i in range(k):
        A[i][i] += alpha
    b = [sum(Xc[r][i] * yc[r] for r in range(n)) for i in range(k)]
    w = _gauss_solve(A, b)
    if center:
        intercept = ymean - sum(xmean[j] * w[j] for j in range(k))
    else:
        intercept = 0.0
    return w, intercept

def _gauss_solve(A, b):
    n = len(A)
    M = [list(map(float, A[i])) + [float(b[i])] for i in range(n)]
    for col in range(n):
        piv = max(range(col, n), key=lambda r: abs(M[r][col]))
        if abs(M[piv][col]) < 1e-15:
            _ood("LinearRegression: singular design matrix")
            M[piv][col] = 1e-15
        M[col], M[piv] = M[piv], M[col]
        d = M[col][col]
        M[col] = [v / d for v in M[col]]
        for r in range(n):
            if r != col and M[r][col]:
                f = M[r][col]
                M[r] = [M[r][t] - f * M[col][t] for t in range(n + 1)]
    return [M[i][n] for i in range(n)]


class LinearRegression:
    def __init__(self, fit_intercept=True, **kw):
        self.fit_intercept = fit_intercept
        self.coef_ = None
        self.intercept_ = 0.0
    def fit(self, X, y):
        Xm, yv = _to_matrix(X), _to_vector(y)
        if self.fit_intercept:
            Xa = [[1.0] + row for row in Xm]
            w, _ = _solve_normal(Xa, yv, 0.0, center=False)
            self.intercept_ = w[0]
            self.coef_ = _matmemb(w[1:])
        else:
            w, _ = _solve_normal(Xm, yv, 0.0, center=False)
            self.intercept_ = 0.0
            self.coef_ = _matmemb(w)
        return self
    def predict(self, X):
        Xm = _to_matrix(X)
        coef = self.coef_._data if (_np is not None and isinstance(self.coef_, _np.ndarray)) else self.coef_
        out = [self.intercept_ + sum(c * v for c, v in zip(coef, row)) for row in Xm]
        return _matmemb(out)
    def score(self, X, y):
        return r2_score(_to_vector(y), self.predict(X))


class Ridge:
    def __init__(self, alpha=1.0, fit_intercept=True, **kw):
        self.alpha = alpha
        self.fit_intercept = fit_intercept
        self.coef_ = None
        self.intercept_ = 0.0
    def fit(self, X, y):
        Xm, yv = _to_matrix(X), _to_vector(y)
        w, intercept = _solve_normal(Xm, yv, self.alpha, center=self.fit_intercept)
        self.coef_ = _matmemb(w)
        self.intercept_ = intercept if self.fit_intercept else 0.0
        return self
    def predict(self, X):
        Xm = _to_matrix(X)
        coef = self.coef_._data if (_np is not None and isinstance(self.coef_, _np.ndarray)) else self.coef_
        out = [self.intercept_ + sum(c * v for c, v in zip(coef, row)) for row in Xm]
        return _matmemb(out)
    def score(self, X, y):
        return r2_score(_to_vector(y), self.predict(X))


# ---- metrics (exact elementwise formulas) ----
def r2_score(y_true, y_pred):
    yt, yp = _to_vector(y_true), _to_vector(y_pred)
    m = sum(yt) / len(yt)
    ss_res = sum((a - b) ** 2 for a, b in zip(yt, yp))
    ss_tot = sum((a - m) ** 2 for a in yt)
    return 1.0 - ss_res / ss_tot if ss_tot else 0.0

def mean_squared_error(y_true, y_pred, squared=True):
    yt, yp = _to_vector(y_true), _to_vector(y_pred)
    mse = sum((a - b) ** 2 for a, b in zip(yt, yp)) / len(yt)
    return mse if squared else _math.sqrt(mse)

def root_mean_squared_error(y_true, y_pred):
    return mean_squared_error(y_true, y_pred, squared=False)

def mean_absolute_error(y_true, y_pred):
    yt, yp = _to_vector(y_true), _to_vector(y_pred)
    return sum(abs(a - b) for a, b in zip(yt, yp)) / len(yt)

def _labels(y):
    if _np is not None and isinstance(y, _np.ndarray):
        return list(y._data)
    if hasattr(y, "_data"):
        return list(y._data)
    return list(y)

def accuracy_score(y_true, y_pred, normalize=True):
    yt, yp = _labels(y_true), _labels(y_pred)
    correct = sum(1 for a, b in zip(yt, yp) if a == b)
    return correct / len(yt) if normalize else correct

def _prf(y_true, y_pred, pos_label=1, average="binary"):
    yt, yp = _labels(y_true), _labels(y_pred)
    classes = sorted(set(yt) | set(yp))
    def one(c):
        tp = sum(1 for a, b in zip(yt, yp) if b == c and a == c)
        fp = sum(1 for a, b in zip(yt, yp) if b == c and a != c)
        fn = sum(1 for a, b in zip(yt, yp) if b != c and a == c)
        prec = tp / (tp + fp) if (tp + fp) else 0.0
        rec = tp / (tp + fn) if (tp + fn) else 0.0
        f1 = 2 * prec * rec / (prec + rec) if (prec + rec) else 0.0
        return prec, rec, f1, (tp + fn)
    if average == "binary":
        return one(pos_label)
    stats = [one(c) for c in classes]
    if average == "macro":
        n = len(stats)
        return (sum(s[0] for s in stats) / n, sum(s[1] for s in stats) / n, sum(s[2] for s in stats) / n, None)
    if average == "weighted":
        tot = sum(s[3] for s in stats) or 1
        return (sum(s[0] * s[3] for s in stats) / tot, sum(s[1] * s[3] for s in stats) / tot,
                sum(s[2] * s[3] for s in stats) / tot, None)
    _ood("average=%r" % average)
    return one(pos_label)

def precision_score(y_true, y_pred, pos_label=1, average="binary"):
    return _prf(y_true, y_pred, pos_label, average)[0]
def recall_score(y_true, y_pred, pos_label=1, average="binary"):
    return _prf(y_true, y_pred, pos_label, average)[1]
def f1_score(y_true, y_pred, pos_label=1, average="binary"):
    return _prf(y_true, y_pred, pos_label, average)[2]

def confusion_matrix(y_true, y_pred):
    yt, yp = _labels(y_true), _labels(y_pred)
    classes = sorted(set(yt) | set(yp))
    idx = {c: i for i, c in enumerate(classes)}
    m = [[0] * len(classes) for _ in classes]
    for a, b in zip(yt, yp):
        m[idx[a]][idx[b]] += 1
    return _matmemb(m)


def train_test_split(*arrays, test_size=0.25, train_size=None, random_state=None, shuffle=True, stratify=None):
    n = len(arrays[0])
    if isinstance(test_size, float):
        n_test = int(_math.floor(test_size * n))
    elif test_size is None:
        n_test = n - (int(train_size * n) if isinstance(train_size, float) else (train_size or 0))
    else:
        n_test = test_size
    idx = list(range(n))
    if shuffle:
        if _np is not None:
            rng = _np.random.RandomState(random_state if random_state is not None else 0)
            rng.shuffle(idx)
        else:
            import random as _r
            _r.Random(random_state).shuffle(idx)
    test_idx = idx[:n_test]
    train_idx = idx[n_test:]
    def take(arr, ix):
        if hasattr(arr, "_take"):  # DataFrame
            return arr._take(ix)
        if _np is not None and isinstance(arr, _np.ndarray):
            return arr[_np.array(ix)]
        if hasattr(arr, "_data"):  # Series
            return arr[[arr._index[i] for i in ix]] if hasattr(arr, "_index") else [arr._data[i] for i in ix]
        return [arr[i] for i in ix]
    out = []
    for arr in arrays:
        out.append(take(arr, train_idx))
        out.append(take(arr, test_idx))
    return out


# ---- estimators we deliberately do NOT fake ----
def _unvalidatable(name):
    class _Model:
        def __init__(self, *a, **k):
            _ood("%s is stochastic/iterative — not reproducible bit-for-bit" % name)
        def fit(self, *a, **k):
            raise NotImplementedError("sklearn %s is not implemented in shellsim" % name)
        def predict(self, *a, **k):
            raise NotImplementedError("sklearn %s is not implemented in shellsim" % name)
        def score(self, *a, **k):
            raise NotImplementedError("sklearn %s is not implemented in shellsim" % name)
    _Model.__name__ = name
    return _Model

RandomForestRegressor = _unvalidatable("RandomForestRegressor")
RandomForestClassifier = _unvalidatable("RandomForestClassifier")
GradientBoostingRegressor = _unvalidatable("GradientBoostingRegressor")
GradientBoostingClassifier = _unvalidatable("GradientBoostingClassifier")
LogisticRegression = _unvalidatable("LogisticRegression")
SVC = _unvalidatable("SVC")
SVR = _unvalidatable("SVR")
KMeans = _unvalidatable("KMeans")
DecisionTreeClassifier = _unvalidatable("DecisionTreeClassifier")
DecisionTreeRegressor = _unvalidatable("DecisionTreeRegressor")


# ---- assemble submodules ----
def _mod(name, **members):
    m = _types.ModuleType(name)
    for k, v in members.items():
        setattr(m, k, v)
    _sys.modules[name] = m
    return m

linear_model = _mod("sklearn.linear_model", LinearRegression=LinearRegression, Ridge=Ridge,
                    LogisticRegression=LogisticRegression)
ensemble = _mod("sklearn.ensemble", RandomForestRegressor=RandomForestRegressor,
               RandomForestClassifier=RandomForestClassifier,
               GradientBoostingRegressor=GradientBoostingRegressor,
               GradientBoostingClassifier=GradientBoostingClassifier)
svm = _mod("sklearn.svm", SVC=SVC, SVR=SVR)
tree = _mod("sklearn.tree", DecisionTreeClassifier=DecisionTreeClassifier,
           DecisionTreeRegressor=DecisionTreeRegressor)
cluster = _mod("sklearn.cluster", KMeans=KMeans)
metrics = _mod("sklearn.metrics", r2_score=r2_score, mean_squared_error=mean_squared_error,
              root_mean_squared_error=root_mean_squared_error, mean_absolute_error=mean_absolute_error,
              accuracy_score=accuracy_score, precision_score=precision_score, recall_score=recall_score,
              f1_score=f1_score, confusion_matrix=confusion_matrix)
model_selection = _mod("sklearn.model_selection", train_test_split=train_test_split)

__version__ = "1.6.0-shellsim"
