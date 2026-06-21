//! shellsim CLI.
//!
//! Usage:
//!   shellsim -c '<command>'                 run a command string in a fresh sandbox
//!   shellsim run <script.sh> [args...]      run a shell script
//!   shellsim task <task_dir>                run & score one TBLite task
//!   shellsim bench <tblite_root> [opts]     run & score many tasks, print coverage
//!     --only a,b,c     only these task names
//!     --limit N        first N tasks
//!     --json PATH      write full results as JSON
//!   (no args)                               read a script from stdin

use std::path::Path;
use std::process::exit;

use shellsim::harness::{self, TaskResult};
use shellsim::Interp;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Install the kernel-level syscall sandbox (deny network/exec/fork) before running any task
    // or Python code in THIS process. shellsim never needs those syscalls; this closes the escape
    // hatches that no amount of Python-level monkeypatching can fully cover (see `shellsim::sandbox`).
    //
    // The `bench` orchestrator is the one exemption: it runs nothing untrusted in-process — it
    // spawns one *sandboxed* `shellsim task` child per task so it can wall-clock- and memory-bound
    // each one and hard-kill runaways. Spawning those children needs the very fork/exec syscalls
    // the sandbox denies, so the parent stays unsandboxed; every child re-applies the sandbox.
    if args.get(1).map(|s| s.as_str()) != Some("bench") {
        shellsim::sandbox::apply();
    }
    if args.len() < 2 {
        let src = read_stdin();
        run_script(&src, &[]);
        return;
    }
    match args[1].as_str() {
        "-c" => {
            let src = args.get(2).cloned().unwrap_or_default();
            run_script(&src, &args[3..]);
        }
        "run" => {
            let Some(path) = args.get(2) else {
                eprintln!("usage: shellsim run <script.sh>");
                exit(2);
            };
            let src = std::fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("cannot read {path}: {e}");
                exit(2);
            });
            run_script(&src, &args[3..]);
        }
        "task" => {
            let Some(dir) = args.get(2) else {
                eprintln!("usage: shellsim task <task_dir>");
                exit(2);
            };
            let r = harness::run_task(Path::new(dir));
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
            exit(if r.passed { 0 } else { 1 });
        }
        "bench" => bench(&args[2..]),
        other => {
            eprintln!("unknown subcommand: {other}");
            exit(2);
        }
    }
}

fn run_script(src: &str, args: &[String]) {
    let mut interp = Interp::new();
    interp.positional = args.to_vec();
    // a sensible default working area
    interp.vfs.put_dir("/work", 0o755);
    interp.cwd = "/work".to_string();
    interp.set_var("PWD", "/work");
    let code = interp.run_script(src);
    exit(code);
}

fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

fn bench(args: &[String]) {
    let Some(root) = args.first() else {
        eprintln!("usage: shellsim bench <tblite_root> [--only a,b] [--limit N] [--timeout SECS] [--json PATH]");
        exit(2);
    };
    let mut only: Option<Vec<String>> = None;
    let mut limit: Option<usize> = None;
    let mut json_out: Option<String> = None;
    // Per-task wall-clock budget. A task that exceeds it (a real hang, or a pure-Python numerical
    // workload too heavy to simulate in time) is flagged `timeout` and skipped rather than
    // stalling the whole sweep. 0 disables the budget.
    let mut timeout_secs: u64 = 120;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--only" => {
                only = args.get(i + 1).map(|s| s.split(',').map(|x| x.to_string()).collect());
                i += 1;
            }
            "--limit" => {
                limit = args.get(i + 1).and_then(|s| s.parse().ok());
                i += 1;
            }
            "--timeout" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse().ok()) {
                    timeout_secs = v;
                }
                i += 1;
            }
            "--json" => {
                json_out = args.get(i + 1).cloned();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    let mut task_dirs: Vec<_> = std::fs::read_dir(root)
        .unwrap_or_else(|e| {
            eprintln!("cannot read {root}: {e}");
            exit(2);
        })
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("solution/solve.sh").is_file())
        .collect();
    task_dirs.sort();

    if let Some(only) = &only {
        task_dirs.retain(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            only.iter().any(|o| n.contains(o.as_str()))
        });
    }
    if let Some(l) = limit {
        task_dirs.truncate(l);
    }

    let mut results: Vec<TaskResult> = Vec::new();
    for dir in &task_dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        eprint!("running {name:48} ... ");
        let r = run_task_bounded(dir, &name, timeout_secs);
        eprintln!("{:8} reward={}", r.status, r.reward);
        results.push(r);
    }

    summarize(&results);
    if let Some(path) = json_out {
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&results).unwrap());
        eprintln!("wrote {path}");
    }
}

