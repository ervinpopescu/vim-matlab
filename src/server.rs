//! Unix socket server for vim-matlab.
//!
//! Listens on a Unix domain socket for newline-delimited commands from
//! Neovim (or any client) and dispatches them to the MATLAB PTY.

use std::io;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

use crate::matlab::MatlabActions;
use crate::protocol::{self, Command};

macro_rules! log_raw {
    ($($arg:tt)*) => {
        eprintln!("\r{}", format_args!($($arg)*))
    };
}

/// Run the Unix socket server, accepting connections and dispatching
/// commands to `matlab` until a `kill` command is received.
///
/// Any stale socket file at `socket_path` is removed before binding.
pub async fn run(socket_path: &str, matlab: &impl MatlabActions) -> io::Result<()> {
    // Remove a leftover socket file from a previous run.
    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    log_raw!("[server] listening on {socket_path}");

    loop {
        let (stream, _addr) = listener.accept().await?;
        log_raw!("[server] client connected");
        if handle_client(stream, matlab, socket_path).await? {
            return Ok(());
        }
        log_raw!("[server] client disconnected");
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
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixStream;

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

        let result = handle_client(server, &mock, "/tmp/test.sock")
            .await
            .unwrap();
        assert!(!result);
        assert_eq!(mock.codes.lock().unwrap().as_slice(), &["x = 1", "y = 2"]);
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

        handle_client(server, &mock, "/tmp/test.sock")
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
        let socket_path = "/tmp/vim-matlab-test-kill.sock";
        let _ = std::fs::File::create(socket_path);

        use tokio::io::AsyncWriteExt;
        client.write_all(b"kill\n").await.unwrap();
        drop(client);

        let result = handle_client(server, &mock, socket_path).await.unwrap();
        assert!(result);
        assert_eq!(*mock.sigterms.lock().unwrap(), 1);
        assert!(!std::path::Path::new(socket_path).exists());
    }
}
