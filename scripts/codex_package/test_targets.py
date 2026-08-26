"""Tests for package target and repository-root discovery."""

from pathlib import Path

from scripts.codex_package.targets import resolve_repo_root


def test_repo_root_prefers_environment_override() -> None:
    assert resolve_repo_root(
        {"CODEX_REPO_ROOT": "/work/codex"},
        Path("/runfiles/codex/scripts/codex_package/targets.py"),
    ) == Path("/work/codex")


def test_repo_root_falls_back_to_stable_script_layout() -> None:
    assert resolve_repo_root(
        {}, Path("/work/codex/scripts/codex_package/targets.py")
    ) == Path("/work/codex")
