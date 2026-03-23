"""Tests for server-side functions (output_filter, input_filter, run_code formatting)."""

import importlib
import sys
from unittest import mock


# The server script lives outside the package tree, so import it by path.
def _import_server():
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "vim_matlab_server", "scripts/vim-matlab-server.py"
    )
    mod = importlib.util.module_from_spec(spec)
    # Patch pexpect import so tests work without it installed
    with mock.patch.dict(sys.modules, {"pexpect": None}):
        spec.loader.exec_module(mod)
    return mod


server = _import_server()


# ---------------------------------------------------------------------------
# output_filter
# ---------------------------------------------------------------------------


class TestOutputFilter:
    def setup_method(self):
        server.hide_until_newline = False

    def test_passthrough_when_not_hiding(self):
        assert server.output_filter("hello") == "hello"

    def test_hides_until_newline_str(self):
        server.hide_until_newline = True
        assert server.output_filter("no newline here") == ""
        assert server.hide_until_newline is True

    def test_reveals_after_newline_str(self):
        server.hide_until_newline = True
        result = server.output_filter("before\nafter")
        assert result == "\nafter"
        assert server.hide_until_newline is False

    def test_hides_until_newline_bytes(self):
        server.hide_until_newline = True
        assert server.output_filter(b"no newline here") == b""
        assert server.hide_until_newline is True

    def test_reveals_after_newline_bytes(self):
        server.hide_until_newline = True
        result = server.output_filter(b"before\nafter")
        assert result == b"\nafter"
        assert server.hide_until_newline is False


# ---------------------------------------------------------------------------
# input_filter
# ---------------------------------------------------------------------------


class TestInputFilter:
    def setup_method(self):
        server.auto_restart = True

    def test_normal_input_passthrough(self):
        assert server.input_filter("a") == "a"
        assert server.auto_restart is True

    def test_ctrl_backslash_str(self):
        server.input_filter("\x1c")
        assert server.auto_restart is False

    def test_ctrl_backslash_bytes(self):
        server.input_filter(b"\x1c")
        assert server.auto_restart is False


# ---------------------------------------------------------------------------
# print_flush
# ---------------------------------------------------------------------------


class TestPrintFlush:
    def test_str_output(self, capsys):
        server.use_pexpect = False
        server.print_flush("hello")
        assert capsys.readouterr().out == "hello\n"

    def test_bytes_decoded(self, capsys):
        server.use_pexpect = False
        server.print_flush(b"hello")
        assert capsys.readouterr().out == "hello\n"

    def test_custom_end(self, capsys):
        server.use_pexpect = False
        server.print_flush("hello", end="")
        assert capsys.readouterr().out == "hello"


# ---------------------------------------------------------------------------
# Matlab.run_code command formatting
# ---------------------------------------------------------------------------


class TestRunCodeFormatting:
    def test_wraps_with_timer(self):
        """run_code should wrap code in tic/toc when run_timer=True."""
        matlab = server.Matlab.__new__(server.Matlab)
        matlab.proc = mock.MagicMock()

        # Use non-pexpect path to capture what gets written
        orig = server.use_pexpect
        server.use_pexpect = False
        try:
            matlab.proc.stdin = mock.MagicMock()
            server.Matlab.run_code(matlab, "disp(1)", run_timer=True)
            written = matlab.proc.stdin.write.call_args[0][0]
            assert isinstance(written, bytes)
            text = written.decode()
            assert "tic" in text
            assert "disp(1)" in text
            assert "toc" in text
        finally:
            server.use_pexpect = orig

    def test_no_timer(self):
        """run_code should send raw code when run_timer=False."""
        matlab = server.Matlab.__new__(server.Matlab)
        matlab.proc = mock.MagicMock()

        orig = server.use_pexpect
        server.use_pexpect = False
        try:
            matlab.proc.stdin = mock.MagicMock()
            server.Matlab.run_code(matlab, "disp(1)", run_timer=False)
            written = matlab.proc.stdin.write.call_args[0][0]
            text = written.decode()
            assert text.strip() == "disp(1)"
        finally:
            server.use_pexpect = orig
