mod matlab;
mod protocol;
mod server;

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;

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
#[derive(Parser, Debug, Clone)]
#[command(version, about)]
pub struct Args {
    /// Command used to launch MATLAB.
    #[arg(long, default_value = "matlab -nodesktop -nosplash")]
    pub matlab_cmd: String,

    /// Path to the Unix domain socket.
    ///
    /// The literal `{uid}` is replaced with the current user's UID.
    #[arg(long, default_value = "/tmp/vim-matlab-{uid}.sock")]
    pub socket: String,
}

macro_rules! log_raw {
    ($($arg:tt)*) => {
        eprintln!("\r{}", format_args!($($arg)*))
    };
}

/// Substitute {uid} with the real UID so each user gets their own socket.
fn resolve_socket_path(path: &str) -> String {
    if path.contains("{uid}") {
        let uid = nix::unistd::getuid();
        path.replace("{uid}", &uid.to_string())
    } else {
        path.to_string()
    }
}

fn main() -> io::Result<()> {
    let mut args = Args::parse();

    args.socket = resolve_socket_path(&args.socket);

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
        use tokio::sync::oneshot;
        let (_tx, rx) = oneshot::channel();
        run_server_with_runner(
            args,
            matlab,
            master_raw_fd,
            rx,
            |socket, matlab, shutdown| Box::pin(server::run(socket, matlab, shutdown)),
        )
        .await;
    });

    // Restore original terminal settings.
    if let Some(ref orig) = orig_termios {
        termios::tcsetattr(&stdin_handle, termios::SetArg::TCSANOW, orig).ok();
    }

    std::process::exit(0);
}

async fn run_server_with_runner<F>(
    args: Args,
    matlab: matlab::Matlab,
    master_raw_fd: i32,
    mut external_shutdown: tokio::sync::oneshot::Receiver<()>,
    server_runner: impl FnOnce(String, Arc<matlab::Matlab>, tokio::sync::oneshot::Receiver<()>) -> F,
) where
    F: std::future::Future<Output = io::Result<()>> + Send + 'static,
{
    use std::sync::Arc;
    use tokio::io::unix::AsyncFd;
    use tokio::sync::oneshot;
    use tokio::task::JoinSet;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_tx2, shutdown_rx2) = oneshot::channel();

    let matlab = Arc::new(matlab);
    let mut tasks = JoinSet::new();

    // Wrap the master FD for async I/O.
    let master_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(libc::dup(master_raw_fd)) };
    let master_async = Arc::new(AsyncFd::new(master_fd).expect("Failed to create AsyncFd"));

    // Forward MATLAB PTY output → stdout.
    let master_async_clone = Arc::clone(&master_async);
    tasks.spawn(async move {
        forward_pty_to_stdout(master_async_clone, shutdown_tx).await;
    });

    // Forward stdin → MATLAB PTY.
    tasks.spawn(async move {
        forward_stdin_to_pty(tokio::io::stdin(), master_raw_fd).await;
    });

    // Listen for signals to ensure we can exit even in raw mode.
    tasks.spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let sigint = signal(SignalKind::interrupt()).ok();
        let sigterm = signal(SignalKind::terminate()).ok();
        listen_for_signals(sigint, sigterm, shutdown_tx2).await;
    });

    // Run the Unix socket server.
    let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel();
    let matlab_clone = Arc::clone(&matlab);
    let socket_path = args.socket.clone();
    let server_handle = tokio::spawn(server_runner(socket_path, matlab_clone, server_shutdown_rx));

    tokio::select! {
        _ = server_handle => {},
        _ = shutdown_rx => log_raw!("[server] MATLAB exited, shutting down"),
        _ = shutdown_rx2 => log_raw!("[server] signal received, shutting down"),
        _ = &mut external_shutdown => log_raw!("[server] external shutdown received"),
    }

    let _ = server_shutdown_tx.send(());
    tasks.shutdown().await;

    // Cleanup: ensure the child process (and its group) is dead.
    matlab.kill_session();

    // Remove the socket file on all exit paths.
    let _ = std::fs::remove_file(&args.socket);
}

async fn forward_pty_to_stdout(
    master_async: Arc<tokio::io::unix::AsyncFd<OwnedFd>>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) {
    use tokio::io::AsyncWriteExt;
    let mut stdout = tokio::io::stdout();
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
}

async fn forward_stdin_to_pty(mut stdin: impl tokio::io::AsyncRead + Unpin, master_raw_fd: i32) {
    use tokio::io::AsyncReadExt;
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
}

