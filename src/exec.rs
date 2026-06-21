//! The shell executor: walks the AST against the interpreter state.

use crate::expand::{expand_word, expand_words};
use crate::interp::Interp;
use crate::shell::{Node, RedirOp, Redirect};

/// Execute a node. `stdin` is the input byte stream (from a pipe or empty). Command output
/// is appended to `out`/`err` unless redirected. Returns the exit status.
pub fn exec(interp: &mut Interp, node: &Node, stdin: Vec<u8>, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
    if interp.exiting.is_some() || interp.returning.is_some() || interp.loop_break > 0 || interp.loop_continue > 0 {
        return interp.last_status;
    }
    let status = match node {
        Node::Empty => 0,
        Node::Command { .. } => exec_command(interp, node, stdin, out, err),
        Node::Pipeline(stages) => exec_pipeline(interp, stages, stdin, out, err),
        Node::And(a, b) => {
            let sa = exec_cond(interp, a, stdin.clone(), out, err);
            if sa == 0 && interp.exiting.is_none() {
                exec(interp, b, stdin, out, err)
            } else {
                sa
            }
        }
        Node::Or(a, b) => {
            let sa = exec_cond(interp, a, stdin.clone(), out, err);
            if sa != 0 && interp.exiting.is_none() {
                exec(interp, b, stdin, out, err)
            } else {
                sa
            }
        }
        Node::Not(a) => {
            let sa = exec_cond(interp, a, stdin, out, err);
            if sa == 0 {
                1
            } else {
                0
            }
        }
        Node::Seq(nodes) => {
            let mut s = 0;
            for n in nodes {
                s = exec(interp, n, stdin.clone(), out, err);
                if interp.exiting.is_some() || interp.returning.is_some() {
                    break;
                }
                if interp.loop_break > 0 || interp.loop_continue > 0 {
                    break;
                }
                if s != 0 && interp.opt_errexit && interp.cond_depth == 0 {
                    interp.exiting = Some(s);
                    break;
                }
            }
            s
        }
        Node::Background(a) => {
            let cmd = describe(a);
            let id = interp.new_job(cmd);
            // Run synchronously (deterministic); file effects land immediately. `wait` is a no-op.
            let mut bout = Vec::new();
            let mut berr = Vec::new();
            let s = exec(interp, a, Vec::new(), &mut bout, &mut berr);
            if let Some(j) = interp.jobs.iter_mut().find(|j| j.id == id) {
                j.done = true;
                j.status = s;
            }
            out.extend(bout);
            err.extend(berr);
            interp.set_var("!", id.to_string());
            0
        }
        Node::Subshell(a) | Node::Group(a) => {
            // a real subshell would isolate env; we approximate Group exactly and Subshell
            // closely enough for file-state tasks (cwd is saved/restored for subshells).
            let saved_cwd = interp.cwd.clone();
            let s = exec(interp, a, stdin, out, err);
            if matches!(node, Node::Subshell(_)) {
                interp.cwd = saved_cwd;
            }
            s
        }
        Node::Redirected(inner, redirs) => {
            let mut local_out = Vec::new();
            let mut local_err = Vec::new();
            let plan = plan_redirects(interp, redirs);
            // Make a `< file` input available to `read` across loop iterations via a cursor.
            let has_stdin = redirs.iter().any(|r| r.fd == 0);
            let saved = if has_stdin {
                let s = (std::mem::take(&mut interp.input_stream), interp.input_pos);
                interp.input_stream = plan.stdin.clone();
                interp.input_pos = 0;
                Some(s)
            } else {
                None
            };
            let s = exec(interp, inner, plan.stdin.clone(), &mut local_out, &mut local_err);
            if let Some((stream, pos)) = saved {
                interp.input_stream = stream;
                interp.input_pos = pos;
            }
            apply_outputs(interp, &plan, &local_out, &local_err, out, err);
            s
        }
        Node::If { cond, then, elifs, els } => {
            if exec_cond(interp, cond, stdin.clone(), out, err) == 0 {
                exec(interp, then, stdin, out, err)
            } else {
                for (c, b) in elifs {
                    if exec_cond(interp, c, stdin.clone(), out, err) == 0 {
                        return exec(interp, b, stdin, out, err);
                    }
                }
                if let Some(e) = els {
                    exec(interp, e, stdin, out, err)
                } else {
                    0
                }
            }
        }
        Node::While { cond, body, until } => {
            let mut s = 0;
            let mut guard = 0;
            loop {
                guard += 1;
                // bound runaway poll loops (e.g. `until curl ...; do sleep; done` against a
                // service we don't simulate). Real task loops never need this many iterations.
                if guard > 5_000 {
                    break;
                }
                let c = exec_cond(interp, cond, Vec::new(), out, err);
                let go = if *until { c != 0 } else { c == 0 };
                if !go || interp.exiting.is_some() {
                    break;
                }
                s = exec(interp, body, Vec::new(), out, err);
                if interp.loop_break > 0 {
                    interp.loop_break -= 1;
                    break;
                }
                if interp.loop_continue > 0 {
                    interp.loop_continue -= 1;
                    continue;
                }
                if interp.exiting.is_some() || interp.returning.is_some() {
                    break;
                }
            }
            s
        }
        Node::For { var, words, body } => {
            let items = expand_words(interp, words);
            let mut s = 0;
            for item in items {
                interp.set_var(var, item);
                s = exec(interp, body, Vec::new(), out, err);
                if interp.loop_break > 0 {
                    interp.loop_break -= 1;
                    break;
                }
                if interp.loop_continue > 0 {
                    interp.loop_continue -= 1;
                    continue;
                }
                if interp.exiting.is_some() || interp.returning.is_some() {
                    break;
                }
            }
            s
        }
        Node::Case { word, arms } => {
            let subject = expand_word(interp, word, false).join(" ");
            for (pats, body) in arms {
                for pat in pats {
                    let p = expand_word(interp, pat, false).join(" ");
                    if case_match(&p, &subject) {
                        return exec(interp, body, stdin, out, err);
                    }
                }
            }
            0
        }
        Node::FuncDef { name, body } => {
            interp.funcs.insert(name.clone(), (**body).clone());
            0
        }
    };
    interp.last_status = status;
    status
}

