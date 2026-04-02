//! Unix socket server for vim-matlab.
//!
//! Listens on a Unix domain socket for newline-delimited commands from
//! Neovim (or any client) and dispatches them to the MATLAB PTY.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::matlab::MatlabActions;
use crate::protocol::{self, Command};

macro_rules! log_raw {
    ($($arg:tt)*) => {
        eprintln!("\r{}", format_args!($($arg)*))
    };
}

/// Interface for a stream listener.
#[async_trait::async_trait]
pub trait Listener {
    async fn accept(&self) -> io::Result<(UnixStream, ())>;
}

#[async_trait::async_trait]
impl Listener for UnixListener {
    async fn accept(&self) -> io::Result<(UnixStream, ())> {
        self.accept().await.map(|(s, _a)| (s, ()))
    }
}

/// Run the Unix socket server, accepting connections and dispatching
/// commands to `matlab` until a `kill` command is received.
///
/// Any stale socket file at `socket_path` is removed before binding.
pub async fn run(
    socket_path: String,
    matlab: Arc<impl MatlabActions>,
    shutdown: tokio::sync::oneshot::Receiver<()>,
) -> io::Result<()> {
    // Remove a leftover socket file from a previous run.
    if std::path::Path::new(&socket_path).exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    log_raw!("[server] listening on {socket_path}");
    run_with_listener(listener, matlab, socket_path, shutdown).await
}

/// Run the server with a provided listener.
pub async fn run_with_listener(
    listener: impl Listener,
    matlab: Arc<impl MatlabActions>,
    socket_path: String,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _addr) = result?;
                log_raw!("[server] client connected");
                if handle_client(stream, &*matlab, &socket_path).await? {
                    return Ok(());
                }
                log_raw!("[server] client disconnected");
            }
            _ = &mut shutdown => {
                log_raw!("[server] shutdown signal received");
                return Ok(());
            }
        }
    }
}

