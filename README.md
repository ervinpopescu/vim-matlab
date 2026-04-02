# vim-matlab

A Neovim plugin for controlling a CLI MATLAB (or Octave) instance. A Rust binary spawns MATLAB in a PTY and listens on a Unix socket; the Lua plugin sends code from Neovim buffers to MATLAB for evaluation.

## Requirements

- Neovim 0.9+
- Rust 1.85+ (to build the server binary)
- MATLAB R2020a+ or GNU Octave

## Installation

### 1. Build the server binary

Clone and build:

```sh
git clone https://github.com/ervinpopescu/vim-matlab
cd vim-matlab
cargo build --release
cp target/release/vim-matlab ~/.local/bin/
```

Or install directly with cargo:

```sh
cargo install --git https://github.com/ervinpopescu/vim-matlab
```

### 2. Install the Neovim plugin

Using [lazy.nvim](https://github.com/folke/lazy.nvim):

```lua
{
  "ervinpopescu/vim-matlab",
  ft = { "matlab", "octave" },
  config = function()
    require("vim-matlab").setup()
  end,
}
```

## Usage

Open a `.m` file, launch the server with `<leader>cs`, and use the keybindings below.

### Keybindings (`.m` and `.octave` files)

| Keybinding | Mode | Description |
|---|---|---|
| `<leader><C-m>` | Normal | Run current `%%` cell |
| `<leader><C-m>` | Visual | Run selection |
| `<leader><C-h>` | Normal | Run current line |
| `<leader>cc` | Normal | Cancel (SIGINT) |
| `<leader>cs` | Normal | Launch server in terminal split |
| `,h` | Normal | Help for word under cursor |
| `,e` | Normal | Open file in MATLAB GUI editor |

### Commands

`:MatlabRunCell`, `:MatlabRunSelection`, `:MatlabRunLine`, `:MatlabCancel`, `:MatlabLaunchServer`, `:MatlabHelp`, `:MatlabOpenInEditor`

### Starting the server manually

```sh
vim-matlab --matlab-cmd "matlab -nodesktop -nosplash"
```

The server listens on `/tmp/vim-matlab-<uid>.sock` by default.

## Configuration

```lua
require("vim-matlab").setup({
  -- Custom MATLAB launch command (default: nil, uses "matlab -nodesktop -nosplash")
  matlab_cmd = "matlab -nodesktop -nosplash",
  -- Custom server binary (default: "vim-matlab")
  server_cmd = "/path/to/vim-matlab",
  -- Custom socket path (default: /tmp/vim-matlab-<uid>.sock)
  socket = "/tmp/vim-matlab-custom.sock",
  -- Where to launch the server: "vim" (default) or "tmux"
  launcher = "vim",
  -- Split direction: "vertical" (default) or "horizontal"
  split = "vertical",
  -- Disable automatic keybindings (default: true)
  auto_mappings = true,
})
```

Vim globals (alternative to setup opts):
- `g:matlab_server_launcher` — `"vim"` (default) or `"tmux"`
- `g:matlab_server_split` — `"vertical"` (default) or `"horizontal"`
- `g:matlab_server_cmd` — override the server binary path

## How it works

1. **Rust server** (`vim-matlab` binary): spawns MATLAB in a PTY, forwarding I/O to the terminal. Listens on a Unix socket for newline-delimited commands from Neovim.

2. **Lua plugin**: parses the current buffer (cell detection, comment stripping, ellipsis continuation joining) and sends code over the socket.

### Socket protocol

- `cancel` — send SIGINT to MATLAB
- `kill` — send SIGTERM and shut down the server
- anything else — sent to MATLAB as code

## Development

```sh
cargo build    # build
cargo test     # run tests
cargo clippy   # lint
cargo fmt      # format
```

## Credits

This plugin is a Rust/Lua rewrite of the original
[vim-matlab](https://github.com/daeyunshin/vim-matlab) by
[Daeyun Shin](https://github.com/daeyunshin), which provided the core ideas
for PTY-based MATLAB interaction and Vim integration.