/// Execute as a condition: errexit is suppressed inside.
fn exec_cond(interp: &mut Interp, node: &Node, stdin: Vec<u8>, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
    interp.cond_depth += 1;
    let s = exec(interp, node, stdin, out, err);
    interp.cond_depth -= 1;
    s
}

fn describe(node: &Node) -> String {
    match node {
        Node::Command { words, .. } => words.join(" "),
        _ => "<job>".to_string(),
    }
}

struct RedirPlan {
    stdin: Vec<u8>,
    /// final destination for fd1 and fd2: None=parent buffer, Some((path,append))=file
    out_dest: OutDest,
    err_dest: OutDest,
}

#[derive(Clone)]
enum OutDest {
    Parent,
    File(String, bool), // (path, append)
}

fn plan_redirects(interp: &mut Interp, redirs: &[Redirect]) -> RedirPlan {
    let mut stdin = Vec::new();
    let mut out_dest = OutDest::Parent;
    let mut err_dest = OutDest::Parent;
    for r in redirs {
        match r.op {
            RedirOp::Read => {
                let path = expand_word(interp, &r.target, true).join(" ");
                stdin = interp.vfs.read(&interp.cwd, &path).unwrap_or_default();
            }
            RedirOp::Heredoc => {
                // expand $, command subs in body
                let expanded = expand_heredoc(interp, &r.target);
                stdin = expanded.into_bytes();
            }
            RedirOp::HeredocRaw => {
                stdin = r.target.clone().into_bytes();
            }
            RedirOp::Write | RedirOp::Append => {
                let path = expand_word(interp, &r.target, true).join(" ");
                let dest = OutDest::File(path, r.op == RedirOp::Append);
                if r.fd == 2 {
                    err_dest = dest;
                } else {
                    out_dest = dest;
                }
            }
            RedirOp::DupOut => {
                // &N : duplicate target fd's destination
                let tgt = r.target.trim_start_matches('&');
                let src = tgt.parse::<i32>().unwrap_or(1);
                let dup = if src == 1 { out_dest.clone() } else { err_dest.clone() };
                if r.fd == 2 {
                    err_dest = dup;
                } else {
                    out_dest = dup;
                }
            }
        }
    }
    RedirPlan { stdin, out_dest, err_dest }
}

fn apply_outputs(
    interp: &mut Interp,
    plan: &RedirPlan,
    local_out: &[u8],
    local_err: &[u8],
    out: &mut Vec<u8>,
    err: &mut Vec<u8>,
) {
    match &plan.out_dest {
        OutDest::Parent => out.extend_from_slice(local_out),
        OutDest::File(p, append) => write_to(interp, p, local_out, *append),
    }
    match &plan.err_dest {
        OutDest::Parent => err.extend_from_slice(local_err),
        OutDest::File(p, append) => write_to(interp, p, local_err, *append),
    }
}

