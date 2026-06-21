#!/usr/bin/env python3
"""Ground-truth runner: run a TBLite task's oracle + verifier under REAL CPython.

Container-absolute roots (/workdir, /app, /tests, /logs, ...) are rewritten to a writable
prefix so we don't need root. This gives the *true* reward the oracle achieves on real
CPython, which we compare against shellsim's simulated reward to classify each task as
faithful / our-gap / broken-oracle.
"""
import os, re, sys, shutil, subprocess, json

VENV_PY = "/tmp/gtvenv/bin/python"
ROOTS = ["/workspace", "/workdir", "/solution", "/tests", "/logs", "/work", "/app"]
SKIP_RUN = re.compile(r"\b(apt|apt-get|pip|pip3|uv|uvx|conda|add-apt-repository|"
                      r"update-alternatives|wget|curl|git|npm|cargo|make|gcc|mvn|gradle|su)\b")

def rewrite(text, prefix):
    for r in ROOTS:
        text = re.sub(r"(?<![\w/])" + re.escape(r) + r"(?![\w])", prefix + r, text)
    return text

def run_task(task_dir, prefix):
    if os.path.exists(prefix):
        shutil.rmtree(prefix)
    os.makedirs(prefix)
    os.makedirs(prefix + "/logs/verifier", exist_ok=True)

    env_dir = os.path.join(task_dir, "environment")
    workdir = prefix + "/app"

    # --- interpret Dockerfile ---
    dockerfile = os.path.join(env_dir, "Dockerfile")
    if os.path.exists(dockerfile):
        lines = []
        cur = ""
        for ln in open(dockerfile, errors="replace"):
            ln = ln.rstrip("\n")
            if ln.rstrip().endswith("\\"):
                cur += ln.rstrip()[:-1] + " "
            else:
                cur += ln
                lines.append(cur); cur = ""
        if cur: lines.append(cur)
        for line in lines:
            s = line.strip()
            if not s or s.startswith("#"): continue
            parts = s.split(None, 1)
            instr = parts[0].upper()
            rest = parts[1].strip() if len(parts) > 1 else ""
            if instr == "WORKDIR":
                workdir = prefix + rewrite_root(rest, prefix)
                os.makedirs(workdir, exist_ok=True)
            elif instr in ("COPY", "ADD"):
                toks = [t for t in rest.split() if not t.startswith("--")]
                if len(toks) >= 2:
                    dst = prefix + rewrite_root(toks[-1], prefix)
                    for src in toks[:-1]:
                        hp = os.path.join(env_dir, src.lstrip("./"))
                        if os.path.isdir(hp):
                            os.makedirs(dst, exist_ok=True)
                            for item in os.listdir(hp):
                                s2 = os.path.join(hp, item)
                                d2 = os.path.join(dst, item)
                                if os.path.isdir(s2): shutil.copytree(s2, d2, dirs_exist_ok=True)
                                else: shutil.copy2(s2, d2)
                        elif os.path.isfile(hp):
                            os.makedirs(os.path.dirname(dst), exist_ok=True)
                            shutil.copy2(hp, dst)
            elif instr == "RUN":
                if SKIP_RUN.search(rest): continue
                cmd = rewrite(rest, prefix)
                subprocess.run(["bash", "-c", cmd], cwd=workdir, capture_output=True)

    # also copy any environment files not handled by COPY into workdir/environment-ish
    # (best-effort: if env has data/ and no COPY happened, mirror it)

    # --- place solution and tests ---
    sol_dst = workdir + "/solution"
    copy_tree(os.path.join(task_dir, "solution"), sol_dst)
    copy_tree(os.path.join(task_dir, "tests"), prefix + "/tests")

    # --- rewrite absolute paths in all scripts under prefix ---
    for root, _, files in os.walk(prefix):
        for f in files:
            if f.endswith((".sh", ".py")):
                p = os.path.join(root, f)
                try:
                    t = open(p, errors="replace").read()
                    open(p, "w").write(rewrite(t, prefix))
                except Exception:
                    pass

    # --- run oracle ---
    solve = open(os.path.join(task_dir, "solution/solve.sh"), errors="replace").read()
    solve = rewrite(solve, prefix)
    env = dict(os.environ)
    env["PATH"] = "/tmp/gtvenv/bin:" + env.get("PATH", "")
    env["PYTHONPATH"] = f"{prefix}/tests:{workdir}:{prefix}/app"
    subprocess.run(["bash", "-c", solve], cwd=workdir, env=env, capture_output=True, timeout=120)

    # --- run verifier ---
    test_py = prefix + "/tests/test_outputs.py"
    rc = None
    if os.path.exists(test_py):
        p = subprocess.run([VENV_PY, "-m", "pytest", test_py, "-rA", "-q"],
                           cwd=workdir, env=env, capture_output=True, timeout=180)
        rc = p.returncode

    # --- read reward ---
    rf = prefix + "/logs/verifier/reward.txt"
    if os.path.exists(rf):
        try:
            return float(open(rf).read().strip())
        except Exception:
            pass
    # many test.sh write reward from the pytest exit code via `if [ $? -eq 0 ]`; mirror that
    if rc is not None:
        return 1.0 if rc == 0 else 0.0
    return None

def rewrite_root(path, prefix):
    # return path with leading root preserved (no prefix) for joining
    return path.strip().strip('"')

def copy_tree(src, dst):
    if not os.path.isdir(src): return
    os.makedirs(dst, exist_ok=True)
    for item in os.listdir(src):
        if item in ("__pycache__",) or item.endswith(".pyc"): continue
        s = os.path.join(src, item); d = os.path.join(dst, item)
        if os.path.isdir(s): shutil.copytree(s, d, dirs_exist_ok=True)
        else: shutil.copy2(s, d)

if __name__ == "__main__":
    tasks = sys.argv[1:]
    results = {}
    for t in tasks:
        name = os.path.basename(t.rstrip("/"))
        try:
            r = run_task(t, f"/tmp/gt/{name}")
        except subprocess.TimeoutExpired:
            r = "timeout"
        except Exception as e:
            r = f"error:{e}"
        results[name] = r
        print(f"{name:48} ground_truth_reward={r}", flush=True)
    json.dump(results, open("/tmp/gt_results.json", "w"), indent=2)
