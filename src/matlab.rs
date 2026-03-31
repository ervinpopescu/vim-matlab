//! MATLAB PTY management.
//!
//! Spawns MATLAB inside a pseudo-terminal so that its interactive I/O
//! (prompts, tab-completion, signal handling) works correctly, then
//! exposes helpers to send code and signals to the child process.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use nix::libc;
use nix::pty::{Winsize, openpty};
use nix::sys::signal::{self, Signal};
use nix::unistd::{ForkResult, Pid, close, dup2, execvp, fork, setsid, write};

/// Handle to a MATLAB process running inside a PTY.
pub struct Matlab {
    master: OwnedFd,
    child_pid: Pid,
}

impl Matlab {
    /// Spawn MATLAB (or any command) inside a new PTY.
    ///
    /// `matlab_cmd` is executed via `/bin/bash -c <cmd>` so that shell
    /// features like `PATH` lookup work as expected.
    ///
    /// # Errors
    ///
    /// Returns an error if the PTY cannot be opened or the fork/exec fails.
    pub fn spawn(matlab_cmd: &str) -> io::Result<Self> {
        // Set a reasonable default terminal size — MATLAB may hang without one.
        let ws = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&ws), None).map_err(nix_to_io)?;

        // Prepare the argv for execvp *before* forking. Allocations after
        // fork() in the child are technically undefined behaviour in the
        // presence of threads, but we are single-threaded at this point and
        // the allocations below happen before the fork.
        let bash = CString::new("/bin/bash").expect("/bin/bash is a valid CString");
        let flag = CString::new("-c").expect("-c is a valid CString");
        let cmd = CString::new(matlab_cmd).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("matlab_cmd contains a null byte: {e}"),
            )
        })?;

        // SAFETY: We call `fork()` here, which is unsafe because the child
        // shares the parent's address space until it calls `exec`. This is
        // safe because:
        //   1. We are single-threaded at the point of the fork (Tokio
        //      runtime has not been started yet).
        //   2. The child immediately configures its file descriptors, calls
        //      `setsid`, and then `execvp` — it never returns into Rust
        //      code on success.
        //   3. On exec failure the child calls `std::process::exit` to
        //      avoid running destructors in the forked address space.
        let fork_result = unsafe { fork() }.map_err(nix_to_io)?;

        match fork_result {
            ForkResult::Child => {
                // --- child process ---
                // Close the master side; the child only needs the slave.
                let _ = close(pty.master.as_raw_fd());

                // Create a new session so the slave PTY becomes the
                // controlling terminal.
                let _ = setsid();

                // Set the slave PTY as the controlling terminal.
                unsafe { libc::ioctl(pty.slave.as_raw_fd(), libc::TIOCSCTTY, 0) };

                // Redirect stdin/stdout/stderr to the slave PTY.
                let slave_fd = pty.slave.as_raw_fd();
                let _ = dup2(slave_fd, libc::STDIN_FILENO);
                let _ = dup2(slave_fd, libc::STDOUT_FILENO);
                let _ = dup2(slave_fd, libc::STDERR_FILENO);
                if slave_fd > libc::STDERR_FILENO {
                    let _ = close(slave_fd);
                }

                // Replace the process image with bash running the command.
                // execvp only returns on failure.
                let _ = execvp(&bash, &[&bash, &flag, &cmd]);

                // If exec failed, exit immediately — do NOT unwind.
                std::process::exit(127);
            }
            ForkResult::Parent { child } => {
                // Close the slave side in the parent; we only need the
                // master for I/O with the child.
                drop(pty.slave);

                // Convert the master fd to an OwnedFd so Rust manages its
                // lifetime.  The raw fd from openpty is valid and we are
                // the sole owner after closing the slave.
                let master = unsafe { OwnedFd::from_raw_fd(pty.master.as_raw_fd()) };

                // Prevent the nix OwnedFd from closing the same fd when
                // it goes out of scope — we have transferred ownership.
                std::mem::forget(pty.master);

                Ok(Self {
                    master,
                    child_pid: child,
                })
            }
        }
    }

    /// Write `code` followed by a newline to the PTY master, which MATLAB
    /// reads as interactive input.
    pub fn send_code(&self, code: &str) -> io::Result<()> {
        let mut buf = code.to_owned();
        buf.push('\n');
        let mut remaining = buf.as_bytes();
        while !remaining.is_empty() {
            let n = write(&self.master, remaining).map_err(nix_to_io)?;
            remaining = &remaining[n..];
        }
        Ok(())
    }

    /// Send `SIGINT` to the child process (equivalent to Ctrl-C).
    pub fn send_sigint(&self) {
        let _ = signal::kill(self.child_pid, Signal::SIGINT);
    }

    /// Send `SIGTERM` to the child process for a graceful shutdown.
    pub fn send_sigterm(&self) {
        let _ = signal::kill(self.child_pid, Signal::SIGTERM);
    }

    /// Kill the entire process group/session associated with the child.
    /// This ensures background processes like xwayland-satellite are cleaned up.
    pub fn kill_session(&self) {
        // Kill the process group (using negative PID)
        let _ = signal::kill(Pid::from_raw(-self.child_pid.as_raw()), Signal::SIGTERM);
    }

    /// Return a reference to the PTY master file descriptor.
    ///
    /// Callers can use this to read MATLAB's output (e.g. via
    /// `tokio::io::AsyncFd` or a blocking read loop).
    pub fn master_fd(&self) -> &OwnedFd {
        &self.master
    }

    /// Return the PID of the child process.
    pub fn child_pid(&self) -> Pid {
        self.child_pid
    }
}

/// Convert a `nix::Error` into a `std::io::Error`.
fn nix_to_io(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}
