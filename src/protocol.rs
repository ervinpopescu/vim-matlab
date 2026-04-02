//! Command protocol for vim-matlab.
//!
//! Messages are newline-delimited strings received over a Unix socket.
//! Two special keywords (`cancel`, `kill`) are recognised; everything
//! else is treated as MATLAB code to evaluate.

/// A parsed command from a client connection.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Execute the given string as MATLAB code.
    RunCode(String),
    /// Send SIGINT to the MATLAB process (Ctrl-C).
    Cancel,
    /// Send SIGTERM to the MATLAB process (graceful shutdown).
    Kill,
}

/// Parse a raw message string into a [`Command`].
///
/// Leading and trailing whitespace is stripped before matching.
pub fn parse_message(msg: &str) -> Command {
    match msg.trim() {
        "cancel" => Command::Cancel,
        "kill" => Command::Kill,
        code => Command::RunCode(code.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cancel() {
        assert_eq!(parse_message("cancel"), Command::Cancel);
    }

    #[test]
    fn parse_kill() {
        assert_eq!(parse_message("kill"), Command::Kill);
    }

    #[test]
    fn parse_code() {
        assert_eq!(
            parse_message("disp('hello')"),
            Command::RunCode("disp('hello')".to_string()),
        );
    }

    #[test]
    fn parse_whitespace_trimming() {
        assert_eq!(parse_message("  cancel  "), Command::Cancel);
        assert_eq!(parse_message("\tkill\n"), Command::Kill);
        assert_eq!(
            parse_message("  x = 1  "),
            Command::RunCode("x = 1".to_string()),
        );
    }

    #[test]
    fn parse_empty_is_code() {
        // An empty (whitespace-only) message becomes RunCode("")
        assert_eq!(parse_message("   "), Command::RunCode(String::new()));
    }

    #[test]
    fn parse_case_sensitive() {
        // "Cancel" (capitalised) is NOT the cancel keyword
        assert_eq!(
            parse_message("Cancel"),
            Command::RunCode("Cancel".to_string()),
        );
    }

    #[test]
    fn parse_multiline_code() {
        let code = "x = 1;\ny = 2;";
        assert_eq!(parse_message(code), Command::RunCode(code.to_string()));
    }

    #[test]
    fn parse_code_with_keywords() {
        // "kill" inside a string shouldn't trigger the Kill command
        let code = "disp('kill')";
        assert_eq!(parse_message(code), Command::RunCode(code.to_string()));
    }
}
