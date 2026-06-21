//! Package managers and build tools that are out of scope for the sandbox. They are
//! registered as [`Trust::NoOp`]: the dispatcher records them as unsupported and they
//! pretend success so environment-build scripts proceed (the real work is absent).

use std::collections::HashMap;

use crate::commands::{CommandSpec, Io, Trust};
use crate::interp::Interp;

pub fn register(m: &mut HashMap<&'static str, CommandSpec>) {
    use super::reg;
    reg(
        m,
        &[
            "pip", "pip3", "apt", "apt-get", "npm", "node", "cargo", "make", "gcc", "g++", "cc",
            "clang", "mvn", "gradle", "javac", "java", "docker", "systemctl", "service",
            "uvicorn", "gunicorn", "flask",
        ],
        Trust::NoOp,
        cmd_noop,
    );
}

/// Pretend success; the dispatcher already recorded the command as unsupported.
fn cmd_noop(_interp: &mut Interp, _args: &[String], _io: &mut Io) -> i32 {
    0
}
