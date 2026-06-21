//! Load and score Terminal-Bench / OpenThoughts-TBLite tasks against the simulator.
//!
//! A task directory has the shape:
//!   environment/Dockerfile (+ data/…)   the initial container state
//!   solution/solve.sh (+ helpers)        the oracle solution we run
//!   tests/test.sh, test_outputs.py, …    the verifier (writes /logs/verifier/reward.txt)
//!
//! We interpret the Dockerfile as a setup script (apt/pip become no-ops), load the oracle
//! and tests into the VFS, run the oracle, then run the pytest verifier and read the reward.

use std::path::{Path, PathBuf};

use crate::interp::Interp;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskResult {
    pub task: String,
    pub reward: f64,
    pub passed: bool,
    pub status: String, // "pass" | "fail" | "no-oracle" | "error"
    pub note: String,
    pub unsupported: Vec<String>,
    pub commands: Vec<String>,
    pub virtual_slept_ms: u64,
    /// "high" if no NoOp/Partial command ran during the task, else "low".
    pub trust: String,
    /// Names of any NoOp/Partial-trust commands that ran (empty when `trust == "high"`).
    pub trust_gaps: Vec<String>,
}

pub fn run_task(task_dir: &Path) -> TaskResult {
    let name = task_dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let solve_path = task_dir.join("solution/solve.sh");
    let solve_src = std::fs::read_to_string(&solve_path).unwrap_or_default();
    if is_stub(&solve_src) {
        return TaskResult {
            task: name,
            reward: 0.0,
            passed: false,
            status: "no-oracle".into(),
            note: "solve.sh is a stub (no reference solution)".into(),
            unsupported: vec![],
            commands: vec![],
            virtual_slept_ms: 0,
            trust: "high".into(),
            trust_gaps: vec![],
        };
    }

    let mut interp = Interp::new();
    interp.vfs.put_dir("/logs", 0o755);
    interp.vfs.put_dir("/logs/verifier", 0o755);

    // 1. Interpret the Dockerfile to build the initial environment.
    let env_dir = task_dir.join("environment");
    if let Ok(dockerfile) = std::fs::read_to_string(env_dir.join("Dockerfile")) {
        apply_dockerfile(&mut interp, &dockerfile, &env_dir);
    }
    let workdir = interp.cwd.clone();
    if std::env::var("SHELLSIM_DEBUG").is_ok() {
        eprintln!("---- workdir after Dockerfile: [{workdir}] ----");
    }

    // 2. Place the oracle solution and the tests into the VFS.
    load_host_dir(&mut interp, &task_dir.join("solution"), &join(&workdir, "solution"));
    load_host_dir(&mut interp, &task_dir.join("tests"), "/tests");

    // 3. Run the oracle (solve.sh).
    let mut out = Vec::new();
    let mut err = Vec::new();
    interp.cwd = workdir.clone();
    let _ = interp.run_script_into(&solve_src, &mut out, &mut err);

    let debug = std::env::var("SHELLSIM_DEBUG").is_ok();
    if debug {
        eprintln!("---- solve stdout ----\n{}", String::from_utf8_lossy(&out));
        eprintln!("---- solve stderr ----\n{}", String::from_utf8_lossy(&err));
        eprintln!("---- VFS files after solve ----");
        for (p, n) in interp.vfs.all_paths() {
            if let crate::vfs::NodeKind::File(d) = &n.kind {
                eprintln!("  {:7}  {}", d.len(), p);
            }
        }
    }

    // 4. Run the verifier. Prefer the task's own test.sh if present, else pytest the module.
    // The verifier is a fresh shell invocation in the real harness, so clear any shell options
    // (errexit/nounset/pipefail/xtrace) the oracle's `set -e` left behind — otherwise a failing
    // line like `source $HOME/.local/bin/env` would abort the verifier before it runs pytest.
    interp.exiting = None;
    interp.opt_errexit = false;
    interp.opt_nounset = false;
    interp.opt_pipefail = false;
    interp.opt_xtrace = false;
    interp.cwd = workdir.clone();
    let mut vout = Vec::new();
    let mut verr = Vec::new();
    if interp.vfs.is_file("/", "/tests/test.sh") {
        let test_src = interp.vfs.read_string("/", "/tests/test.sh").unwrap_or_default();
        let _ = interp.run_script_into(&test_src, &mut vout, &mut verr);
    } else {
        let _ = crate::python::run_pytest(
            &mut interp,
            &["/tests/test_outputs.py".to_string()],
            &mut vout,
            &mut verr,
        );
    }

    if debug {
        eprintln!("---- verifier stdout ----\n{}", String::from_utf8_lossy(&vout));
        eprintln!("---- verifier stderr ----\n{}", String::from_utf8_lossy(&verr));
        eprintln!(
            "---- reward.txt present: {} ----",
            interp.vfs.is_file("/", "/logs/verifier/reward.txt")
        );
    }

    // 5. Read the reward.
    let reward = interp
        .vfs
        .read_string("/", "/logs/verifier/reward.txt")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok());

    let unsupported = dedup(interp.unsupported.clone());
    let commands = dedup(interp.cmd_trace.clone());

    // Trust verdict: how much to trust this simulated reward as a stand-in for a real machine.
    // Fold in Python third-party import failures (a module the task needed wasn't available) —
    // these are not shell no-ops but are an equally strong "this reward is unreliable" signal.
    let (mut trust, mut trust_gaps) = trust_verdict(&interp.trust_noop, &interp.trust_partial);
    let mut missing = missing_modules(&String::from_utf8_lossy(&err));
    missing.extend(missing_modules(&String::from_utf8_lossy(&verr)));
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        trust = "low".to_string();
        for m in &missing {
            trust_gaps.push(format!("import:{m}"));
        }
    }
    // A reimplemented scientific library was used: the reward is plausible but not guaranteed
    // byte-identical to the real library, so cap trust at `medium` (the eval/ ground-truth
    // harness is the backstop for logic divergence). Does not downgrade an already-`low`.
    if !interp.py_simlib.is_empty() {
        if trust == "high" {
            trust = "medium".to_string();
        }
        for m in &interp.py_simlib {
            trust_gaps.push(format!("simlib:{m}"));
        }
    }
    // An OOD event means a mini-library hit a path it does not faithfully implement — the
    // reward is unreliable, full stop.
    if !interp.py_ood.is_empty() {
        trust = "low".to_string();
        let mut ood = dedup(interp.py_ood.clone());
        ood.truncate(8);
        for m in ood {
            trust_gaps.push(format!("ood:{m}"));
        }
    }
    trust_gaps.sort();
    trust_gaps.dedup();
    let (reward, passed, status, note) = match reward {
        Some(r) => {
            let passed = r >= 0.999;
            let status = if passed { "pass" } else { "fail" };
            let note = if passed {
                "reward=1".to_string()
            } else {
                format!("reward={r}; stderr: {}", tail(&verr, 240))
            };
            (r, passed, status.to_string(), note)
        }
        None => (
            0.0,
            false,
            "error".to_string(),
            format!("no reward.txt written; verifier stderr: {}", tail(&verr, 300)),
        ),
    };

    TaskResult {
        task: name,
        reward,
        passed,
        status,
        note,
        unsupported,
        commands,
        virtual_slept_ms: interp.clock.slept_ms,
        trust,
        trust_gaps,
    }
}

