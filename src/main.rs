mod matlab;
mod protocol;
mod server;

use std::io;
use std::os::fd::{AsRawFd, FromRawFd};

use clap::Parser;
use nix::sys::termios;

/// Read from a raw fd into `buf`. Returns the number of bytes read, or
/// an error. An `EIO` result is mapped to `Ok(0)` (EOF on PTY).
fn raw_read(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EIO) {
            return Ok(0);
        }
        return Err(err);
    }
    Ok(n as usize)
}

/// Write all bytes in `buf` to a raw fd.
fn raw_write_all(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// vim-matlab server — spawns MATLAB in a PTY and listens for commands
/// from Neovim over a Unix socket.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Command used to launch MATLAB.
    #[arg(
        long,
        default_value = "setsid xwayland-satellite :1 >/dev/null 2>&1 & for i in {1..50}; do xset -display :1 q >/dev/null 2>&1 && break || sleep 0.1; done; export DISPLAY=:1; xrdb -load ~/.config/X11/Xresources; QT_QPA_PLATFORM=xcb LD_PRELOAD=/usr/lib/libstdc++.so:/usr/lib/libfreetype.so LD_LIBRARY_PATH=/usr/lib/dri/ matlab -nodesktop -nosplash -webui"
    )]
    matlab_cmd: String,

    /// Path to the Unix domain socket.
    ///
    /// The literal `{uid}` is replaced with the current user's UID.
    #[arg(long, default_value = "/tmp/vim-matlab-{uid}.sock")]
    socket: String,
}

macro_rules! log_raw {
    ($($arg:tt)*) => {
        eprintln!("\r{}", format_args!($($arg)*))
    };
}

fn main() -> io::Result<()> {
    let mut args = Args::parse();

    // Substitute {uid} with the real UID so each user gets their own socket.
    if args.socket.contains("{uid}") {
        let uid = nix::unistd::getuid();
        args.socket = args.socket.replace("{uid}", &uid.to_string());
    }

    log_raw!("matlab_cmd: {}", args.matlab_cmd);
    log_raw!("socket:     {}", args.socket);

    // Spawn MATLAB *before* starting the Tokio runtime.
    // fork() must happen while we are single-threaded.
    let matlab = matlab::Matlab::spawn(&args.matlab_cmd)?;
    log_raw!("MATLAB spawned (pid {})", matlab.child_pid());

    // Put the parent's stdin into raw mode so PTY forwarding works
    // correctly (no double echo, no line buffering).
    let stdin_handle = io::stdin();
    let orig_termios = termios::tcgetattr(&stdin_handle).ok();
    if let Some(ref orig) = orig_termios {
        let mut raw = orig.clone();
        termios::cfmakeraw(&mut raw);
        termios::tcsetattr(&stdin_handle, termios::SetArg::TCSANOW, &raw).ok();
    }

    // Grab the raw fd for the PTY master so we can pass it into
    // blocking tasks (raw fds are Send, OwnedFd is not).
    let master_raw_fd = matlab.master_fd().as_raw_fd();

    // Build and enter the Tokio runtime manually so that `matlab`
    // (which is !Send) stays on the main thread.
    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt, unix::AsyncFd};
        use tokio::sync::oneshot;

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (shutdown_tx2, shutdown_rx2) = oneshot::channel();

        // Wrap the master FD for async I/O.
        let master_fd =
            unsafe { std::os::unix::io::OwnedFd::from_raw_fd(libc::dup(master_raw_fd)) };
        let master_async = AsyncFd::new(master_fd).expect("Failed to create AsyncFd");

        // Forward MATLAB PTY output → stdout.
        let mut stdout = tokio::io::stdout();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let mut guard = match master_async.readable().await {
                    Ok(g) => g,
                    Err(_) => break,
                };

                match raw_read(guard.get_inner().as_raw_fd(), &mut buf) {
                    Ok(0) => {
                        let _ = shutdown_tx.send(());
                        break;
                    }
                    Ok(n) => {
                        guard.clear_ready();
                        if stdout.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        let _ = stdout.flush().await;
                    }
                    Err(e) => {
                        if e.kind() != io::ErrorKind::WouldBlock {
                            log_raw!("[pty->stdout] read error: {e}");
                            break;
                        }
                    }
                }
            }
        });

        // Forward stdin → MATLAB PTY.
        let mut stdin = tokio::io::stdin();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let n = match stdin.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                if raw_write_all(master_raw_fd, &buf[..n]).is_err() {
                    break;
                }
            }
        });

        // Listen for signals to ensure we can exit even in raw mode.
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigint = signal(SignalKind::interrupt()).unwrap();
            let mut sigterm = signal(SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = sigint.recv() => log_raw!("[server] caught SIGINT"),
                _ = sigterm.recv() => log_raw!("[server] caught SIGTERM"),
            }
            let _ = shutdown_tx2.send(());
        });

        // Run the Unix socket server.
        tokio::select! {
            _ = server::run(&args.socket, &matlab) => {},
            _ = shutdown_rx => log_raw!("[server] MATLAB exited, shutting down"),
            _ = shutdown_rx2 => log_raw!("[server] signal received, shutting down"),
        }

        // Cleanup: ensure the child process (and its group) is dead.
        matlab.kill_session();
    });

    // Restore original terminal settings.
    if let Some(ref orig) = orig_termios {
        termios::tcsetattr(&stdin_handle, termios::SetArg::TCSANOW, orig).ok();
    }

    std::process::exit(0);
}
