#!/usr/bin/env python3
"""Compare sim_results.json (shellsim) vs gt_results.json (real CPython) and classify."""
import json, collections

sim = json.load(open("/tmp/sim_results.json"))
gt = json.load(open("/tmp/gt_results.json"))

def isnum(x):
    return isinstance(x, (int, float))

rows = []
for task in sorted(sim.keys()):
    s = sim[task]
    sr = s.get("reward")
    gr = gt.get(task)
    grn = gr if isnum(gr) else None
    # classification
    if isnum(sr) and grn is not None:
        if abs(sr - grn) < 1e-6:
            cls = "FAITHFUL" if grn > 0 else "FAITHFUL(both~0)"
        elif sr < grn:
            cls = "OUR_GAP"          # sandbox underperforms real CPython
        else:
            cls = "SIM_HIGHER"       # sandbox > gt (gt harness missing deps, usually)
    elif isnum(sr) and grn is None:
        cls = "GT_UNAVAILABLE"        # gt couldn't run (needs real deps/exec); sim-only
    else:
        cls = "SIM_FAILED"
    rows.append((task, sr, gr, s.get("status"), cls, s.get("unsupported", [])))

by = collections.Counter(r[4] for r in rows)
print("================= SIM vs GROUND-TRUTH CLASSIFICATION =================")
for k in ["FAITHFUL", "FAITHFUL(both~0)", "OUR_GAP", "SIM_HIGHER", "GT_UNAVAILABLE", "SIM_FAILED"]:
    print(f"  {by.get(k,0):3}  {k}")
print(f"  --- total real-oracle tasks: {len(rows)}")

# faithful where real oracle achieves reward>0 (the meaningful "we correctly simulate" set)
faithful_pos = [r for r in rows if r[4] == "FAITHFUL"]
sim_pass = [r for r in rows if isnum(r[1]) and r[1] >= 0.999]
gt_pass = [r for r in rows if isnum(r[2]) and r[2] >= 0.999]
faithful_any = [r for r in rows if r[4].startswith("FAITHFUL")]
print(f"\n  sandbox reward>0 matches real CPython exactly (FAITHFUL>0): {len(faithful_pos)}")
print(f"  sandbox fully passes (reward=1):                          {len(sim_pass)}")
print(f"  ground-truth fully passes (reward=1):                     {len(gt_pass)}")
print(f"  total FAITHFUL (sim==gt, incl both-zero):                 {len(faithful_any)}")

# where sim and gt both numeric, correlation of rewards
pairs = [(r[1], r[2]) for r in rows if isnum(r[1]) and isnum(r[2])]
if pairs:
    exact = sum(1 for a, b in pairs if abs(a-b) < 1e-6)
    print(f"\n  tasks with BOTH sim+gt numeric: {len(pairs)}; exact-reward-match: {exact} ({100*exact//len(pairs)}%)")

print("\n---- OUR_GAP (sandbox underperforms real CPython — true coverage gaps) ----")
for r in rows:
    if r[4] == "OUR_GAP":
        print(f"  {r[0]:46} sim={r[1]} gt={r[2]}  unsup={r[5][:4]}")

print("\n---- SIM_HIGHER (sandbox >= gt; usually gt missing deps) ----")
for r in rows:
    if r[4] == "SIM_HIGHER":
        print(f"  {r[0]:46} sim={r[1]} gt={r[2]}")

print("\n---- FAITHFUL with reward>0 (correctly simulated, oracle works) ----")
for r in faithful_pos:
    print(f"  {r[0]:46} reward={r[1]}")

# unsupported feature frequency
unsup = collections.Counter()
for r in rows:
    for u in r[5]:
        unsup[u] += 1
print("\n---- top unsupported features across all real-oracle tasks ----")
for k, n in unsup.most_common(30):
    print(f"  {n:3}  {k}")

json.dump([{"task": r[0], "sim": r[1], "gt": r[2], "status": r[3], "class": r[4]} for r in rows],
          open("/tmp/comparison.json", "w"), indent=2)
