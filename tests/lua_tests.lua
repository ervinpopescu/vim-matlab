-- Mock vim for testing logic in isolation
local mock_uv = {
  getuid = function()
    return 1000
  end,
}
_G.vim = {
  trim = function(s)
    if not s then
      return ""
    end
    return s:match("^%s*(.-)%s*$")
  end,
  schedule = function() end,
  notify = function() end,
  tbl_extend = function(_, ...)
    local result = {}
    for _, t in ipairs({ ... }) do
      for k, v in pairs(t) do
        result[k] = v
      end
    end
    return result
  end,
  bo = { filetype = "" },
  b = setmetatable({}, {
    __index = function()
      return {}
    end,
  }),
  fn = {
    shellescape = function(s)
      return "'" .. s .. "'"
    end,
    system = function() end,
  },
  api = {
    nvim_create_augroup = function() end,
    nvim_create_autocmd = function() end,
    nvim_buf_create_user_command = function() end,
    nvim_get_current_buf = function()
      return 0
    end,
  },
  keymap = {
    set = function() end,
  },
  log = { levels = { ERROR = 1 } },
  uv = mock_uv,
  loop = mock_uv,
}

-- Mock the require path
package.path = package.path .. ";./?.lua;./lua/?.lua"
local M = require("vim-matlab.init")

local function assert_eq(actual, expected, msg)
  if type(actual) == "table" and type(expected) == "table" then
    if #actual ~= #expected then
      error(string.format("FAIL: %s\n  Length mismatch: %d vs %d", msg, #actual, #expected))
    end
    for i = 1, #actual do
      if actual[i] ~= expected[i] then
        error(
          string.format("FAIL: %s\n  At index %d: '%s' vs '%s'", msg, i, actual[i], expected[i])
        )
      end
    end
  elseif actual ~= expected then
    error(
      string.format(
        "FAIL: %s\n  Expected: '%s'\n  Actual:   '%s'",
        msg,
        tostring(expected),
        tostring(actual)
      )
    )
  end
end

print("Running Lua logic tests...")

-- Test _strip_comment
assert_eq(M._strip_comment("x = 1; % comment"), "x = 1; ", "Basic comment stripping")
assert_eq(
  M._strip_comment("disp('% not a comment')"),
  "disp('% not a comment')",
  "Comment symbol in string"
)
assert_eq(M._strip_comment("%% cell header"), "", "Double percent header stripping")

-- Test _clean_lines
local lines = {
  "x = 1; % comment",
  "",
  "y = 2; ...",
  "    + 3;",
  "disp('hello')",
}
local expected = {
  "x = 1;",
  "y = 2; + 3;",
  "disp('hello')",
}
assert_eq(M._clean_lines(lines), expected, "Clean lines and join ellipsis")

-- Test _join_statements
assert_eq(M._join_statements({ "a = 1", "b = 2" }), "a = 1; b = 2;", "Join multiple lines")
assert_eq(M._join_statements({ "a = 1;", "b = 2;" }), "a = 1; b = 2;", "Avoid double semicolons")
assert_eq(
  M._join_statements({ "a = 1,", "b = 2" }),
  "a = 1; b = 2;",
  "Normalize comma to semicolon"
)
assert_eq(
  M._join_statements({ "Fs = 2e6;", "t = 0:1/Fs:1;" }),
  "Fs = 2e6; t = 0:1/Fs:1;",
  "Fix double semicolon issue from user report"
)

print("Lua tests passed!")
