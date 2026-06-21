//! Interpreter state shared across the shell executor and all commands.

use std::collections::HashMap;

use crate::clock::Clock;
use crate::net::VirtualNet;
use crate::vfs::Vfs;

/// A simulated background job (started with `&`). Because there is no real concurrency,
/// a job is just a captured AST that will be run-to-completion when the scheduler decides
/// to (e.g. on `wait`, or when the foreground needs its effects). For most tasks the body
/// is run immediately and synchronously, which is indistinguishable for file-state checks.
pub struct Job {
    pub id: u32,
    pub cmd: String,
    pub done: bool,
    pub status: i32,
}

pub struct Interp {
    pub vfs: Vfs,
    pub clock: Clock,
    pub net: VirtualNet,
    /// shell + environment variables (we don't distinguish exported vs not for simplicity,
    /// except that `env`/child python only sees exported ones, tracked in `exported`)
    pub vars: HashMap<String, String>,
    pub exported: std::collections::BTreeSet<String>,
    pub cwd: String,
    pub funcs: HashMap<String, crate::shell::Node>,
    /// `$?`
    pub last_status: i32,
    /// positional parameters `$1 $2 ... $@`
    pub positional: Vec<String>,
    /// `set -e` / `set -u` / `set -x`
    pub opt_errexit: bool,
    pub opt_nounset: bool,
    pub opt_xtrace: bool,
    /// `set -o pipefail`
    pub opt_pipefail: bool,
    pub jobs: Vec<Job>,
    next_job_id: u32,
    /// recursion / loop-control signaling
    pub loop_break: u32,
    pub loop_continue: u32,
    pub returning: Option<i32>,
    pub exiting: Option<i32>,
    /// depth of "condition" contexts (if/while/&&/||/!) where `set -e` is suppressed
    pub cond_depth: u32,
    /// effective uid (for permission-ish checks / `id`)
    pub uid: u32,
    /// persistent input stream + cursor for `read` inside `while read…; done < file`
    pub input_stream: Vec<u8>,
    pub input_pos: usize,
    /// trace of every external command name executed (telemetry for "what did we cover")
    pub cmd_trace: Vec<String>,
    /// commands requested that we don't implement (coverage gaps)
    pub unsupported: Vec<String>,
}

impl Interp {
    pub fn new() -> Self {
        let mut vars = HashMap::new();
        vars.insert("HOME".into(), "/root".into());
        vars.insert("PATH".into(), "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
        vars.insert("PWD".into(), "/".into());
        vars.insert("SHELL".into(), "/bin/bash".into());
        vars.insert("TERM".into(), "xterm-256color".into());
        vars.insert("USER".into(), "root".into());
        vars.insert("HOSTNAME".into(), "sandbox".into());
        vars.insert("LANG".into(), "C.UTF-8".into());
        vars.insert("IFS".into(), " \t\n".into());
        let mut exported = std::collections::BTreeSet::new();
        for k in ["HOME", "PATH", "PWD", "SHELL", "TERM", "USER", "HOSTNAME", "LANG"] {
            exported.insert(k.to_string());
        }
        Interp {
            vfs: Vfs::new(),
            clock: Clock::new(),
            net: VirtualNet::new(),
            vars,
            exported,
            cwd: "/".to_string(),
            funcs: HashMap::new(),
            last_status: 0,
            positional: Vec::new(),
            opt_errexit: false,
            opt_nounset: false,
            opt_xtrace: false,
            opt_pipefail: false,
            jobs: Vec::new(),
            next_job_id: 1,
            loop_break: 0,
            loop_continue: 0,
            returning: None,
            exiting: None,
            cond_depth: 0,
            uid: 0,
            input_stream: Vec::new(),
            input_pos: 0,
            cmd_trace: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    pub fn get_var(&self, name: &str) -> Option<String> {
        match name {
            "?" => Some(self.last_status.to_string()),
            "$" => Some("1234".to_string()), // deterministic fake PID
            "#" => Some(self.positional.len().to_string()),
            "PWD" => Some(self.cwd.clone()),
            "@" | "*" => Some(self.positional.join(" ")),
            _ => {
                if let Ok(n) = name.parse::<usize>() {
                    if n == 0 {
                        return Some("shellsim".to_string());
                    }
                    return self.positional.get(n - 1).cloned();
                }
                self.vars.get(name).cloned()
            }
        }
    }

    pub fn set_var(&mut self, name: &str, val: impl Into<String>) {
        let val = val.into();
        if name == "PWD" {
            self.cwd = val.clone();
        }
        self.vars.insert(name.to_string(), val);
    }

    pub fn export(&mut self, name: &str) {
        self.exported.insert(name.to_string());
    }

    /// Environment map visible to a child process (e.g. the python engine).
    pub fn child_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();
        for k in &self.exported {
            if let Some(v) = self.vars.get(k) {
                env.insert(k.clone(), v.clone());
            }
        }
        env.insert("PWD".to_string(), self.cwd.clone());
        env
    }

    pub fn new_job(&mut self, cmd: String) -> u32 {
        let id = self.next_job_id;
        self.next_job_id += 1;
        self.jobs.push(Job { id, cmd, done: false, status: 0 });
        id
    }

    pub fn note_unsupported(&mut self, what: &str) {
        self.unsupported.push(what.to_string());
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}
