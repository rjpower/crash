//! "Process-ish" commands: virtual-clock time/scheduling (sleep/usleep/timeout/date/sync),
//! nested shells (sh/bash/dash/zsh), uv launchers, the Python engine, jq, and pytest.

use std::collections::HashMap;

use crate::commands::util::{parse_duration, wln};
use crate::commands::{CommandSpec, Io, Trust};
use crate::interp::Interp;

pub fn register(m: &mut HashMap<&'static str, CommandSpec>) {
    use super::reg;
    // time / scheduling (virtual clock, never blocks)
    reg(m, &["sleep"], Trust::Real, cmd_sleep);
    reg(m, &["usleep"], Trust::Real, cmd_usleep);
    reg(m, &["timeout"], Trust::Real, cmd_timeout);
    reg(m, &["date"], Trust::Real, cmd_date);
    reg(m, &["sync"], Trust::Real, |_, _, _| 0);

    // nested shells / uv-launched verifiers
    reg(m, &["sh", "bash", "dash", "zsh"], Trust::Real, cmd_sh);
    reg(m, &["uv", "uvx", "uvenv"], Trust::Partial, cmd_uv);

    // interpreters
    reg(m, &["python3"], Trust::Real, cmd_python3);
    reg(m, &["python"], Trust::Real, cmd_python);
    reg(m, &["python3.11"], Trust::Real, cmd_python311);
    reg(m, &["python3.12"], Trust::Real, cmd_python312);
    reg(m, &["python3.13"], Trust::Real, cmd_python313);
    reg(m, &["jq"], Trust::Partial, cmd_jq);
    reg(m, &["pytest", "py.test"], Trust::Real, cmd_pytest);
}

fn cmd_sleep(interp: &mut Interp, args: &[String], _io: &mut Io) -> i32 {
    let secs = args.first().map(|s| parse_duration(s)).unwrap_or(0);
    interp.clock.sleep_ms(secs);
    0
}

fn cmd_usleep(interp: &mut Interp, args: &[String], _io: &mut Io) -> i32 {
    let us: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    interp.clock.sleep_ms(us / 1000);
    0
}

fn cmd_timeout(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    // timeout DURATION CMD ... : we never actually time out (virtual clock), just run cmd
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    if rest.is_empty() {
        return 0;
    }
    let stdin = std::mem::take(&mut io.stdin);
    crate::commands::run(interp, &rest, stdin, io.out, io.err)
}

fn cmd_date(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    let secs = interp.clock.unix_secs();
    // handle +FORMAT and %s
    if let Some(fmt) = args.iter().find(|a| a.starts_with('+')) {
        let f = &fmt[1..];
        if f.contains("%s") {
            wln(io.out, &f.replace("%s", &secs.to_string()));
        } else {
            wln(io.out, &format_date(secs, f));
        }
    } else {
        wln(io.out, &format_date(secs, "%a %b %e %H:%M:%S UTC %Y"));
    }
    0
}

fn format_date(secs: u64, fmt: &str) -> String {
    // convert unix secs to UTC fields (proleptic Gregorian)
    let days = (secs / 86400) as i64;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days);
    fmt.replace("%Y", &format!("{y:04}"))
        .replace("%m", &format!("{mo:02}"))
        .replace("%d", &format!("{d:02}"))
        .replace("%H", &format!("{h:02}"))
        .replace("%M", &format!("{mi:02}"))
        .replace("%S", &format!("{s:02}"))
        .replace("%e", &format!("{d:2}"))
        .replace("%a", "Mon")
        .replace("%b", "Jan")
        .replace("%s", &secs.to_string())
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `sh`/`bash -c "…"` or a script file — run it through our own interpreter.
fn cmd_sh(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" => {
                let src = args.get(i + 1).cloned().unwrap_or_default();
                // `sh -c SCRIPT [name [args…]]`: name is $0, the rest are $1+
                let extra = args.get(i + 3..).map(|s| s.to_vec()).unwrap_or_default();
                let saved = std::mem::replace(&mut interp.positional, extra);
                let code = interp.run_script_into(&src, io.out, io.err);
                interp.positional = saved;
                interp.exiting = None;
                return code;
            }
            "-o" => {
                i += 2;
            }
            s if s.starts_with('-') => {
                i += 1;
            }
            s => {
                if let Ok(src) = interp.vfs.read_string(&interp.cwd, s) {
                    let extra = args.get(i + 1..).map(|x| x.to_vec()).unwrap_or_default();
                    let saved = std::mem::replace(&mut interp.positional, extra);
                    let code = interp.run_script_into(&src, io.out, io.err);
                    interp.positional = saved;
                    interp.exiting = None;
                    return code;
                }
                crate::commands::util::ewln(io.err, &format!("{}: {}: No such file or directory", args[0], s));
                return 127;
            }
        }
    }
    // no -c and no file → run stdin as a script
    let src = String::from_utf8_lossy(&io.stdin).into_owned();
    let code = interp.run_script_into(&src, io.out, io.err);
    interp.exiting = None;
    code
}

/// `uv` / `uvx` / `uv run` / `uv tool run`: we don't install anything, but we DO route an
/// embedded `pytest`/`python` invocation to our engines so verifiers that launch pytest via
/// uv still run. Everything else (pip install, venv, …) is a successful no-op.
fn cmd_uv(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    if let Some(pos) = args.iter().position(|a| a == "pytest" || a.ends_with("/pytest")) {
        return crate::python::run_pytest(interp, &args[pos + 1..], io.out, io.err);
    }
    if let Some(pos) = args.iter().position(|a| a == "python" || a == "python3") {
        let mut argv = vec![args[pos].clone()];
        argv.extend(args[pos + 1..].iter().cloned());
        let stdin = std::mem::take(&mut io.stdin);
        return crate::python::run_python(interp, &argv, stdin, io.out, io.err);
    }
    // record the unrouted uv invocation as unsupported (preserves legacy behavior)
    interp.note_unsupported(&args[0]);
    0
}

fn cmd_python3(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    python_impl(interp, "python3", args, io)
}
fn cmd_python(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    python_impl(interp, "python", args, io)
}
fn cmd_python311(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    python_impl(interp, "python3.11", args, io)
}
fn cmd_python312(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    python_impl(interp, "python3.12", args, io)
}
fn cmd_python313(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    python_impl(interp, "python3.13", args, io)
}

fn python_impl(interp: &mut Interp, name: &str, args: &[String], io: &mut Io) -> i32 {
    // run_python expects argv[0] to be the program name (it skips it).
    let mut argv = vec![name.to_string()];
    argv.extend(args.iter().cloned());
    let stdin = std::mem::take(&mut io.stdin);
    crate::python::run_python(interp, &argv, stdin, io.out, io.err)
}

fn cmd_jq(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    let stdin = std::mem::take(&mut io.stdin);
    crate::jqcmd::jq(interp, args, stdin, io.out, io.err)
}

fn cmd_pytest(interp: &mut Interp, args: &[String], io: &mut Io) -> i32 {
    crate::python::run_pytest(interp, args, io.out, io.err)
}
