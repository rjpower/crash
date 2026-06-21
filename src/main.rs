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
        eprintln!("usage: shellsim bench <tblite_root> [--only a,b] [--limit N] [--json PATH]");
        exit(2);
    };
    let mut only: Option<Vec<String>> = None;
    let mut limit: Option<usize> = None;
    let mut json_out: Option<String> = None;
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
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| harness::run_task(dir)))
            .unwrap_or_else(|_| TaskResult {
                task: name.clone(),
                reward: 0.0,
                passed: false,
                status: "panic".into(),
                note: "panicked".into(),
                unsupported: vec![],
                commands: vec![],
                virtual_slept_ms: 0,
                trust: "low".into(),
                trust_gaps: vec![],
            });
        eprintln!("{:8} reward={}", r.status, r.reward);
        results.push(r);
    }

    summarize(&results);
    if let Some(path) = json_out {
        let _ = std::fs::write(&path, serde_json::to_string_pretty(&results).unwrap());
        eprintln!("wrote {path}");
    }
}

fn summarize(results: &[TaskResult]) {
    let total = results.len();
    let pass = results.iter().filter(|r| r.status == "pass").count();
    let fail = results.iter().filter(|r| r.status == "fail").count();
    let err = results.iter().filter(|r| r.status == "error" || r.status == "panic").count();
    let noora = results.iter().filter(|r| r.status == "no-oracle").count();
    let with_oracle = total - noora;

    println!("\n================ COVERAGE SUMMARY ================");
    println!("tasks scanned:        {total}");
    println!("  with real oracle:   {with_oracle}");
    println!("  stub (no oracle):   {noora}");
    println!("oracle tasks PASSED:  {pass}   ({:.0}% of oracle tasks)", pct(pass, with_oracle));
    println!("oracle tasks FAILED:  {fail}");
    println!("oracle tasks ERRORED: {err}");

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

    println!("\n---- failing / erroring oracle tasks ----");
    for r in results {
        if r.status == "fail" || r.status == "error" || r.status == "panic" {
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
