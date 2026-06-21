#!/usr/bin/env python3
"""Extract the third-party Python API surface actually used by a corpus of solutions.

This walks every ``*.py`` under one or more roots, parses it with ``ast``, resolves
import aliases, and records:

  * every attribute chain rooted at a tracked library  (``np.random.randn`` ->
    ``numpy.random.randn``), and
  * method/attribute accesses on values produced by a known constructor
    (``pd.read_csv(...).groupby(...)`` -> ``DataFrame.groupby``), via a light,
    flow-insensitive "flavor" inference good enough to rank the surface.

The point is to drive what we implement in the sandbox's mini numpy/pandas/scipy/
sklearn: build to the *measured* surface, and know what we are choosing not to cover.

Usage:
    extract_api.py <root> [<root> ...] [--json OUT] [--libs numpy,pandas,...]

Each root is scanned recursively; a root may be a task corpus (dirs with
solution/ + tests/) or any tree of .py files.  Datasets shipped as JSONL with a
solution column can be exploded to .py first (see dump_dataset.py).
"""
import argparse
import ast
import json
import os
import sys
from collections import Counter, defaultdict

# Libraries whose surface we care about. Top-level module name -> tracked.
DEFAULT_LIBS = [
    "numpy", "pandas", "scipy", "sklearn", "matplotlib", "torch",
    "yaml", "requests", "scipy.stats",
]

# Constructors that yield a value of a given "flavor", so we can attribute the
# methods called on the result.  Keyed by the fully-qualified callable name.
FLAVOR_CONSTRUCTORS = {
    "pandas.read_csv": "DataFrame",
    "pandas.read_json": "DataFrame",
    "pandas.read_sql": "DataFrame",
    "pandas.read_sql_query": "DataFrame",
    "pandas.read_excel": "DataFrame",
    "pandas.read_parquet": "DataFrame",
    "pandas.DataFrame": "DataFrame",
    "pandas.Series": "Series",
    "pandas.concat": "DataFrame",
    "pandas.merge": "DataFrame",
    "pandas.to_datetime": "Series",
    "numpy.array": "ndarray",
    "numpy.zeros": "ndarray",
    "numpy.ones": "ndarray",
    "numpy.arange": "ndarray",
    "numpy.asarray": "ndarray",
    "numpy.linspace": "ndarray",
    "numpy.full": "ndarray",
    "numpy.random.randn": "ndarray",
    "numpy.random.rand": "ndarray",
    "numpy.random.normal": "ndarray",
    "numpy.random.uniform": "ndarray",
    "numpy.empty": "ndarray",
}


