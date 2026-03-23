"""Tests for io_helper module."""

import os

from vim_matlab import io_helper


def test_find_plugin_matlab_path():
    path = io_helper.find_plugin_matlab_path()
    assert os.path.isdir(path)
    assert path.endswith("matlab")


def test_matlab_dir_contains_m_files():
    path = io_helper.find_plugin_matlab_path()
    m_files = [f for f in os.listdir(path) if f.endswith(".m")]
    assert len(m_files) > 0
