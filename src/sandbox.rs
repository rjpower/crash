//! Process-level syscall sandbox.
//!
//! RustPython is memory-safe but **not** a deny-by-default capability sandbox: it ships
//! `os`/`io`/`socket` implemented against the real OS, so Python-level tricks
//! (`os.system`, `socket.connect`, `os.open`, …) can reach the host. The pure-Python prelude
//! neutralizes the high- and low-level *file* surface cooperatively, but a cooperative patch
//! is not a hard boundary. This module installs the hard one: a `seccomp` filter applied at
//! process start that makes the kernel itself refuse the two worst escape classes —
//! **network egress** (`socket`/`connect`) and **process exec/fork** (`execve`/`fork`). No
//! Python-level trick can bypass a kernel seccomp filter.
//!
//! This is safe for shellsim because the simulator never opens sockets, execs programs, or
//! forks — it is entirely in-process with a virtual clock and virtual network. The filter is
//! **default-allow**: only the explicit denylist is blocked (returning `EPERM`), so the
//! harness's own file reads/writes are untouched. It is NOT a replacement for an OS sandbox
//! around the whole process for adversarial workloads (FS-read confinement still wants
//! landlock / a subprocess jail — see [[shellsim-sandbox-boundary]]); it is defense-in-depth
//! that closes the network/exec holes for real.
//!
//! Set `SHELLSIM_NO_SANDBOX=1` to skip it (debugging / unusual hosts).

/// Install the syscall sandbox. No-op on non-Linux and when `SHELLSIM_NO_SANDBOX` is set.
pub fn apply() {
    if std::env::var_os("SHELLSIM_NO_SANDBOX").is_some() {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = imp::install() {
            // Don't hard-fail: a host that forbids seccomp (some CI/containers) should still
            // run, just without the kernel backstop. The cooperative prelude hardening remains.
            eprintln!("shellsim: warning: seccomp sandbox not installed ({e}); relying on cooperative hardening");
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter};
    use std::collections::BTreeMap;

    pub fn install() -> Result<(), Box<dyn std::error::Error>> {
        // Syscalls present on every Linux arch we target. Blocking `socket`/`connect` kills all
        // network egress (you cannot connect without first creating a socket); blocking
        // `execve`/`execveat` means even a successful fork/posix_spawn cannot run a real program,
        // which neuters `os.system`/`subprocess`. `ptrace` is blocked so a child cannot be used
        // to poke another process.
        let mut denied: Vec<i64> = vec![
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_socketpair,
            libc::SYS_execve,
            libc::SYS_execveat,
            libc::SYS_ptrace,
        ];
        // fork/vfork don't exist as distinct syscalls on every arch (aarch64 routes through
        // clone); include them where the libc constant is defined. Denying exec already blocks
        // running new programs — this is belt-and-suspenders for raw process creation.
        #[cfg(target_arch = "x86_64")]
        {
            denied.push(libc::SYS_fork);
            denied.push(libc::SYS_vfork);
        }

        let mut rules: BTreeMap<i64, Vec<seccompiler::SeccompRule>> = BTreeMap::new();
        for s in denied {
            rules.insert(s, vec![]); // empty rule vec == match the syscall unconditionally
        }

        let arch = std::env::consts::ARCH.try_into()?;
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,                      // default for everything not listed
            SeccompAction::Errno(libc::EPERM as u32),  // listed syscalls fail with EPERM
            arch,
        )?;
        let prog: BpfProgram = filter.try_into()?;
        seccompiler::apply_filter(&prog)?;
        Ok(())
    }
}
