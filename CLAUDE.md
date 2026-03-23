# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

vim-matlab is a Neovim remote plugin (rplugin) that provides an alternative to MATLAB's default editor. It remotely controls a CLI MATLAB instance via a TCP socket server, sending code from Neovim buffers to MATLAB for evaluation.

## Commands

- **Run tests**: `PYTHONPATH="rplugin/python3:rplugin/python3/vim_matlab" pytest -s` (from repo root)
- **Lint/format**: `ruff check` / `ruff format` (ruff is used, see `.ruff_cache/`)
- **Reload plugin in Neovim**: `scripts/reload-vim.sh [file]`
- **Run MATLAB server**: `scripts/vim-matlab-server.py`

## Architecture

### Two-process design

1. **Server** (`scripts/vim-matlab-server.py`) — standalone Python script that spawns a MATLAB CLI process (via `pexpect` or `subprocess`) and listens on TCP port 43889. Handles MATLAB crash recovery and auto-restart. Runs independently of Neovim.

2. **Neovim rplugin** (`rplugin/python3/vim_matlab/`) — registered via `:UpdateRemotePlugins`. Connects to the server over TCP to send code for execution.

### rplugin module layout (`rplugin/python3/vim_matlab/`)

- `__init__.py` — `VimMatlab` class decorated with `@neovim.plugin`. Defines all `:Matlab*` commands and autocmds. Entry point for the rplugin.
- `matlab_cli_controller.py` — `MatlabCliController` manages the TCP socket connection to the server. All code execution goes through `run_code()` which sends semicolon-joined lines over the socket.
- `python_vim_utils.py` — `PythonVimUtils` static utility class for Neovim buffer manipulation. Contains regex patterns for MATLAB comment stripping, cell detection (`%%`), ellipsis continuation, function block parsing, and variable detection. Module-level `vim` variable is set by `__init__.py`.
- `io_helper.py` — resolves path to bundled MATLAB helper scripts (`rplugin/python3/vim_matlab/matlab/`).
- `matlab/` — MATLAB `.m` helper scripts added to MATLAB's path at connection time (e.g., `evalAndClean.m`, `sendTcp.m`, `printVarInfo.m`).

### Key bindings

Defined in `ftplugin/matlab/vim-matlab.vim`, controlled by `g:matlab_auto_mappings` (default: 1). Keybindings only activate for `.m` files via ftplugin.

### Communication protocol

Simple line-based TCP protocol on port 43889. Commands are newline-terminated strings. Special messages: `"kill"` (SIGTERM), `"cancel"` (SIGINT). Everything else is treated as MATLAB code. The server wraps code in a `tic`/`toc` timer by default.

## Python version note

The rplugin directory is `rplugin/python3/` (Python 3). The `run-tests.sh` script still references `rplugin/python` — run pytest directly with the correct PYTHONPATH instead.
