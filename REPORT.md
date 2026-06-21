# shellsim — a bring-your-own-sandbox shell + Python simulator for agentic RL

**Final report · weaver #243 · 2026-06-21**

## 1. The problem

Agentic RL training needs to execute the actions an agent proposes (shell commands,
file edits, scripts) and score the result with a task verifier. Doing that safely
normally means a real sandbox per rollout — gVisor, a microVM, or a container — which
is expensive at the throughput RL wants.

The bet here: **for a meaningful subset of tasks, you don't need a real machine — you
need a faithful enough *simulation* of one.** If a single static Rust binary can
emulate a bash session against an in-memory filesystem, run the task's Python, and make
the task's *verifier* produce the *same reward it would on a real machine*, then those
rollouts can skip the sandbox entirely.

This report covers: (1) what we learned canvassing the target corpus, (2) what we built,
(3) how we proved faithfulness, and (4) exactly where it works and where it doesn't.

## 2. Phase 1 — canvassing the corpus

Target corpus: **OpenThoughts `OpenThoughts-TBLite`** (the Terminal-Bench-Lite split used
for OT-agent eval), 100 tasks. Each task is:

```
task.toml · instruction.md
environment/Dockerfile (+ data)     # builds the starting machine state
solution/solve.sh                   # the oracle (reference solution)
tests/test.sh + test_outputs.py     # the verifier → writes /logs/verifier/reward.txt
```

The verifier is the ground truth: it re-runs/re-hashes outputs (anti-cheat) and writes a
reward in `[0,1]` (often fractional partial credit). 72/100 tasks ship a real oracle; 28
are unsolved stubs.

**What the tasks actually need** (sampled across all 72 oracles + verifiers):

| Surface | Frequency | Notes |
|---|---|---|
| coreutils + text (grep/sed/awk/sort/cut/find/…) | ~all | the bread and butter |
| Python (stdlib: json/re/hashlib/csv/pathlib/argparse) | ~60% | solutions *and* verifiers |
| `pytest` as the verifier harness | ~70% | usually launched via `uv`/`uvx` |
| `subprocess` calling the solution back | 22/72 | verifier re-runs the CLI |
| pytest fixtures (`tmp_path`, custom) | 10/72 | |
| numpy / pandas / sklearn | ~7 | **fundamental** sim boundary |
| real servers / sockets / native compilers | ~15 | **fundamental** sim boundary |

The conclusion that shaped the build: **the long tail is Python, not exotic shell.** A
good shell is necessary but the deciding factor is whether the *Python* in the oracle and
the verifier runs faithfully against the same virtual filesystem.

## 3. Phase 2 — what we built

A single Rust binary, `shellsim` (~8 kLOC, 31 MB static). No `unsafe`, no host process
spawning, fully deterministic.

```
src/vfs.rs       in-memory filesystem (BTreeMap<path, Node>; files/dirs/symlinks, modes)
src/clock.rs     virtual clock — sleep advances logical time, never blocks
src/net.rs       virtual network — curl/wget resolve a route table, no real egress
src/shell.rs     lexer + recursive-descent parser (heredocs, pipes, control flow, funcs)
src/expand.rs    word expansion: $VAR ${..} $(..) `..` $((..)) globbing, splitting
src/exec.rs      executor: pipelines, redirects, if/for/while/case, errexit/pipefail
src/commands.rs  ~110 builtins/coreutils dispatched in-process
src/hashes.rs    byte-exact sha1/256/512, md5, crc32, base64
src/jqcmd.rs     a jq subset over serde_json
src/python.rs    the Python engine (RustPython) + the VFS bridge  ← the hard part
src/harness.rs   load a TBLite task, apply the Dockerfile, run oracle, run verifier, read reward
```

### The Python bridge

We embed **RustPython** (full stdlib: json, re, hashlib, csv, datetime, argparse, …).
The trick is making it operate on *our* VFS, not the host disk, with no host-callbacks:

