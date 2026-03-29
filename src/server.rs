//! Unix socket server for vim-matlab.
//!
//! Listens on a Unix domain socket for newline-delimited commands from
//! Neovim (or any client) and dispatches them to the MATLAB PTY.

use std::io;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

use crate::matlab::Matlab;
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
pub async fn run(socket_path: &str, matlab: &Matlab) -> io::Result<()> {
    // Remove a leftover socket file from a previous run.
    if std::path::Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    log_raw!("[server] listening on {socket_path}");

    loop {
        let (stream, _addr) = listener.accept().await?;
        log_raw!("[server] client connected");

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
                    return Ok(());
                }
                Command::RunCode(code) => {
                    let preview = if code.len() > 80 {
                        format!("{}...", &code[..80])
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

        log_raw!("[server] client disconnected");
    }
}