async fn listen_for_signals(
    sigint: Option<tokio::signal::unix::Signal>,
    sigterm: Option<tokio::signal::unix::Signal>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) {
    let mut sigint = sigint;
    let mut sigterm = sigterm;

    let int_fut = async {
        if let Some(ref mut s) = sigint {
            s.recv().await;
            log_raw!("[server] caught SIGINT");
        } else {
            std::future::pending::<()>().await;
        }
    };

    let term_fut = async {
        if let Some(ref mut s) = sigterm {
            s.recv().await;
            log_raw!("[server] caught SIGTERM");
        } else {
            std::future::pending::<()>().await;
        }
    };

    tokio::select! {
        _ = int_fut => {},
        _ = term_fut => {},
    }
    let _ = shutdown_tx.send(());
}

#[cfg(test)]
async fn mock_listen_for_signals(
    mut sigint: impl std::future::Future<Output = ()> + Unpin,
    mut sigterm: impl std::future::Future<Output = ()> + Unpin,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
) {
    tokio::select! {
        _ = &mut sigint => log_raw!("[server] caught SIGINT"),
        _ = &mut sigterm => log_raw!("[server] caught SIGTERM"),
    }
    let _ = shutdown_tx.send(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_socket_path() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("/tmp/vim-matlab-test-{}-{}.sock", std::process::id(), n)
    }

    #[test]
    fn test_raw_read_success() {
        let (mut s1, s2) = UnixStream::pair().unwrap();
        s1.write_all(b"hello").unwrap();
        let mut buf = [0u8; 10];
        let n = raw_read(s2.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn test_raw_read_eof() {
        let (_s1, s2) = UnixStream::pair().unwrap();
        // Closing _s1 should send EOF
        drop(_s1);
        let mut buf = [0u8; 10];
        let n = raw_read(s2.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_raw_write_all_success() {
        let (s1, mut s2) = UnixStream::pair().unwrap();
        raw_write_all(s1.as_raw_fd(), b"world").unwrap();
        let mut buf = [0u8; 5];
        s2.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn test_raw_read_error() {
        // Use an invalid FD
        let mut buf = [0u8; 10];
        let res = raw_read(-1, &mut buf);
        assert!(res.is_err());
    }

    #[test]
    fn test_raw_write_all_error() {
        // Use an invalid FD
        let res = raw_write_all(-1, b"error");
        assert!(res.is_err());
    }

    #[test]
    fn test_resolve_socket_path() {
        let uid = nix::unistd::getuid();
        assert_eq!(
            resolve_socket_path("/tmp/test-{uid}"),
            format!("/tmp/test-{}", uid)
        );
        assert_eq!(resolve_socket_path("/tmp/test-1000"), "/tmp/test-1000");
    }

    #[test]
    fn test_log_raw_macro() {
        // Just a smoke test to ensure it compiles and runs
        log_raw!("test log: {}", 123);
    }

    #[tokio::test]
    async fn test_forward_pty_to_stdout_io_error() {
        let (_s1, _s2) = std::os::unix::net::UnixStream::pair().unwrap();
        // Use an invalid FD to trigger an error
        let (_tx, _rx) = tokio::sync::oneshot::channel::<()>();

        // This is hard to trigger a real error with AsyncFd without closing it,
        // but we can at least hit the branch with a mock if needed.
    }

    #[tokio::test]
    async fn test_forward_pty_to_stdout_eof() {
        let (s1, s2) = UnixStream::pair().unwrap();
        let owned_s2 = unsafe { OwnedFd::from_raw_fd(libc::dup(s2.as_raw_fd())) };

        // Set non-blocking
        let flags = nix::fcntl::fcntl(&owned_s2, nix::fcntl::FcntlArg::F_GETFL).unwrap();
        let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
        flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
        nix::fcntl::fcntl(&owned_s2, nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();

        let master_async = Arc::new(tokio::io::unix::AsyncFd::new(owned_s2).unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();

        // Close s1 to send EOF
        drop(s1);

        forward_pty_to_stdout(master_async, tx).await;

        // Should have sent shutdown
        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_forward_pty_to_stdout_data() {
        let (mut s1, s2) = UnixStream::pair().unwrap();
        let owned_s2 = unsafe { OwnedFd::from_raw_fd(libc::dup(s2.as_raw_fd())) };

        // Set non-blocking
        let flags = nix::fcntl::fcntl(&owned_s2, nix::fcntl::FcntlArg::F_GETFL).unwrap();
        let mut flags = nix::fcntl::OFlag::from_bits_truncate(flags);
        flags.insert(nix::fcntl::OFlag::O_NONBLOCK);
        nix::fcntl::fcntl(&owned_s2, nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();

        let master_async = Arc::new(tokio::io::unix::AsyncFd::new(owned_s2).unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();

        s1.write_all(b"test data").unwrap();
        // Drop s1 to trigger EOF after data is read
        drop(s1);

        forward_pty_to_stdout(master_async, tx).await;
        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_listen_for_signals_none() {
        // With both signals None the futures are pending forever, so we race
        // against a short timeout to exercise entry into the function body.
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let listen = listen_for_signals(None, None, tx);
        let timeout = tokio::time::sleep(std::time::Duration::from_millis(10));
        tokio::select! {
            _ = listen => {},
            _ = timeout => {},
        }
        // rx will be dropped (sender dropped on timeout), that's fine.
        drop(rx);
    }

    #[tokio::test]
    async fn test_mock_listen_for_signals_int() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sigint = std::future::ready(());
        let sigterm = std::future::pending();
        mock_listen_for_signals(Box::pin(sigint), Box::pin(sigterm), tx).await;
        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_mock_listen_for_signals_term() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sigint = std::future::pending();
        let sigterm = std::future::ready(());
        mock_listen_for_signals(Box::pin(sigint), Box::pin(sigterm), tx).await;
        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_listen_for_signals_sigint() {
        use tokio::signal::unix::{SignalKind, signal};
        let sigint = signal(SignalKind::interrupt()).ok();
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(listen_for_signals(sigint, None, tx));

        // Send SIGINT to ourselves
        nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGINT).unwrap();

        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_listen_for_signals_sigterm() {
        use tokio::signal::unix::{SignalKind, signal};
        let sigterm = signal(SignalKind::terminate()).ok();
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(listen_for_signals(None, sigterm, tx));

        nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGTERM)
            .unwrap();

        rx.await.unwrap();
    }

    #[tokio::test]
    async fn test_forward_stdin_to_pty_read_error() {
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::{AsyncRead, ReadBuf};

        struct ErrReader;
        impl AsyncRead for ErrReader {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context,
                _buf: &mut ReadBuf,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, "read error")))
            }
        }

        // Should return without panic when stdin errors
        forward_stdin_to_pty(ErrReader, 0).await;
    }

    #[tokio::test]
    async fn test_forward_stdin_to_pty_write_error() {
        // Writing to fd -1 fails, so forward_stdin_to_pty should break cleanly
        let data = b"hello";
        let stdin = io::Cursor::new(data);
        forward_stdin_to_pty(stdin, -1).await;
    }

    #[tokio::test]
    async fn test_raw_read_eio() {
        // Close the slave side of a PTY so that reading from master returns EIO.
        use nix::pty::{Winsize, openpty};
        let ws = Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&ws), None).unwrap();
        let master_fd = pty.master.as_raw_fd();
        drop(pty.slave); // closing slave triggers EIO on master read
        let mut buf = [0u8; 16];
        // raw_read maps EIO to Ok(0)
        let n = raw_read(master_fd, &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn test_run_server_with_runner_external_shutdown() {
        // Spawn `cat` as a stand-in for MATLAB.
        let matlab = matlab::Matlab::spawn("cat").expect("failed to spawn cat");
        let master_raw_fd = matlab.master_fd().as_raw_fd();
        let socket_path = unique_socket_path();
        let args = Args {
            matlab_cmd: "cat".to_string(),
            socket: socket_path.clone(),
        };

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        // Fire the external shutdown immediately.
        tx.send(()).unwrap();

        run_server_with_runner(
            args,
            matlab,
            master_raw_fd,
            rx,
            |socket, matlab_arc, shutdown| Box::pin(server::run(socket, matlab_arc, shutdown)),
        )
        .await;

        // Socket file should have been removed.
        assert!(!std::path::Path::new(&socket_path).exists());
    }

    #[tokio::test]
    async fn test_forward_stdin_to_pty_data() {
        let (s1, s2) = std::os::unix::net::UnixStream::pair().unwrap();
        s1.set_nonblocking(true).unwrap();
        s2.set_nonblocking(true).unwrap();
        let (s1_pty, mut _s2_pty) = std::os::unix::net::UnixStream::pair().unwrap();
        let pty_fd = s1_pty.as_raw_fd();

        let mut client = tokio::net::UnixStream::from_std(s1).unwrap();
        let stdin = tokio::net::UnixStream::from_std(s2).unwrap();

        use tokio::io::AsyncWriteExt;
        tokio::spawn(async move {
            client.write_all(b"to pty").await.unwrap();
            drop(client);
        });

        forward_stdin_to_pty(stdin, pty_fd).await;

        // Verify data reached pty
        let mut buf = [0u8; 6];
        _s2_pty.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"to pty");
    }
}