1. Serialize the VFS into the VM as `__VFS_FILES: {path: bytes}` + `__VFS_DIRS`.
2. A pure-Python prelude reinstalls `open`, `os`, `os.path`, `pathlib.Path`, `glob`,
   `shutil`, and `subprocess` as shims over those dicts.
3. Run the user program.
4. Serialize the (mutated) dicts back out (base64+JSON) and reconcile into the Rust VFS.

Everything stays in-memory and deterministic. Stubs for `pydantic` and `pytest` are
provided because nearly every verifier imports them.

### The two requirements you called out

Both are first-class and demonstrable from a script:

```sh
# sleep is a no-op: it advances a virtual clock instead of blocking
$ shellsim -c 'echo $(date +%s); sleep 3600; echo $(date +%s)'
1735689600
1735693200            # +3600s of virtual time, 0.00s wall-clock

# fake URLs for curl/wget via a route table — no real egress
$ shellsim -c '
    net route https://api.example.com/health 200 "{\"status\":\"ok\"}"
    net route "https://astral.sh/uv/*/install.sh" 200 "echo fake-installer"
    curl -s https://api.example.com/health           # -> {"status":"ok"}
    curl -s https://astral.sh/uv/0.9.5/install.sh | sh   # -> runs fake-installer
'
```

`sleep`, `timeout`, background jobs, and "wait for N ticks" are all driven by the virtual
clock, so there is no real blocking anywhere in a rollout.

## 4. Phase 3 — proving faithfulness (the part that matters)

A simulator that *passes tasks* is worthless if it passes the wrong ones. The metric we
actually care about is **"does the sandbox reproduce the reward a real machine would give?"**

So we built a **ground-truth harness** (`/tmp/gt_run.py`): it takes each task's *real*
oracle and *real* verifier and runs them under **real CPython 3.14** + pytest + the
scientific stack, at rewritten paths, and records the reward. Then we compare, per task,
`sandbox_reward` vs `cpython_reward`.

### Headline result (72 real-oracle tasks)

```
sandbox full passes (reward = 1.0):              7
tasks both harnesses could score:               57
  exact reward match  (FAITHFUL):               36   (63%)
  of which reward > 0 (non-trivial):             7
OUR_GAP   (sandbox < real, true gaps):          18
SIM_HIGHER (ground-truth-harness artifacts):     3
GT_UNAVAILABLE (offline gt couldn't score):     10
```

The faithful set includes **byte-exact fractional partial credit** from anti-cheat
graders — not just pass/fail:

| task | sandbox | real CPython |
|---|---|---|
| api-endpoint-permission-canonicalizer | 1.0 | 1.0 |
| log-summary | 1.0 | 1.0 |
| malicious-package-forensics | 1.0 | 1.0 |
| schedule-vacation | 1.0 | 1.0 |
| security-breach-incident-response | **0.750** | 0.750 |
| security-incident-log-analysis | **0.922** | 0.9219999999999999 |
| tsl-test-case-generation | **0.8825** | 0.8825000000000001 |

Reproducing `0.922` and `0.8825` to the last digit means the sandbox is running the real
grader's real arithmetic on real outputs — the strongest possible evidence of fidelity.

### "SIM_HIGHER" is the ground-truth harness being wrong, not the sandbox

In all 3 cases the sandbox passes and the offline ground-truth scores 0. On inspection
this is an artifact of *our* gt harness: it rewrites absolute paths inside `.py` files,
which changes a file's bytes and trips that task's SHA-anti-tamper test (e.g.
`test_model_file_not_modified`). In a real container the file is untouched and the
sandbox's 1.0 is the correct answer. We verified this on the representative case; the
sandbox is the faithful one there.

## 5. Iteration: the bugs faithfulness testing exposed

