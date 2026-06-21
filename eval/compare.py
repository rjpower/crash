#!/usr/bin/env python3
"""Run shellsim (simulated) and ground-truth (real CPython) over all real-oracle tasks and
compare. Classifies each task as faithful-match / our-gap / gt-only / both-broken."""
import os, sys, json, subprocess

TBLITE = "/tmp/tblite"
SHELLSIM = "/home/power/code/crash/.worktrees/shell-simulation-framework/target/release/shellsim"
TASKS = [t.strip() for t in open("/tmp/real_solutions.txt") if t.strip()]

def sim_run(task):
    d = os.path.join(TBLITE, task)
    try:
        p = subprocess.run(["timeout", "35", SHELLSIM, "task", d],
                           capture_output=True, text=True, timeout=45)
        # stdout is the JSON result (may be preceded by stderr noise; parse last {...})
        out = p.stdout
        start = out.find("{")
        if start < 0:
            return {"reward": None, "status": "crash", "unsupported": []}
        return json.loads(out[start:])
    except subprocess.TimeoutExpired:
        return {"reward": None, "status": "timeout", "unsupported": []}
    except Exception as e:
        return {"reward": None, "status": f"err:{e}", "unsupported": []}

def main():
    sim = {}
    for i, t in enumerate(TASKS):
        r = sim_run(t)
        sim[t] = r
        print(f"[sim {i+1}/{len(TASKS)}] {t:46} {r.get('status'):8} reward={r.get('reward')}", flush=True)
    json.dump(sim, open("/tmp/sim_results.json", "w"), indent=2)

if __name__ == "__main__":
    main()