/// Classify how trustworthy a simulated reward is, from which *no-op'd* commands actually ran.
///
/// - `high`   — nothing consequential was stubbed; the reward should match a real machine.
/// - `medium` — a dependency installer was stubbed (`pip`/`conda`/`npm install`/…). The reward
///   is usually right, but if the task's code actually *uses* that dependency it could be wrong
///   — worth spot-checking against a real sandbox.
/// - `low`    — a command that genuinely needs real execution ran as a no-op (compiler, language
///   runtime, container/service). The reward is not reliable; route this task to a real sandbox.
///
/// Only no-op'd commands feed the verdict. `apt-get`/`add-apt-repository`/etc. are pure system
/// bootstrap that can't change a simulable task's reward and are ignored. Partial-fidelity tools
/// (the `jq`/`sed` subsets) are *not* counted here — they produce real output, so any divergence
/// shows up as a wrong reward and is caught by the ground-truth eval harness, not by this flag.
/// Extract *known third-party* module names from `ModuleNotFoundError: No module named 'X'`
/// tracebacks. Only an uncaught failure prints this line, so a `try/except ImportError` won't
/// trip it. We restrict to a curated heavy-dependency set so that a handled/local import of e.g.
/// `model` doesn't false-flag a faithful task; a missing `numpy`/`pandas` is unambiguous.
fn missing_modules(stderr: &str) -> Vec<String> {
    const THIRD_PARTY: &[&str] = &[
        "numpy", "pandas", "scipy", "sklearn", "scikit", "torch", "tensorflow", "keras",
        "matplotlib", "seaborn", "flask", "django", "fastapi", "starlette", "requests", "httpx",
        "aiohttp", "redis", "fakeredis", "celery", "lxml", "bs4", "sqlalchemy", "psycopg2",
        "pymongo", "boto3", "cryptography", "nacl", "jwt", "PIL", "cv2", "skimage", "networkx",
        "sympy", "statsmodels", "xgboost", "lightgbm", "transformers", "datasets", "rdkit",
        "openai", "anthropic", "yaml", "toml", "openpyxl", "pyarrow", "polars", "numba",
    ];
    let mut out = Vec::new();
    for line in stderr.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("ModuleNotFoundError: No module named ") {
            let name = rest.trim().trim_matches(|c| c == '\'' || c == '"');
            let top = name.split('.').next().unwrap_or(name);
            if THIRD_PARTY.contains(&top) {
                out.push(top.to_string());
            }
        }
    }
    out
}

