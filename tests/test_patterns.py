from vim_matlab import python_vim_utils

PVU = python_vim_utils.PythonVimUtils


def strip_comments(line):
    return PVU.comment_pattern.sub(r"\1", line).strip()


# ---------------------------------------------------------------------------
# Comment stripping
# ---------------------------------------------------------------------------


def test_comment_pattern_simple():
    assert "hello" == strip_comments("hello")
    assert "hello" == strip_comments("hello %")
    assert "hello" == strip_comments("hello %%")


def test_comment_pattern_strings():
    assert "disp('%')" == strip_comments("disp('%') % ")
    assert "disp('%')" == strip_comments("disp('%')")
    assert "disp('')" == strip_comments("disp('')")
    assert "disp('')" == strip_comments("disp('')% ")
    assert "disp('')" == strip_comments("disp('') % ")
    assert "disp('')" == strip_comments("disp('') %%")


def test_comment_pattern_compound():
    assert "disp('%');disp('%')" == strip_comments("disp('%');disp('%')%disp('')")
    assert "A = sqrtm(B) \\ C.';" == strip_comments("A = sqrtm(B) \\ C.'; % foo")
    assert "disp('100%');A = sqrtm(B) \\ C.';" == strip_comments(
        "disp('100%');A = sqrtm(B) \\ C.';% foo"
    )


def test_comment_pattern_transpose():
    assert "disp('%');C.'" == strip_comments("disp('%');C.'")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% '")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% ''")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% '''")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% '.''")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% ' '")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% ' ' %")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% ' %' ")
    assert "disp('%');C.'" == strip_comments("disp('%');C.'% ' %' %")
    assert "disp('% ''');C.';" == strip_comments("disp('% ''');C.'; % ' %' %")
    assert "disp('% .''');C.';" == strip_comments("disp('% .''');C.'; % ' %' %")


# ---------------------------------------------------------------------------
# Cell header detection
# ---------------------------------------------------------------------------


def test_cell_header_pattern():
    pat = PVU.cell_header_pattern
    assert pat.match("%% Cell 1")
    assert pat.match("%% ")
    assert pat.match("%%\n")
    assert not pat.match("%%% three percents")
    assert not pat.match("% single comment")
    assert not pat.match("x = 1;")


def test_cell_header_function():
    pat = PVU.cell_header_pattern
    assert pat.match("function y = foo(x)")
    assert pat.match("  function y = foo(x)")
    assert pat.match("classdef MyClass")
    assert not pat.match("% function y = foo(x)")


# ---------------------------------------------------------------------------
# Ellipsis (line continuation)
# ---------------------------------------------------------------------------


def test_ellipsis_pattern():
    pat = PVU.ellipsis_pattern
    assert pat.match("x = 1 + ...")
    assert pat.match("x = 1 +...")
    assert pat.match("x = 1 + ...  ")
    assert not pat.match("x = 1;")
    assert not pat.match("...")
    assert not pat.match("   ...")


def test_ellipsis_captures_content():
    pat = PVU.ellipsis_pattern
    m = pat.match("x = 1 + ...")
    assert m.group(1) == "x = 1 +"


# ---------------------------------------------------------------------------
# Variable detection
# ---------------------------------------------------------------------------


def test_variable_pattern():
    pat = PVU.variable_pattern
    matches = [m.group(0) for m in pat.finditer("myVar + obj.field")]
    assert "myVar" in matches
    assert "obj.field" in matches


def test_variable_pattern_underscore():
    pat = PVU.variable_pattern
    matches = [m.group(0) for m in pat.finditer("my_var_2")]
    assert "my_var_2" in matches


# ---------------------------------------------------------------------------
# Function block detection
# ---------------------------------------------------------------------------


def test_function_block_pattern_single():
    pat = PVU.function_block_pattern
    code = "function y = foo(x)\n    y = x + 1;\nend"
    matches = {m.group(1): m.group(0).strip() for m in pat.finditer(code)}
    assert "foo" in matches


def test_function_block_pattern_multiple():
    pat = PVU.function_block_pattern
    code = (
        "function y = foo(x)\n    y = x + 1;\n"
        "function z = bar(x)\n    z = x * 2;\n"
    )
    matches = {m.group(1): m.group(0).strip() for m in pat.finditer(code)}
    assert "foo" in matches
    assert "bar" in matches


def test_function_block_classdef():
    pat = PVU.function_block_pattern
    # classdef without inheritance — name is captured directly
    code = "classdef MyClass\n    properties\n        x\n    end\nend"
    matches = {m.group(1): m.group(0).strip() for m in pat.finditer(code)}
    assert "MyClass" in matches


# ---------------------------------------------------------------------------
# Option line parsing
# ---------------------------------------------------------------------------


def test_option_line_pattern():
    pat = PVU.option_line_pattern
    m = pat.match("%%! vim-matlab: split (helpers)")
    assert m
    assert m.group(1) == "split"
    assert m.group(2).strip() == "helpers"


def test_option_line_pattern_multiple_args():
    pat = PVU.option_line_pattern
    m = pat.match("%%! vim-matlab: split (a, b, c)")
    assert m
    args = [s.strip() for s in m.group(2).split(",")]
    assert args == ["a", "b", "c"]


def test_option_line_pattern_no_match():
    pat = PVU.option_line_pattern
    assert not pat.match("% just a comment")
    assert not pat.match("x = 1;")


# ---------------------------------------------------------------------------
# trim_matlab_code
# ---------------------------------------------------------------------------


def test_trim_removes_comments():
    result = PVU.trim_matlab_code(["x = 1; % set x", "y = 2; % set y"])
    assert result == ["x = 1;", "y = 2;"]


def test_trim_removes_blank_lines():
    result = PVU.trim_matlab_code(["x = 1;", "", "  ", "y = 2;"])
    assert result == ["x = 1;", "y = 2;"]


def test_trim_joins_continuations():
    result = PVU.trim_matlab_code(["x = 1 + ...", "2 + ...", "3;"])
    assert result == ["x = 1 +2 +3;"]


def test_trim_skips_bare_ellipsis():
    result = PVU.trim_matlab_code(["x = 1;", "...", "y = 2;"])
    assert result == ["x = 1;", "y = 2;"]