/// Synthesize a TaskResult for a task we couldn't score in-band (timeout / crash / spawn failure).
fn sentinel_result(name: &str, status: &str, note: String, gap: &str) -> TaskResult {
    TaskResult {
        task: name.to_string(),
        reward: 0.0,
        passed: false,
        status: status.to_string(),
        note,
        unsupported: vec![],
        commands: vec![],
        virtual_slept_ms: 0,
        trust: "low".into(),
        trust_gaps: if gap.is_empty() { vec![] } else { vec![gap.to_string()] },
    }
}

/// Run one task as an isolated `shellsim task <dir>` **subprocess**, bounded in both wall-clock
/// and address space. A task that hangs is SIGKILL'd at `timeout_secs`; a task that runs away on
/// memory hits its `RLIMIT_AS` cap and dies on its own — neither can take down the bench (the
/// reason a thread-based timeout was insufficient: a detached worker thread keeps allocating).
/// The child re-applies the seccomp sandbox; the parent stays unsandboxed so it can spawn + kill.
/// `timeout_secs == 0` disables the wall-clock budget.
fn run_task_bounded(dir: &std::path::Path, name: &str, timeout_secs: u64) -> TaskResult {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("shellsim"));
    let tmp = std::env::temp_dir().join(format!(
        "shellsim-bench-{}.json",
        name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect::<String>()
    ));
    let outf = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return sentinel_result(name, "error", format!("cannot open result file: {e}"), ""),
    };

    let mut cmd = Command::new(&exe);
    cmd.arg("task").arg(dir).stdout(Stdio::from(outf)).stderr(Stdio::null());
    // Cap the child's address space so a memory-runaway task fails allocation (and aborts) rather
    // than OOM-killing the host. Generous vs. the simulated workloads (RustPython + a few MB of VFS).
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            let lim = libc::rlimit { rlim_cur: 8 << 30, rlim_max: 8 << 30 };
            libc::setrlimit(libc::RLIMIT_AS, &lim);
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return sentinel_result(name, "error", format!("spawn failed: {e}"), ""),
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if timeout_secs > 0 && Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&tmp);
                    return sentinel_result(
                        name,
                        "timeout",
                        format!("exceeded {timeout_secs}s budget — too slow/hung to simulate; skipped (expected to fail)"),
                        "timeout",
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return sentinel_result(name, "error", format!("wait failed: {e}"), "");
            }
        }
    }

    let mut s = String::new();
    let _ = std::fs::File::open(&tmp).and_then(|mut f| f.read_to_string(&mut s));
    let _ = std::fs::remove_file(&tmp);
    serde_json::from_str::<TaskResult>(&s).unwrap_or_else(|_| {
        // No parseable result: the child crashed (OOM via RLIMIT, panic, or abort).
        sentinel_result(
            name,
            "error",
            "child produced no result (likely crashed — OOM/abort/panic)".into(),
            "crash",
        )
    })
}

fn summarize(results: &[TaskResult]) {
    let total = results.len();
    let pass = results.iter().filter(|r| r.status == "pass").count();
    let fail = results.iter().filter(|r| r.status == "fail").count();
    let err = results.iter().filter(|r| r.status == "error" || r.status == "panic").count();
    let timeout = results.iter().filter(|r| r.status == "timeout").count();
    let noora = results.iter().filter(|r| r.status == "no-oracle").count();
    let with_oracle = total - noora;

    println!("\n================ COVERAGE SUMMARY ================");
    println!("tasks scanned:        {total}");
    println!("  with real oracle:   {with_oracle}");
    println!("  stub (no oracle):   {noora}");
    println!("oracle tasks PASSED:  {pass}   ({:.0}% of oracle tasks)", pct(pass, with_oracle));
    println!("oracle tasks FAILED:  {fail}");
    println!("oracle tasks ERRORED: {err}");
    println!("oracle tasks TIMEOUT: {timeout}   (skipped — too slow/hung to simulate)");

    // unsupported-command frequency across all tasks
    let mut unsup: std::collections::BTreeMap<String, usize> = Default::default();
    for r in results {
        for u in &r.unsupported {
            *unsup.entry(u.clone()).or_default() += 1;
        }
    }
    if !unsup.is_empty() {
        println!("\n---- top unsupported features (coverage gaps) ----");
        let mut v: Vec<_> = unsup.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        for (k, n) in v.into_iter().take(25) {
            println!("  {n:3}  {k}");
        }
    }

    println!("\n---- failing / erroring / timed-out oracle tasks ----");
    for r in results {
        if r.status == "fail" || r.status == "error" || r.status == "panic" || r.status == "timeout" {
            println!("  {:10} {:42} {}", r.status, r.task, r.note);
        }
    }
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        100.0 * a as f64 / b as f64
    }
}
