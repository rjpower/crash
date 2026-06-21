//! Built-in commands and coreutils, implemented natively against the VFS.
//!
//! Each command is a plain function with the uniform signature
//! [`CmdFn`] = `fn(&mut Interp, &[String], &mut Io) -> i32`, where the slice is `argv[1..]`
//! and all I/O flows through the [`Io`] context (read `io.stdin`, write `io.out` / `io.err`).
//! Builtins that must mutate interpreter state (cd, export, set, …) do so directly on `Interp`.
//!
//! Commands are looked up in a [`OnceLock`]-backed registry that records each command's
//! [`Trust`] level, so a run can report whether it stayed inside the faithfully-simulated
//! envelope. Unknown commands fall through to the VFS-script / shebang fallback.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::interp::Interp;

mod builtins;
mod fs;
mod hashing;
mod net;
mod pkg;
mod proc;
mod text;
pub mod util;

/// Bundled standard I/O for a command invocation.
pub struct Io<'a> {
    pub stdin: Vec<u8>,
    pub out: &'a mut Vec<u8>,
    pub err: &'a mut Vec<u8>,
}

/// Uniform command signature. `args` is `argv[1..]`.
pub type CmdFn = fn(&mut Interp, &[String], &mut Io) -> i32;

/// How faithfully a command is simulated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trust {
    /// Faithful implementation (coreutils / builtins we fully model).
    Real,
    /// A subset of the real behavior (jq, sed, grep, uv, …).
    Partial,
    /// Ignored / pretend-success (apt-get, pip, …).
    NoOp,
}

/// A registered command: its implementation and trust level.
pub struct CommandSpec {
    pub run: CmdFn,
    pub trust: Trust,
}

static REGISTRY: OnceLock<HashMap<&'static str, CommandSpec>> = OnceLock::new();

fn registry() -> &'static HashMap<&'static str, CommandSpec> {
    REGISTRY.get_or_init(build_registry)
}

/// Register `f` under every name in `names` with trust `t`.
fn reg(map: &mut HashMap<&'static str, CommandSpec>, names: &[&'static str], t: Trust, f: CmdFn) {
    for &n in names {
        map.insert(n, CommandSpec { run: f, trust: t });
    }
}

fn build_registry() -> HashMap<&'static str, CommandSpec> {
    let mut m = HashMap::new();
    builtins::register(&mut m);
    text::register(&mut m);
    fs::register(&mut m);
    hashing::register(&mut m);
    net::register(&mut m);
    proc::register(&mut m);
    pkg::register(&mut m);
    m
}

/// Dispatch entry point: look up `argv[0]`, record its trust, and run it.
///
/// Unknown commands fall through to the legacy fallback: try to execute a script that lives
/// in the VFS (shell or `#!`-python), otherwise record it as unsupported and return 127.
pub fn run(interp: &mut Interp, argv: &[String], stdin: Vec<u8>, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
    let cmd = argv[0].as_str();
    let args = &argv[1..];
    if let Some(spec) = registry().get(cmd) {
        match spec.trust {
            Trust::NoOp => {
                // NoOp commands (package managers / build tools) are recorded as unsupported,
                // preserving the legacy `note_unsupported(cmd)` behavior for that arm.
                interp.note_unsupported(cmd);
                interp.trust_noop.insert(cmd.to_string());
            }
            Trust::Partial => {
                interp.trust_partial.insert(cmd.to_string());
            }
            Trust::Real => {}
        }
        let mut io = Io { stdin, out, err };
        return (spec.run)(interp, args, &mut io);
    }

    // ---- fallback: maybe it's an executable script in the VFS ----
    if let Some(code) = util::try_exec_script(interp, cmd, args, &stdin, out, err) {
        return code;
    }
    interp.note_unsupported(cmd);
    util::ewln(err, &format!("{cmd}: command not found"));
    127
}

/// Provide `run_script_into` for nested execution (source, eval, scripts).
impl Interp {
    pub fn run_script_into(&mut self, src: &str, out: &mut Vec<u8>, err: &mut Vec<u8>) -> i32 {
        let ast = crate::shell::parse(src);
        let r = self.returning.take();
        let code = crate::exec::exec(self, &ast, Vec::new(), out, err);
        if r.is_some() {
            self.returning = r;
        }
        code
    }
}