/// Handle a single client connection.
async fn handle_client(
    stream: tokio::net::UnixStream,
    matlab: &impl MatlabActions,
    socket_path: &str,
) -> io::Result<bool> {
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        match protocol::parse_message(&line) {
            Command::Cancel => {
                log_raw!("[server] <- cancel");
                matlab.send_sigint();
            }
            Command::Kill => {
                log_raw!("[server] <- kill");
                matlab.send_sigterm();
                // Clean up the socket file before exiting.
                let _ = std::fs::remove_file(socket_path);
                return Ok(true);
            }
            Command::RunCode(code) => {
                let preview = if code.len() > 80 {
                    format!("{}...", code.chars().take(80).collect::<String>())
                } else {
                    code.clone()
                };
                log_raw!("[server] <- code: {preview}");
                if let Err(e) = matlab.send_code(&code) {
                    log_raw!("[server] error sending code: {e}");
                }
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use tokio::net::UnixStream;

    fn unique_socket_path() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("/tmp/vim-matlab-test-{}-{}.sock", std::process::id(), n)
    }

    struct MockMatlab {
        codes: Arc<Mutex<Vec<String>>>,
        sigints: Arc<Mutex<usize>>,
        sigterms: Arc<Mutex<usize>>,
    }

    impl MatlabActions for MockMatlab {
        fn send_code(&self, code: &str) -> io::Result<()> {
            self.codes.lock().unwrap().push(code.to_string());
            Ok(())
        }
        fn send_sigint(&self) {
            *self.sigints.lock().unwrap() += 1;
        }
        fn send_sigterm(&self) {
            *self.sigterms.lock().unwrap() += 1;
        }
    }

    #[tokio::test]
    async fn test_handle_client_run_code() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mock = MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        };

        use tokio::io::AsyncWriteExt;
        client.write_all(b"x = 1\ny = 2\n").await.unwrap();
        drop(client);

        let result = handle_client(server, &mock, &unique_socket_path())
            .await
            .unwrap();
        assert!(!result);
        assert_eq!(mock.codes.lock().unwrap().as_slice(), &["x = 1", "y = 2"]);
    }

    #[tokio::test]
    async fn test_handle_client_long_code() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mock = MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        };

        use tokio::io::AsyncWriteExt;
        let long_code = "a".repeat(100);
        client
            .write_all(format!("{}\n", long_code).as_bytes())
            .await
            .unwrap();
        drop(client);

        handle_client(server, &mock, &unique_socket_path())
            .await
            .unwrap();
        assert_eq!(mock.codes.lock().unwrap()[0], long_code);
    }

    #[tokio::test]
    async fn test_handle_client_cancel() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mock = MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        };

        use tokio::io::AsyncWriteExt;
        client.write_all(b"cancel\n").await.unwrap();
        drop(client);

        handle_client(server, &mock, &unique_socket_path())
            .await
            .unwrap();
        assert_eq!(*mock.sigints.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_handle_client_kill() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mock = MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        };

        // Create a dummy socket file so Kill doesn't fail
        let socket_path = unique_socket_path();
        let _ = std::fs::File::create(&socket_path);

        use tokio::io::AsyncWriteExt;
        client.write_all(b"kill\n").await.unwrap();
        drop(client);

        let result = handle_client(server, &mock, &socket_path).await.unwrap();
        assert!(result);
        assert_eq!(*mock.sigterms.lock().unwrap(), 1);
        assert!(!std::path::Path::new(&socket_path).exists());
    }

    #[tokio::test]
    async fn test_handle_client_send_error() {
        let (mut client, server) = UnixStream::pair().unwrap();
        struct ErrorMatlab;
        impl MatlabActions for ErrorMatlab {
            fn send_code(&self, _code: &str) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::Other, "send error"))
            }
            fn send_sigint(&self) {}
            fn send_sigterm(&self) {}
        }

        use tokio::io::AsyncWriteExt;
        client.write_all(b"x = 1\n").await.unwrap();
        drop(client);

        // Should not return error, just log it
        let result = handle_client(server, &ErrorMatlab, &unique_socket_path()).await;
        assert!(result.is_ok());
    }

    struct MockListener {
        stream: Mutex<Option<UnixStream>>,
    }

    #[async_trait::async_trait]
    impl Listener for MockListener {
        async fn accept(&self) -> io::Result<(UnixStream, ())> {
            let mut s = self.stream.lock().unwrap();
            if let Some(stream) = s.take() {
                Ok((stream, ()))
            } else {
                // Simulate listener closing/error to break loop
                Err(io::Error::new(io::ErrorKind::Other, "done"))
            }
        }
    }

    #[tokio::test]
    async fn test_run_with_listener_kill() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let mock_matlab = Arc::new(MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        });

        let socket_path = unique_socket_path();
        let _ = std::fs::File::create(&socket_path);

        let listener = MockListener {
            stream: Mutex::new(Some(server)),
        };

        use tokio::io::AsyncWriteExt;
        client.write_all(b"kill\n").await.unwrap();
        drop(client);

        let (_tx, rx) = tokio::sync::oneshot::channel();
        let _ = run_with_listener(listener, mock_matlab.clone(), socket_path.clone(), rx).await;
        assert_eq!(*mock_matlab.sigterms.lock().unwrap(), 1);
        assert!(!std::path::Path::new(&socket_path).exists());
    }

    #[tokio::test]
    async fn test_run_direct_socket() {
        let socket_path = unique_socket_path();
        let _ = std::fs::remove_file(&socket_path);

        let mock_matlab = Arc::new(MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        });

        let (_tx, rx) = tokio::sync::oneshot::channel();
        let path_clone = socket_path.clone();
        let matlab_clone = Arc::clone(&mock_matlab);
        let server_task = tokio::spawn(async move { run(path_clone, matlab_clone, rx).await });

        // Give the listener a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let mut client = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
        use tokio::io::AsyncWriteExt;
        let _ = std::fs::File::create(&socket_path); // ensure file exists for kill cleanup
        client.write_all(b"kill\n").await.unwrap();
        drop(client);

        server_task.await.unwrap().unwrap();
        assert_eq!(*mock_matlab.sigterms.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn test_run_with_listener_client_disconnect() {
        // Client connects but sends nothing — tests the "disconnected" log path.
        let (client, server) = UnixStream::pair().unwrap();
        drop(client); // immediate disconnect

        let mock_matlab = Arc::new(MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        });

        let listener = MockListener {
            stream: Mutex::new(Some(server)),
        };

        let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
        // MockListener errors on the second accept, which propagates through result?
        let result = run_with_listener(listener, mock_matlab, unique_socket_path(), rx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_with_listener_shutdown() {
        // Use a listener whose accept() never resolves so the shutdown branch
        // is guaranteed to win the select, regardless of scheduling order.
        struct PendingListener;
        #[async_trait::async_trait]
        impl Listener for PendingListener {
            async fn accept(&self) -> io::Result<(UnixStream, ())> {
                std::future::pending().await
            }
        }

        let mock_matlab = Arc::new(MockMatlab {
            codes: Arc::new(Mutex::new(vec![])),
            sigints: Arc::new(Mutex::new(0)),
            sigterms: Arc::new(Mutex::new(0)),
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(()).unwrap();

        let result =
            run_with_listener(PendingListener, mock_matlab, unique_socket_path(), rx).await;
        assert!(result.is_ok());
    }
}
