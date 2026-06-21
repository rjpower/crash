# Embedded scientific libraries

shellsim ships pure-Python reimplementations of the libraries that the largest slice of
pure-compute tasks reach for: `numpy`, `pandas`, `scipy` (`.stats` + `.linalg`), a micro
`sklearn`, and `yaml`. They live in `src/python/sim_*.py` and are compiled into the binary via
`include_str!`.

## Design contract

1. **Dumb and slow is fine.** Everything is a flat Python `list` + a shape/index; no vectorization.
   A 50k-row loop that real numpy does in ~1s may take a minute here. Correctness over speed.
2. **Cover the *common* surface, not the whole API.** Array creation / indexing (int, slice,
   tuple, boolean-mask, fancy) / broadcasting / reductions / ufuncs / basic linalg for numpy;
   read_csv/Series/DataFrame/groupby/merge for pandas; the validatable distributions for scipy.
3. **Never be silently wrong.** Anything outside the modelled surface calls `_shellsim_ood(msg)`,
   which the Rust harness reads back and turns into a `low` trust verdict (with a `ood:...` gap).
   This is the load-bearing rule: a task is allowed to be *uncovered*, never *miscomputed*.
4. **Gated on installation.** A sim-lib is importable only if a package manager "installed" it
   (`pip`/`uv`), mirroring a real venv. Using one caps the verdict at `medium` (it is a
   reimplementation, however faithful) and adds a `simlib:<name>` gap.

## What each module covers

| module | covered | OOD / not covered |
|---|---|---|
| `numpy` | ndarray (n-d, C-order), dtypes, full indexing + assignment, broadcasting (incl. 0-size), reductions w/ `axis=int` and `axis=tuple`, ufuncs, `dot`/`@` (vec·vec, vec·mat, mat·vec, mat·mat), `outer`, `pad`, `concatenate`/`stack`, `clip` (scalar + array bounds), `percentile`/`quantile` (linear), `linalg.{solve,inv,det,norm,eigh}`, deterministic `random` (SplitMix64), `testing.assert_*` | fft, einsum, masked arrays, structured dtypes, most of `linalg` beyond the above, exact bit-compat RNG vs real numpy |
| `pandas` | `read_csv`/`read_json`, `Series`/`DataFrame`, indexing (`[]`/`iloc`/`loc`), arithmetic incl. reflected ops + `Series⊗ndarray`, `iterrows`, reductions, `fillna`/`dropna`/`astype`/`groupby`/`merge`/`concat`/`to_csv`, `str` accessor, `between`/`sort_index`/`sort_values` | pivot, multi-index, time-series resampling, most of the long tail |
| `scipy.stats` | `t`/`norm`/`lognorm` (cdf/sf/pdf/ppf, exact via `math.erf`), `ttest_ind`/`ttest_1samp`/`pearsonr`/`sem`/`zscore`/`describe` | `shapiro`/`levene`/`mannwhitneyu`/`kruskal`/… (raise — would need unvalidated approximations) |
| `scipy.linalg` | `inv`/`solve`/`det`/`norm`/`pinv` (delegate to the numpy linalg) | decompositions (`lu`, `qr`, `svd`, `cholesky`, `sqrtm`, …) |
| `sklearn` | `LinearRegression`/`Ridge` (closed-form), `metrics.{r2,mse,mae,accuracy,precision,recall,f1,confusion_matrix}`, `train_test_split` | `RandomForest`/`SVC`/`LogisticRegression`/… (record OOD, raise on `fit` — no validated reimplementation) |
| `yaml` | `safe_load`/`safe_dump` of block + flow scalars/maps/lists | anchors/aliases/multi-doc/custom tags (OOD) |

## Validation

- **Core-op bit-exactness.** `det`, `inv`, `outer`, `dot`, matrix/vector products, `lognorm.cdf`,
  `ttest_ind`, `percentile`, `np.std` (ddof) were diffed against real numpy/scipy (CPython 3.14
  venv) on fixed inputs and match to full printed precision.
- **End-to-end task faithfulness.** Five OpenThoughts-TBLite tasks whose oracle reward is 1.0 and
  which exercise this stack now reproduce reward = 1.0 in the sandbox:
  `convolutional-layers` (n-d arrays, pad, multi-axis max), `csv-json-jsonl-merger` (pandas I/O),
  `multi-labeller` (deterministic RNG, fancy indexing, `scope="module"` fixtures),
  `anomaly-detection-ranking` (pandas + numpy), and `bandit-delayed-feedback` (LinUCB:
  50k-row IPW-weighted training, `scipy.linalg.inv`, `scipy.stats.lognorm`, deterministic and
  bit-faithful to the oracle).

## Bugs these reimplementations cost us to find (regression-tested)

- broadcasting a size-1 dim against size-**0** must yield 0, not `max(1,0)`;
- module-level `sum`/`all`/`min`/`abs`/… shadow the builtins — internal code must use the
  captured `_b*` aliases or it recurses;
- a foreign sequence (a pandas `Series`) handed to a numpy op must coerce to its values, not be
  treated as an opaque scalar (this silently corrupted `np.clip(series, …)`);
- `Series ⊗ ndarray` must align element-wise, not treat the array as a scalar.

These are covered by `cargo test --features python` (`sim_integration_tests`).
