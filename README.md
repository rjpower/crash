# shellsim

A **bring-your-own-sandbox** shell + Python simulator for agentic RL training, in a single
static Rust binary. Instead of running each rollout in a gVisor/VM/container, `shellsim`
*simulates* a bash session against an in-memory filesystem — deterministically, with no
real processes, network, or blocking — and runs the task's verifier to produce the same
reward a real machine would.

See **[REPORT.md](REPORT.md)** for the full design, validation methodology, and results.

## TL;DR

- ~110 builtins/coreutils, a real shell parser (pipes, heredocs, control flow, functions).
- Embedded Python (RustPython, full stdlib) operating on the same in-memory VFS via a
  pure-Python bridge — including a `subprocess` shim that re-enters the VM in-process and
  a minimal `pytest` (with fixtures).
- `sleep`/`timeout`/background jobs are driven by a **virtual clock** (no real blocking).
- `curl`/`wget` resolve a **virtual route table** (`net route …`) — no real egress.
- **Validated against real CPython:** on the 57 OpenThoughts-TBLite tasks where both a
  faithful oracle and our sandbox can be scored, the sandbox reproduces the real reward
  exactly 63% of the time — including byte-exact fractional partial credit (0.922, 0.8825).

## Build

```sh
cargo build --release --features python
```

## Use

```sh
# run a shell snippet
./target/release/shellsim -c 'echo hello | tr a-z A-Z'

# run one TBLite task end-to-end (Dockerfile → oracle → verifier → reward)
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

## Validation

The `eval/` directory contains the ground-truth comparison harness used throughout
development — it runs each task's *real* oracle and verifier under real CPython and
compares the reward to the sandbox's. See [eval/README.md](eval/README.md).
