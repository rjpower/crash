# shellsim

A **bring-your-own-sandbox** shell + Python simulator for agentic RL training, in a single
static Rust binary. Instead of running each rollout in a gVisor/VM/container, `shellsim`
*simulates* a bash session against an in-memory filesystem — deterministically, with no
real processes, network, or blocking — and runs the task's verifier to produce the same
reward a real machine would.

It is a **filter, not a replacement**: it covers the subset of tasks that stay inside a
faithfully-simulated envelope (bash + coreutils + Python stdlib), and it tells you, per
rollout, whether it stayed inside that envelope so you can route the rest to a real sandbox.

## What it simulates

- **Shell:** a real lexer + recursive-descent parser (pipes, heredocs, `if`/`for`/`while`/
  `case`, functions, `set -e`/`-u`/`-o pipefail`), word expansion (`$VAR`, `${..}`, `$(..)`,
  `` `..` ``, `$((..))`, globbing, splitting), and **bash arrays** (indexed + associative:
  `arr=(…)`, `${arr[@]}`, `${!m[@]}`, `${#a[@]}`, slices, `declare -A`).
- **~110 builtins / coreutils** dispatched in-process against an in-memory VFS (files, dirs,
  symlinks, modes): the usual `ls/cat/cp/mv/grep/sed/sort/cut/tr/find/jq/sha*/base64/…`.
- **Embedded Python** (RustPython, full stdlib) operating on the *same* in-memory VFS via a
  pure-Python bridge — including an `importlib` loader that imports from the VFS, a
  `subprocess` shim that re-enters the VM in-process, and a minimal `pytest` (with fixtures
  `tmp_path`/`monkeypatch`/`capsys`/…) so task verifiers run unmodified.
- **Virtual clock:** `sleep`/`timeout`/background jobs advance logical time and return
  instantly — no real blocking anywhere in a rollout.
- **Virtual network:** `curl`/`wget` resolve a route table (`net route <url> <status> <body>`)
  — no real egress.

## Status

Validated by comparing the sandbox's reward against **real CPython 3.14** running each task's
*real* oracle + verifier (see [`eval/`](eval/)). On the OpenThoughts-TBLite corpus (the 72
tasks of 100 that ship a reference solution):

| metric | value |
|---|---|
| tasks both harnesses can score | 57 |
| **exact reward match (faithful)** | **38 (66%)** — incl. byte-exact partial credit (0.922, 0.8825) |
| full passes (reward = 1.0) | 9 |
| true coverage gaps (sandbox < real) | 16 |

Every rollout emits a **trust signal** (`high`/`medium`/`low`) plus the offending
`trust_gaps`, so a training pipeline knows whether to use the simulated reward or fall back:

- `high` — nothing consequential was stubbed; the reward should match a real machine.
- `medium` — a dependency installer (`pip`/…) or an unknown tool was stubbed.
- `low` — a real-execution command ran as a no-op (compiler / runtime / server), **or** a
  known third-party module failed to import (`numpy`/`pandas`/…).

Distribution over the corpus: **high 28 / medium 18 / low 21** (5 error). All 9 tasks where
the sandbox produces a correct non-zero reward read `high`. The signal reliably flags
*resource* gaps; it does **not** catch subtle *logic* divergence (a task that runs
end-to-end but returns a wrong reward), which is why the `eval/` ground-truth harness stays
the backstop.

## What it does *not* cover

These are the boundaries — a task that needs any of them is a poor fit and should run in a
real sandbox:

- **No scientific Python stack** — `numpy`, `pandas`, `scipy`, `sklearn`, … are not
  simulated (they fail to import, which the trust signal reports as `low`). This is the
  single biggest gap (~5 of 16).
- **No native compilation** — `gcc`/`clang`/`cargo`/`make` are no-ops; tasks that build and
  then run a compiled artifact won't work.
- **No real services or concurrency** — listening sockets, `uvicorn`/`gunicorn`, databases,
  containers (`docker`), `vim`, and `flock`-style real concurrency are out of scope.
- **Package installs are stubbed** — `pip`/`apt`/`npm` pretend to succeed; a task that
  actually executes installed package *contents* won't have them.
- **Python is the stdlib subset RustPython supports.** Most stdlib works; exotic C-extension
  modules don't.

A task is a **good fit** when its oracle and verifier stay within: bash + coreutils + text
tools, the Python standard library, file/JSON/CSV I/O, hashing, and subprocess-ing its own
CLI. On this corpus that's roughly a quarter of tasks.

## Build

```sh
cargo build --release --features python
```

(Without `--features python` you get the shell-only binary; both write the same
`target/release/shellsim`, so always build with the feature for task runs.)

## Use

```sh
# run a shell snippet
./target/release/shellsim -c 'echo hello | tr a-z A-Z'

# run one TBLite task end-to-end (Dockerfile → oracle → verifier → reward + trust JSON)
./target/release/shellsim task <tblite-task-dir>

# run a whole corpus
./target/release/shellsim bench --json <corpus-dir>
```

Fake URLs and no-op sleep:

```sh
./target/release/shellsim -c '
  net route https://api.example.com/health 200 "{\"status\":\"ok\"}"
  curl -s https://api.example.com/health
  sleep 3600          # advances virtual time, returns instantly
'
```

## Layout

```
src/shell.rs     lexer + recursive-descent parser
src/expand.rs    word expansion + arrays + globbing
src/exec.rs      executor: pipelines, redirects, control flow, errexit/pipefail
src/commands/    ~110 builtins behind a registry (CommandSpec{run, trust}); see mod.rs
src/python/      embedded RustPython + the pure-Python VFS bridge (*.py via include_str!)
src/vfs.rs       in-memory filesystem
src/clock.rs     virtual clock
src/net.rs       virtual network (route table)
src/harness.rs   TBLite task runner (Dockerfile → oracle → verifier → reward + trust)
eval/            faithfulness harness (real-CPython ground truth; see eval/README.md)
```

`cargo test --features python` runs 22 unit tests (arrays, VFS, expansion, net routing, and
a CPython `py_compile` check on the embedded Python). End-to-end faithfulness lives in
`eval/`, which needs the external corpus and a CPython venv.

## Validation

The [`eval/`](eval/) directory contains the ground-truth comparison harness used throughout
development — it runs each task's *real* oracle and verifier under real CPython and compares
the reward to the sandbox's. See [eval/README.md](eval/README.md).