fn trust_verdict(
    noop: &std::collections::BTreeSet<String>,
    _partial: &std::collections::BTreeSet<String>,
) -> (String, Vec<String>) {
    // System bootstrap that can't change a pure-simulation reward — ignored.
    const BENIGN: &[&str] = &[
        "apt", "apt-get", "apt-cache", "add-apt-repository", "update-alternatives", "dpkg",
        "dpkg-reconfigure", "ldconfig", "groupadd", "useradd", "usermod",
    ];
    // Stubbing these genuinely needs real execution — the reward can't be trusted.
    const CONSEQUENTIAL: &[&str] = &[
        "gcc", "g++", "cc", "clang", "make", "cmake", "node", "cargo", "go", "javac", "java",
        "mvn", "gradle", "bazel", "docker", "docker-compose", "systemctl", "service", "uvicorn",
        "gunicorn", "flask", "redis-server", "mongod", "mysqld", "postgres", "nginx", "rustc",
    ];
    let mut gaps: Vec<String> =
        noop.iter().filter(|c| !BENIGN.contains(&c.as_str())).cloned().collect();
    gaps.sort();
    gaps.dedup();
    let trust = if gaps.is_empty() {
        "high"
    } else if gaps.iter().any(|c| CONSEQUENTIAL.contains(&c.as_str())) {
        "low"
    } else {
        "medium"
    };
    (trust.to_string(), gaps)
}

fn is_stub(src: &str) -> bool {
    let body: String = src
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#') && !t.starts_with("#!")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let compact: String = body.split_whitespace().collect();
    body.lines().count() <= 1
        && (compact.contains("nosolution")
            || compact == "exit0"
            || compact == "true"
            || compact.is_empty())
}