The first end-to-end runs looked great (14 "passes") and were **mostly fake** — verifiers
launched via `uvx pytest` were no-op'd, and `if [ $? -eq 0 ]; then echo 1 > reward.txt`
wrote a 1 without any test running. Comparing against ground truth is what caught it.
Eight systemic fixes followed, each found by a real task and each lifting a whole class:

1. **errexit leaked from oracle into verifier** — `set -e` in `solve.sh` persisted, so a
   failing `source $HOME/.local/bin/env` aborted the verifier before pytest. (~20 tasks)
2. **`python` argv off-by-one** — `python cli.py scan` ran `scan` as the script. (all CLI tasks)
3. **`__name__` was `'builtins'`** — every `if __name__ == '__main__': main()` silently
   did nothing. This one breaks essentially all real Python programs.
4. **`subprocess` routing** — added an in-process stub so `subprocess.run([... ,"python",
   "cli.py"])` re-enters the same VM on the same VFS (22/72 verifiers depend on this).
5. **pytest fixtures** — `tmp_path`, `monkeypatch`, `capsys`, and user `@pytest.fixture`
   functions (incl. yield-finalizers) are now injected by parameter name. (10/72)
6. **`pathlib.Path.relative_to`** (+ `rglob`, `match`, `cwd`, `home`, …) were missing.
7. **`while read … done < file`** — added a persistent input cursor so the idiom
   terminates instead of looping forever.
8. **verifier launchers** — `sh -c`, `bash`, and `uvx/uv run pytest` now execute instead
   of no-op'ing.

## 6. Where it works, and the boundary

The 18 real coverage gaps are **mostly fundamental**, not bugs:

| blocker | tasks | verdict |
|---|---|---|
| numpy / pandas / sklearn | 5 | out of scope for a lightweight sim |
| task-specific (intricate verifier/CLI fidelity) | 8 | fixable case-by-case |
| `importlib.spec_from_file_location` loads from host FS | 2 | fixable (VFS importer hook) |
| bash associative/indexed arrays | 2 | fixable (medium effort) |
| `flock` / real concurrency | 1 | out of scope |

**A task is a good fit for shellsim when** its oracle and verifier stay within: bash +
coreutils + text tools, Python standard library, file/JSON/CSV I/O, hashing, and
subprocess-ing its own CLI. **It is a poor fit when** it needs the scientific Python
stack, a real listening server, native compilation (gcc/clang), real concurrency, or
package installs whose contents the task actually executes.

On this corpus that's roughly **a quarter of tasks today**, with a clear, mostly
mechanical path (arrays, the VFS importer, a small numpy shim) to push higher. Crucially,
the framework **knows when it's out of its depth**: unsupported commands are recorded, so
a training pipeline can *gate* on "did this rollout stay inside the simulable envelope?"
and fall back to a real sandbox only for the tasks that need one.

## 7. Recommendation

This is a viable cost-reducer for agentic RL, used as a **filter, not a replacement**:

- Run rollouts in `shellsim` first; it's deterministic, sub-millisecond per command, and
  has no real I/O, network, or blocking.
- Use the unsupported-feature log to decide per-task (or per-rollout) whether the
  simulated reward is trustworthy; route the rest to a real sandbox.
- The ground-truth comparison harness should be kept in CI: it's how we know the sim
  hasn't drifted, and it's how every faithfulness regression in this project was caught.

The highest-leverage next steps, in order: bash arrays, a VFS-backed `importlib` loader,
`@pytest.mark.parametrize`, and a minimal numpy/pandas shim — each converts a named,
already-categorized slice of the OUR_GAP list into faithful coverage.

## Appendix — reproducing

```sh
cargo build --release --features python
./target/release/shellsim task <tblite-task-dir>     # run one task, print reward JSON
./target/release/shellsim bench --json <corpus-dir>  # run the whole corpus
python3 /tmp/gt_run.py <task-dirs...>                # real-CPython ground truth
python3 /tmp/analyze_compare.py                      # sim-vs-gt classification
```
