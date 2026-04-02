local M = {}

local uv = vim.uv or vim.loop
local uid = uv.getuid()
local default_socket = "/tmp/vim-matlab-" .. uid .. ".sock"

local socket_path = default_socket
local server_cmd = "vim-matlab"
local matlab_cmd = nil

--- Strip MATLAB comment from a line (everything after unquoted %).
--- Single-quoted strings are skipped.
---@param line string
---@return string
function M._strip_comment(line)
  local i = 1
  local len = #line
  while i <= len do
    local c = line:sub(i, i)
    if c == "'" then
      -- skip single-quoted string
      i = i + 1
      while i <= len and line:sub(i, i) ~= "'" do
        i = i + 1
      end
      i = i + 1 -- skip closing quote
    elseif c == "%" then
      return line:sub(1, i - 1)
    else
      i = i + 1
    end
  end
  return line
end

--- Strip comments and blank lines; join ellipsis continuations.
---@param lines string[]
---@return string[]
function M._clean_lines(lines)
  local result = {}
  local is_continuation = false
  for _, raw in ipairs(lines) do
    local line = vim.trim(M._strip_comment(raw))
    if line ~= "" and line ~= "..." then
      local has_ellipsis = line:match("^(.*%S)%s*%.%.%.%s*$")
      if has_ellipsis then
        line = has_ellipsis
      end
      if is_continuation and #result > 0 then
        result[#result] = result[#result] .. " " .. line
      else
        result[#result + 1] = line
      end
      is_continuation = has_ellipsis ~= nil
    end
  end
  return result
end

--- Send a message to the vim-matlab Unix socket.
---@param msg string
local function send(msg)
  local pipe = uv.new_pipe()
  pipe:connect(socket_path, function(err)
    if err then
      vim.schedule(function()
        vim.notify("vim-matlab: " .. err, vim.log.levels.ERROR)
      end)
      pipe:close()
      return
    end
    pipe:write(msg .. "\n", function(werr)
      if werr then
        vim.schedule(function()
          vim.notify("vim-matlab: write error: " .. werr, vim.log.levels.ERROR)
        end)
      end
      pipe:close()
    end)
  end)
end

--- Cell header pattern: %% (but not %%%), or function/classdef.
local function is_cell_boundary(line)
  if line:match("^%%%%[^%%]") or line:match("^%%%%$") then
    return true
  end
  if line:match("^%s*function%s") or line:match("^%s*classdef%s") then
    return true
  end
  return false
end

--- Join statements smartly to avoid double semicolons or syntax errors.
function M._join_statements(lines)
  local result = ""
  for i, line in ipairs(lines) do
    -- Trim whitespace
    line = vim.trim(line)
    -- Remove any trailing semicolons or commas to prevent duplication
    line = line:gsub("[;,]+$", "")

    result = result .. line
    if i < #lines then
      result = result .. "; "
    else
      -- Add a trailing semicolon to the very last statement to suppress output
      result = result .. ";"
    end
  end
  return result
end

function M.run_cell()
  local buf = vim.api.nvim_get_current_buf()
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local crow = vim.api.nvim_win_get_cursor(0)[1] -- 1-indexed

  -- find cell start (search upward)
  local cell_start = crow
  while cell_start > 1 do
    if is_cell_boundary(lines[cell_start]) then
      cell_start = cell_start + 1
      break
    end
    cell_start = cell_start - 1
  end

  -- find cell end (search downward)
  local cell_end = crow
  while cell_end < #lines do
    cell_end = cell_end + 1
    if is_cell_boundary(lines[cell_end]) then
      cell_end = cell_end - 1
      break
    end
  end

  local cell_lines = {}
  for i = cell_start, cell_end do
    cell_lines[#cell_lines + 1] = lines[i]
  end

  local cleaned = M._clean_lines(cell_lines)
  if #cleaned > 0 then
    send(M._join_statements(cleaned))
  end
end

function M.run_selection()
  local s = vim.fn.getpos("'<")
  local e = vim.fn.getpos("'>")
  local lines = vim.api.nvim_buf_get_lines(0, s[2] - 1, e[2], false)
  local cleaned = M._clean_lines(lines)
  if #cleaned > 0 then
    send(M._join_statements(cleaned))
  end
end

function M.run_line()
  local line = vim.api.nvim_get_current_line()
  local stripped = vim.trim(M._strip_comment(line))
  if stripped ~= "" then
    send(stripped)
  end
end

function M.cancel()
  send("cancel")
end

function M.help()
  local word = vim.fn.expand("<cword>")
  if word ~= "" then
    send("help " .. word .. ";")
  end
end

function M.open_in_editor()
  local filepath = vim.fn.expand("%:p")
  if filepath ~= "" then
    send("edit '" .. filepath .. "';")
  end
end

function M.launch_server()
  local split = vim.g.matlab_server_split or "vertical"
  local launcher = vim.g.matlab_server_launcher or "vim"
  local cmd = vim.g.matlab_server_cmd or server_cmd
  if matlab_cmd and matlab_cmd ~= "" then
    if not cmd:find("--matlab-cmd") then
      cmd = cmd .. " --matlab-cmd " .. vim.fn.shellescape(matlab_cmd)
    end
  end
  if launcher == "tmux" then
    local flag = split == "horizontal" and "-v" or "-h"
    vim.fn.system("tmux split-window " .. flag .. " " .. vim.fn.shellescape(cmd))
  else
    local prefix = split == "horizontal" and "split" or "vsplit"
    vim.cmd(prefix .. " | terminal " .. cmd)
  end
end

--- Apply buffer-local mappings and commands to the current buffer.
---@param bufnr? number
function M.attach(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  if vim.b[bufnr].vim_matlab_attached then
    return
  end
  vim.b[bufnr].vim_matlab_attached = true
  local bopts = { buffer = bufnr, silent = true }

  -- Mappings
  vim.keymap.set(
    "n",
    "<leader><C-m>",
    M.run_cell,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: run cell" })
  )
  vim.keymap.set(
    "v",
    "<leader><C-m>",
    "<ESC><cmd>lua require('vim-matlab').run_selection()<CR>",
    vim.tbl_extend("force", bopts, { desc = "MATLAB: run selection" })
  )
  vim.keymap.set(
    "n",
    "<leader><C-h>",
    M.run_line,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: run line" })
  )
  vim.keymap.set(
    "n",
    "<leader>cc",
    M.cancel,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: cancel (SIGINT)" })
  )
  vim.keymap.set(
    "n",
    ",h",
    M.help,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: help for word" })
  )
  vim.keymap.set(
    "n",
    ",e",
    M.open_in_editor,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: open in editor" })
  )
  vim.keymap.set(
    "n",
    "<leader>cs",
    M.launch_server,
    vim.tbl_extend("force", bopts, { desc = "MATLAB: launch server" })
  )

  -- Commands
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabRunCell", M.run_cell, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabRunSelection", M.run_selection, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabRunLine", M.run_line, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabCancel", M.cancel, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabHelp", M.help, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabOpenInEditor", M.open_in_editor, {})
  vim.api.nvim_buf_create_user_command(bufnr, "MatlabLaunchServer", M.launch_server, {})
end

---@param opts? { socket?: string, auto_mappings?: boolean, server_cmd?: string, matlab_cmd?: string, launcher?: string, split?: string }
function M.setup(opts)
  opts = opts or {}
  if opts.socket then
    socket_path = opts.socket
  end
  if opts.server_cmd then
    server_cmd = opts.server_cmd
  end
  if opts.matlab_cmd ~= nil then
    matlab_cmd = opts.matlab_cmd
  end
  if opts.launcher then
    vim.g.matlab_server_launcher = opts.launcher
  end
  if opts.split then
    vim.g.matlab_server_split = opts.split
  end

  if opts.auto_mappings ~= false then
    -- If we're already in a matlab/octave file, attach immediately
    local ft = vim.bo.filetype
    if ft == "matlab" or ft == "octave" then
      M.attach()
    end

    -- Also handle any future matlab/octave files
    vim.api.nvim_create_autocmd("FileType", {
      pattern = { "matlab", "octave" },
      group = vim.api.nvim_create_augroup("VimMatlab", { clear = true }),
      callback = function(ev)
        M.attach(ev.buf)
      end,
    })
  end
end

return M