/// Interpret a Dockerfile subset: ENV, WORKDIR, COPY/ADD, RUN.
fn apply_dockerfile(interp: &mut Interp, dockerfile: &str, ctx: &Path) {
    let lines = join_continuations(dockerfile);
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (instr, rest) = match line.split_once(char::is_whitespace) {
            Some((a, b)) => (a.to_uppercase(), b.trim()),
            None => (line.to_uppercase(), ""),
        };
        match instr.as_str() {
            "ENV" => {
                // ENV K=V  or  ENV K V
                if let Some((k, v)) = rest.split_once('=') {
                    interp.set_var(k.trim(), v.trim().trim_matches('"'));
                    interp.export(k.trim());
                } else if let Some((k, v)) = rest.split_once(char::is_whitespace) {
                    interp.set_var(k.trim(), v.trim().trim_matches('"'));
                    interp.export(k.trim());
                }
            }
            "WORKDIR" => {
                let p = expand_simple(interp, rest);
                let _ = interp.vfs.mkdir_all("/", &p);
                interp.cwd = crate::vfs::resolve_against("/", &p);
                interp.set_var("PWD", interp.cwd.clone());
            }
            "COPY" | "ADD" => {
                let parts = shell_words(rest);
                if parts.len() >= 2 {
                    let dst = parts.last().unwrap().clone();
                    for src in &parts[..parts.len() - 1] {
                        if src.starts_with("--") {
                            continue;
                        }
                        copy_host_into_vfs(interp, ctx, src, &dst);
                    }
                }
            }
            "RUN" => {
                let mut o = Vec::new();
                let mut e = Vec::new();
                let _ = interp.run_script_into(rest, &mut o, &mut e);
                interp.exiting = None;
            }
            _ => {} // FROM, ENV, EXPOSE, CMD, etc. ignored
        }
    }
}

fn copy_host_into_vfs(interp: &mut Interp, ctx: &Path, src: &str, dst: &str) {
    let host = ctx.join(src.trim_start_matches("./"));
    let dst_abs = crate::vfs::resolve_against(&interp.cwd, dst);
    if host.is_dir() {
        load_host_dir(interp, &host, &dst_abs);
    } else if host.is_file() {
        if let Ok(data) = std::fs::read(&host) {
            // if dst ends with '/', keep filename
            let target = if dst.ends_with('/') {
                join(&dst_abs, &file_name(&host))
            } else {
                dst_abs
            };
            interp.vfs.put_file(&target, data, 0o644);
        }
    } else if src == "." {
        load_host_dir(interp, ctx, &dst_abs);
    }
}

/// Recursively load a host directory into the VFS at `dest` (skipping caches/Dockerfile).
fn load_host_dir(interp: &mut Interp, host: &Path, dest: &str) {
    if !host.is_dir() {
        return;
    }
    interp.vfs.put_dir(dest, 0o755);
    let mut stack: Vec<(PathBuf, String)> = vec![(host.to_path_buf(), dest.to_string())];
    while let Some((dir, vdir)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "__pycache__" || name == "Dockerfile" || name.ends_with(".pyc") {
                continue;
            }
            let vpath = join(&vdir, &name);
            if p.is_dir() {
                interp.vfs.put_dir(&vpath, 0o755);
                stack.push((p, vpath));
            } else if let Ok(data) = std::fs::read(&p) {
                interp.vfs.put_file(&vpath, data, 0o644);
            }
        }
    }
}

fn join_continuations(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.lines() {
        let trimmed = line.trim_end();
        if let Some(stripped) = trimmed.strip_suffix('\\') {
            cur.push_str(stripped);
            cur.push(' ');
        } else {
            cur.push_str(trimmed);
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn expand_simple(interp: &Interp, s: &str) -> String {
    let mut out = s.to_string();
    for (k, v) in &interp.vars {
        out = out.replace(&format!("${k}"), v).replace(&format!("${{{k}}}"), v);
    }
    out.trim_matches('"').to_string()
}

fn shell_words(s: &str) -> Vec<String> {
    s.split_whitespace().map(|w| w.trim_matches('"').to_string()).collect()
}

fn join(a: &str, b: &str) -> String {
    format!("{}/{}", a.trim_end_matches('/'), b)
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

fn tail(bytes: &[u8], n: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    if s.len() <= n {
        s.to_string()
    } else {
        format!("…{}", &s[s.len() - n..])
    }
}
