# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

vim-matlab is a Neovim plugin that controls a CLI MATLAB instance. A Rust binary spawns MATLAB in a PTY and listens on a Unix socket; a Lua plugin sends code from Neovim buffers to MATLAB for evaluation.

## Commands

- **Build**: `cargo build`
- **Run tests**: `cargo test`
- **Run server**: `cargo run -- --matlab-cmd "matlab -nodesktop -nosplash"`
- **Lint**: `cargo clippy`
- **Format**: `cargo fmt`

## Architecture

### Two-component design

1. **Rust binary** (`src/`) — spawns MATLAB in a PTY, listens on a Unix socket (`/tmp/vim-matlab-{uid}.sock`). Accepts newline-delimited messages: `"cancel"` (SIGINT), `"kill"` (SIGTERM), anything else is MATLAB code.

2. **Lua plugin** (`lua/vim-matlab/init.lua`) — registers Neovim commands and keybindings for `.m` files. Parses buffer text (cells, selections, lines), strips MATLAB comments, and sends code over the Unix socket.

### Rust modules (`src/`)

- `main.rs` — CLI entry (clap), PTY stdout/stdin forwarding, tokio runtime setup
- `matlab.rs` — MATLAB PTY management: spawn via fork/exec, send code, send signals
- `server.rs` — tokio Unix socket listener, command dispatch
- `protocol.rs` — message parsing (code vs cancel/kill)

### Lua plugin (`lua/vim-matlab/`)

- `init.lua` — all plugin logic: socket client, buffer parsing, commands, keybindings, setup()

### Key bindings (for `.m` files, via ftplugin)

- `<leader><C-m>` — run cell (normal) / run selection (visual)
- `<leader><C-h>` — run current line
- `<leader>cc` — cancel (SIGINT)
- `<leader>cs` — launch server in terminal split
- `,h` — help for word under cursor
- `,e` — open file in MATLAB GUI editor

### Configuration (Lua)

```lua
require("vim-matlab").setup({
  socket = "/tmp/vim-matlab-custom.sock",  -- optional
  auto_mappings = true,                     -- default
})
```

Vim globals: `g:matlab_server_split` ("vertical"/"horizontal"), `g:matlab_server_cmd` (binary path override).
