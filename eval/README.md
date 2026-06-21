# Faithfulness evaluation harness

The core validation question for shellsim is **not** "what pass-rate does it get" but
**"does it reproduce the reward a real machine would give?"** These scripts answer that by
running each task's real oracle + verifier under real CPython and comparing.

## Setup

1. Clone the corpus (OpenThoughts-TBLite) somewhere, e.g. `/tmp/tblite`.
2. Create a CPython ground-truth venv with the deps verifiers expect:
   ```sh
   python3 -m venv /tmp/gtvenv
   /tmp/gtvenv/bin/pip install pytest pydantic pandas numpy scipy
   ```
3. The scripts use hard-coded `/tmp/...` paths (TBLite at `/tmp/tblite`, sandbox binary in
   this repo's `target/release/`). Adjust the constants at the top of each script if your
   layout differs.

## Scripts

- **`gt_run.py`** — ground truth. Interprets each task's Dockerfile, runs `solve.sh` and
  the pytest verifier under real CPython at rewritten paths, and records the reward
  (falling back to the pytest exit code when the reward is written by `test.sh`'s `$?`).
  Output: `/tmp/gt_results.json`.
- **`compare.py`** — runs `shellsim task <dir>` over every real-oracle task with a timeout,
  capturing the simulated reward. Output: `/tmp/sim_results.json`.
- **`analyze_compare.py`** — joins the two and classifies each task:
  `FAITHFUL` (exact match), `OUR_GAP` (sandbox < real — a true coverage gap),
  `SIM_HIGHER` (sandbox > real — usually a ground-truth-harness artifact),
  `GT_UNAVAILABLE` (the offline harness couldn't score it).
- **`real_oracle_tasks.txt`** — the 72 tasks (of 100) that ship a real reference solution.

## Known ground-truth-harness limitations

`gt_run.py` rewrites absolute paths inside `.py` files so they resolve under a writable
prefix. This changes a file's bytes, so tasks with SHA-anti-tamper tests
(`test_*_not_modified`) can fail in the ground truth while the sandbox correctly passes —
these surface as `SIM_HIGHER` and should be read as "the sandbox is right here." A real
container running at the real paths would not have this issue.