class Visitor(ast.NodeVisitor):
    def __init__(self, libs):
        self.libs = set(libs)
        self.top_libs = {l.split(".")[0] for l in libs}
        # alias name -> fully-qualified module/symbol it refers to
        self.aliases = {}
        # local var name -> flavor ("DataFrame"/"ndarray"/...)
        self.flavors = {}
        self.attr_hits = Counter()      # "numpy.random.randn" -> n
        self.flavor_hits = Counter()    # "DataFrame.groupby" -> n
        self.imports = Counter()        # "numpy" / "sklearn.linear_model.LinearRegression"

    # ---- imports ----
    def visit_Import(self, node):
        for a in node.names:
            top = a.name.split(".")[0]
            if top in self.top_libs:
                self.aliases[a.asname or a.name] = a.name
                self.imports[a.name] += 1
        self.generic_visit(node)

    def visit_ImportFrom(self, node):
        mod = node.module or ""
        top = mod.split(".")[0]
        if top in self.top_libs:
            for a in node.names:
                fq = f"{mod}.{a.name}"
                self.aliases[a.asname or a.name] = fq
                self.imports[fq] += 1
        self.generic_visit(node)

    # ---- resolve an attribute chain to a fully-qualified name, if rooted at a lib ----
    def _resolve(self, node):
        parts = []
        cur = node
        while isinstance(cur, ast.Attribute):
            parts.append(cur.attr)
            cur = cur.value
        if isinstance(cur, ast.Name):
            base = self.aliases.get(cur.id)
            if base is not None:
                parts.append(base)
                return ".".join(reversed(parts))
        return None

    def _name_flavor(self, node):
        """Flavor of an expression node, if known."""
        if isinstance(node, ast.Name):
            return self.flavors.get(node.id)
        # chained call/attr that produces same flavor (df.dropna().groupby) -> best effort
        if isinstance(node, ast.Call):
            return self._call_flavor(node)
        if isinstance(node, ast.Subscript):
            return self._name_flavor(node.value)  # df[...] keeps frame-ish flavor
        if isinstance(node, ast.Attribute):
            # df.loc / df.iloc / df.T etc keep flavor
            fl = self._name_flavor(node.value)
            return fl
        return None

    def _call_flavor(self, node):
        fq = self._resolve(node.func)
        if fq and fq in FLAVOR_CONSTRUCTORS:
            return FLAVOR_CONSTRUCTORS[fq]
        # method call on a flavored object usually returns the same flavor
        if isinstance(node.func, ast.Attribute):
            return self._name_flavor(node.func.value)
        return None

    def visit_Assign(self, node):
        fl = self._name_flavor(node.value)
        if fl:
            for t in node.targets:
                if isinstance(t, ast.Name):
                    self.flavors[t.id] = fl
        self.generic_visit(node)

    def visit_Attribute(self, node):
        fq = self._resolve(node)
        if fq:
            self.attr_hits[fq] += 1
        else:
            fl = self._name_flavor(node.value)
            if fl:
                self.flavor_hits[f"{fl}.{node.attr}"] += 1
        self.generic_visit(node)

    def visit_Call(self, node):
        # record flavored method calls explicitly (visit_Attribute also catches the attr)
        self.generic_visit(node)


def scan_file(path, libs):
    try:
        src = open(path, "r", encoding="utf-8", errors="replace").read()
        tree = ast.parse(src)
    except (SyntaxError, ValueError):
        return None
    v = Visitor(libs)
    v.visit(tree)
    return v


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("roots", nargs="+")
    ap.add_argument("--json", default=None)
    ap.add_argument("--libs", default=",".join(DEFAULT_LIBS))
    ap.add_argument("--top", type=int, default=60)
    args = ap.parse_args()
    libs = [l for l in args.libs.split(",") if l]

    attr = Counter()
    flavor = Counter()
    imports = Counter()
    # which top-level task dir each symbol appeared in (for "n tasks" counts)
    attr_tasks = defaultdict(set)
    flavor_tasks = defaultdict(set)
    import_tasks = defaultdict(set)
    nfiles = 0

    for root in args.roots:
        for dirpath, _dirs, files in os.walk(root):
            for fn in files:
                if not fn.endswith(".py"):
                    continue
                p = os.path.join(dirpath, fn)
                rel = os.path.relpath(p, root)
                task = rel.split(os.sep)[0]
                v = scan_file(p, libs)
                if v is None:
                    continue
                nfiles += 1
                for k, n in v.attr_hits.items():
                    attr[k] += n
                    attr_tasks[k].add(task)
                for k, n in v.flavor_hits.items():
                    flavor[k] += n
                    flavor_tasks[k].add(task)
                for k, n in v.imports.items():
                    imports[k] += n
                    import_tasks[k].add(task)

    def fmt(counter, taskmap, title):
        print(f"\n==== {title} ====")
        for k, n in counter.most_common(args.top):
            print(f"  {n:4}  {len(taskmap[k]):3}t  {k}")

    print(f"scanned {nfiles} python files under {args.roots}")
    fmt(imports, import_tasks, "imports (symbol -> uses, tasks)")
    fmt(attr, attr_tasks, "library attribute/function surface")
    fmt(flavor, flavor_tasks, "object method surface (DataFrame/Series/ndarray)")

    if args.json:
        out = {
            "nfiles": nfiles,
            "imports": {k: [n, sorted(import_tasks[k])] for k, n in imports.items()},
            "attr": {k: [n, sorted(attr_tasks[k])] for k, n in attr.items()},
            "flavor": {k: [n, sorted(flavor_tasks[k])] for k, n in flavor.items()},
        }
        json.dump(out, open(args.json, "w"), indent=2)
        print(f"\nwrote {args.json}")


if __name__ == "__main__":
    main()
