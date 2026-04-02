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
use nix::unistd::{ForkResult, Pid, execvp, fork, setsid, write};

/// Interface for MATLAB process actions.
pub trait MatlabActions {
    fn send_code(&self, code: &str) -> io::Result<()>;
    fn send_sigint(&self);
    fn send_sigterm(&self);
}

/// Handle to a MATLAB process running inside a PTY.
#[derive(Debug)]
pub struct Matlab {
    master: OwnedFd,
    child_pid: Pid,
}

impl MatlabActions for Matlab {
    /// Write `code` followed by a newline to the PTY master, which MATLAB
    /// reads as interactive input.
    fn send_code(&self, code: &str) -> io::Result<()> {
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
    fn send_sigint(&self) {
        let _ = signal::kill(self.child_pid, Signal::SIGINT);
    }

    /// Send `SIGTERM` to the child process for a graceful shutdown.
    fn send_sigterm(&self) {
        let _ = signal::kill(self.child_pid, Signal::SIGTERM);
    }
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
                // SAFETY: Raw syscalls in the child after fork are safe as they are
                // async-signal-safe and we are about to call exec.
                unsafe { libc::close(pty.master.as_raw_fd()) };

                // Create a new session so the slave PTY becomes the
                // controlling terminal.
                let _ = setsid();

                // Set the slave PTY as the controlling terminal.
                // SAFETY: TIOCSCTTY is a standard ioctl to set the controlling terminal.
                unsafe { libc::ioctl(pty.slave.as_raw_fd(), libc::TIOCSCTTY, 0) };

                // Redirect stdin/stdout/stderr to the slave PTY.
                let slave_fd = pty.slave.as_raw_fd();
                // SAFETY: These libc calls are safe in the child after fork. Using direct libc
                // avoids Rust's RAII/Drop semantics which might be unsafe in a forked child.
                unsafe {
                    libc::dup2(slave_fd, libc::STDIN_FILENO);
                    libc::dup2(slave_fd, libc::STDOUT_FILENO);
                    libc::dup2(slave_fd, libc::STDERR_FILENO);
                    if slave_fd > libc::STDERR_FILENO {
                        libc::close(slave_fd);
                    }
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

                // Set non-blocking mode for the master so it works correctly with AsyncFd.
                let flags =
                    nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_GETFL).map_err(nix_to_io)?;
                let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
                flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                nix::fcntl::fcntl(&master, nix::fcntl::FcntlArg::F_SETFL(flags))
                    .map_err(nix_to_io)?;

                Ok(Self {
                    master,
                    child_pid: child,
                })
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Read all available bytes from a non-blocking fd, retrying on EAGAIN.
    fn drain_nonblocking(fd: i32) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..40 {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize]);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        out
    }

    // ── spawn error paths ─────────────────────────────────────────────────────

    #[test]
    fn test_spawn_rejects_null_byte() {
        // CString::new fails before fork() is ever called.
        let err = Matlab::spawn("echo\0hello").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    // ── fd validity and flags ─────────────────────────────────────────────────

    #[test]
    fn test_master_fd_is_valid() {
        let matlab = Matlab::spawn("cat").unwrap();
        // A valid fd can be dup'd; dup returns -1 on invalid fd.
        let dup_fd = unsafe { libc::dup(matlab.master_fd().as_raw_fd()) };
        assert!(dup_fd >= 0, "master fd should be valid");
        unsafe { libc::close(dup_fd) };
    }

    #[test]
    fn test_master_fd_is_nonblocking() {
        let matlab = Matlab::spawn("cat").unwrap();
        let flags = nix::fcntl::fcntl(matlab.master_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
        let oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
        assert!(
            oflags.contains(nix::fcntl::OFlag::O_NONBLOCK),
            "master fd must have O_NONBLOCK set for AsyncFd"
        );
    }

    #[test]
    fn test_pty_window_size() {
        let matlab = Matlab::spawn("cat").unwrap();
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let ret = unsafe { libc::ioctl(matlab.master_fd().as_raw_fd(), libc::TIOCGWINSZ, &mut ws) };
        assert_eq!(ret, 0, "TIOCGWINSZ should succeed");
        assert_eq!(ws.ws_row, 24);
        assert_eq!(ws.ws_col, 80);
    }

    #[test]
    fn test_child_pid_is_positive() {
        let matlab = Matlab::spawn("cat").unwrap();
        assert!(matlab.child_pid().as_raw() > 0);
    }

    // ── send_code ─────────────────────────────────────────────────────────────

    #[test]
    fn test_send_code_empty_string() {
        // Empty string sends only "\n"; should not error or hang.
        let matlab = Matlab::spawn("cat").unwrap();
        matlab.send_code("").unwrap();
        let bytes = drain_nonblocking(matlab.master_fd().as_raw_fd());
        assert!(bytes.contains(&b'\n'));
    }

    #[test]
    fn test_send_code_unicode() {
        // Multi-byte UTF-8 must be written completely without slicing mid-char.
        let matlab = Matlab::spawn("cat").unwrap();
        matlab.send_code("café").unwrap();
        let bytes = drain_nonblocking(matlab.master_fd().as_raw_fd());
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("caf\u{e9}") || s.contains("café"));
    }

    #[test]
    fn test_nix_to_io() {
        let err = nix::Error::from_raw(libc::EIO);
        let io_err = nix_to_io(err);
        assert_eq!(io_err.raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn test_matlab_spawn_and_actions() {
        // Use 'cat' as a dummy MATLAB. It will echo everything back.
        let matlab = Matlab::spawn("cat").expect("Failed to spawn cat");
        assert!(matlab.child_pid().as_raw() > 0);

        // Test send_code
        matlab.send_code("hello world").unwrap();

        // Give it a moment to echo
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Read from master to verify the echo. Master is non-blocking now.
        let mut master =
            unsafe { std::fs::File::from_raw_fd(libc::dup(matlab.master_fd().as_raw_fd())) };
        let mut buf = [0u8; 1024];
        let mut total_read = 0;

        // Try reading in a loop to handle EAGAIN
        for _ in 0..20 {
            match master.read(&mut buf) {
                Ok(n) if n > 0 => {
                    total_read += n;
                    break;
                }
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    continue;
                }
                Err(_) => break,
            }
        }
        assert!(total_read > 0);

        // Test signals
        matlab.send_sigint();
        matlab.send_sigterm();
        matlab.kill_session();
    }

    #[tokio::test]
    async fn test_matlab_async_read() {
        let matlab = Matlab::spawn("echo 'test output'").expect("Failed to spawn echo");
        let master_fd = unsafe { OwnedFd::from_raw_fd(libc::dup(matlab.master_fd().as_raw_fd())) };
        let async_fd = tokio::io::unix::AsyncFd::new(master_fd).unwrap();

        let mut buf = [0u8; 1024];
        let mut total_read = 0;

        // Try reading for a bit
        for _ in 0..20 {
            let mut guard = match tokio::time::timeout(
                std::time::Duration::from_millis(200),
                async_fd.readable(),
            )
            .await
            {
                Ok(Ok(g)) => g,
                _ => break,
            };

            let n = unsafe {
                libc::read(
                    guard.get_inner().as_raw_fd(),
                    buf.as_mut_ptr().add(total_read).cast(),
                    buf.len() - total_read,
                )
            };

            if n <= 0 {
                break;
            }
            total_read += n as usize;
            guard.clear_ready();
        }

        let output = String::from_utf8_lossy(&buf[..total_read]);
        assert!(output.contains("test output") || output.contains("echo"));
    }
}

/// Convert a `nix::Error` into a `std::io::Error`.
fn nix_to_io(e: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}