fn write_to(interp: &mut Interp, path: &str, data: &[u8], append: bool) {
    if path == "/dev/null" {
        return;
    }
    if path == "/dev/stdout" {
        return;
    }
    let cwd = interp.cwd.clone();
    let r = if append {
        interp.vfs.append(&cwd, path, data, 0o644)
    } else {
        interp.vfs.write(&cwd, path, data, 0o644)
    };
    if r.is_err() {
        // surface nothing; the command's status already reflects success
    }
}

fn exec_command(interp: &mut Interp, node: &Node, stdin: Vec<u8>, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
    let (assigns, words, redirects) = match node {
        Node::Command { assigns, words, redirects } => (assigns, words, redirects),
        _ => unreachable!(),
    };

    // Expand assignment values.
    let expanded_assigns: Vec<(String, String)> = assigns
        .iter()
        .map(|(k, v)| (k.clone(), expand_word(interp, v, false).join(" ")))
        .collect();

    // No command words → assignments are persistent.
    let argv = expand_words(interp, words);
    if argv.is_empty() {
        for (k, v) in expanded_assigns {
            interp.set_var(&k, v);
        }
        // a bare redirection like `> file` still truncates
        if !redirects.is_empty() {
            let plan = plan_redirects(interp, redirects);
            apply_outputs(interp, &plan, &[], &[], out, err);
        }
        return 0;
    }

    // Set up redirects.
    let plan = plan_redirects(interp, redirects);
    let cmd_stdin = if redirects.iter().any(|r| r.fd == 0) { plan.stdin.clone() } else { stdin };

    // Temporary assignments apply only for the duration of this command (we set then restore).
    let saved: Vec<(String, Option<String>)> = expanded_assigns
        .iter()
        .map(|(k, _)| (k.clone(), interp.vars.get(k).cloned()))
        .collect();
    for (k, v) in &expanded_assigns {
        interp.set_var(k, v.clone());
        interp.export(k); // exported to child for the command
    }

    let mut local_out = Vec::new();
    let mut local_err = Vec::new();

    interp.cmd_trace.push(argv[0].clone());

    let status = if let Some(body) = interp.funcs.get(&argv[0]).cloned() {
        // function call: set positional params
        let saved_pos = std::mem::replace(&mut interp.positional, argv[1..].to_vec());
        let s = exec(interp, &body, cmd_stdin, &mut local_out, &mut local_err);
        interp.positional = saved_pos;
        interp.returning.take().unwrap_or(s)
    } else {
        crate::commands::run(interp, &argv, cmd_stdin, &mut local_out, &mut local_err)
    };

    // restore temporary assignments
    for (k, v) in saved {
        match v {
            Some(val) => {
                interp.vars.insert(k, val);
            }
            None => {
                interp.vars.remove(&k);
            }
        }
    }

    apply_outputs(interp, &plan, &local_out, &local_err, out, err);
    status
}

fn exec_pipeline(interp: &mut Interp, stages: &[Node], stdin: Vec<u8>, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
    let mut input = stdin;
    let mut last_status = 0;
    let mut statuses = Vec::new();
    for (idx, stage) in stages.iter().enumerate() {
        let is_last = idx == stages.len() - 1;
        let mut stage_out = Vec::new();
        // stderr of all stages flows to the shared err
        last_status = exec(interp, stage, std::mem::take(&mut input), &mut stage_out, err);
        statuses.push(last_status);
        if is_last {
            out.extend_from_slice(&stage_out);
        } else {
            input = stage_out;
        }
    }
    if interp.opt_pipefail {
        statuses.into_iter().rev().find(|s| *s != 0).unwrap_or(last_status)
    } else {
        last_status
    }
}

fn expand_heredoc(interp: &mut Interp, body: &str) -> String {
    // expand $VAR, ${...}, $(...), `...` in the heredoc body, line by line
    let mut out = String::new();
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // reuse double-quote expansion semantics (no splitting/globbing)
        let parts = expand_word(interp, &double_wrap(line), false);
        out.push_str(&parts.join(" "));
    }
    out
}

/// Wrap a line so expand_word treats it as a double-quoted context (expansions, no split).
fn double_wrap(line: &str) -> String {
    // escape existing double quotes and backslashes minimally
    let escaped = line.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn case_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let re = format!("^{}$", glob_to_regex_body(pattern));
    regex::Regex::new(&re).map(|r| r.is_match(text)).unwrap_or(pattern == text)
}

fn glob_to_regex_body(pat: &str) -> String {
    let mut re = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                re.push('[');
                while let Some(&n) = chars.peek() {
                    chars.next();
                    re.push(n);
                    if n == ']' {
                        break;
                    }
                }
            }
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '\\' => {
                re.push('\\');
                re.push(c);
            }
            _ => re.push(c),
        }
    }
    re
}
