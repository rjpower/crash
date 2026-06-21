//! shellsim — a deterministic, in-memory bash-like shell simulator for agentic RL.
//!
//! The crate is organized as a small operating environment:
//!   * [`vfs`]   — in-memory filesystem (the single source of truth)
//!   * [`clock`] — virtual clock (sleep never blocks)
//!   * [`net`]   — virtual network (curl/wget against a route table)
//!   * [`interp`]— interpreter state shared by the shell and all commands
//!   * [`shell`] / [`expand`] / [`exec`] — bash-subset parser, word expansion, executor
//!   * [`commands`] — native coreutils + builtins
//!   * [`python`] — embedded Python (RustPython) wired to the VFS
//!   * [`harness`] — load & run Terminal-Bench / TBLite tasks and score them

pub mod clock;
pub mod commands;
pub mod exec;
pub mod expand;
pub mod harness;
pub mod hashes;
pub mod interp;
pub mod jqcmd;
pub mod net;
pub mod netcmd;
pub mod python;
pub mod sandbox;
pub mod shell;
pub mod vfs;

pub use interp::Interp;
