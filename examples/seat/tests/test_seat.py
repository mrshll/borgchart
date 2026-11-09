"""Tests for the seat plugin with fake usage sources.

Run from the repo root:
    uv run --no-project --with pytest==8.3.5 pytest examples/seat/tests
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

PLUGIN = Path(__file__).resolve().parent.parent
GO, BAD_ARGUMENTS, WAIT = 0, 2, 3


def install(tmp_path: Path, source: str | None) -> Path:
    """A copy of the plugin in its own folder, with `source` as its usage
    source (a shell script body), or none."""
    folder = tmp_path / "seat"
    folder.mkdir(parents=True)
    shutil.copy(PLUGIN / "seat", folder / "seat")
    if source is not None:
        path = folder / "usage-source"
        path.write_text("#!/bin/sh\n" + source)
        path.chmod(0o755)
    return folder


def seat(folder: Path, *args: str) -> tuple[int, str]:
    done = subprocess.run(
        [sys.executable, str(folder / "seat"), *args],
        capture_output=True,
        check=False,
        text=True,
        timeout=60,
    )
    return done.returncode, done.stdout


def reading(text: str) -> str:
    return f"cat <<'JSON'\n{text}\nJSON\n"


def test_usage_under_the_threshold_is_go(tmp_path: Path) -> None:
    """Catches a plugin that never says go, or says it without a reason."""
    folder = install(tmp_path, reading('{"used_percent": 12.5}'))
    code, out = seat(folder, "route", "claude", "opus")
    assert code == GO
    assert out.startswith("go: ") and "12.5%" in out
    assert len(out.splitlines()) == 1


def test_usage_at_or_over_the_threshold_is_wait_with_the_reset(tmp_path: Path) -> None:
    """Catches a go at the default threshold, and a wait that hides when
    usage resets."""
    for used in (80, 97):
        folder = install(
            tmp_path / str(used),
            reading(f'{{"used_percent": {used}, "resets_at": "2026-01-01T06:00:00Z"}}'),
        )
        code, out = seat(folder, "route", "codex", "gpt-6.1-sol")
        assert code == WAIT, out
        assert out.startswith("wait: ") and "2026-01-01T06:00:00Z" in out


def test_a_threshold_file_replaces_the_default(tmp_path: Path) -> None:
    """Catches a threshold file being ignored, and a bad one read as go."""
    folder = install(tmp_path, reading('{"used_percent": 30}'))
    assert seat(folder, "route", "claude", "opus")[0] == GO
    (folder / "threshold").write_text("25\n")
    assert seat(folder, "route", "claude", "opus")[0] == WAIT
    for bad in ("lots", "0", "250"):
        (folder / "threshold").write_text(bad)
        code, out = seat(folder, "route", "claude", "opus")
        assert (code, out[:6]) == (WAIT, "wait: "), bad


def test_a_broken_threshold_link_is_wait_not_the_default(tmp_path: Path) -> None:
    """Catches an installed threshold whose link target is gone being read as
    no threshold, so the looser default answers go."""
    folder = install(tmp_path, reading('{"used_percent": 30}'))
    target = tmp_path / "policy"
    target.write_text("25\n")
    (folder / "threshold").symlink_to(target)
    assert seat(folder, "route", "claude", "opus")[0] == WAIT
    target.unlink()
    code, out = seat(folder, "route", "claude", "opus")
    assert (code, out[:6]) == (WAIT, "wait: "), out


def test_unknown_usage_is_wait(tmp_path: Path) -> None:
    """Catches unknown usage (null) taken for zero."""
    folder = install(tmp_path, reading('{"used_percent": null}'))
    code, out = seat(folder, "route", "claude", "opus")
    assert code == WAIT
    assert out.startswith("wait: ") and "unknown" in out


def test_a_failing_source_is_wait(tmp_path: Path) -> None:
    """Catches a failing source's partial output being trusted."""
    folder = install(
        tmp_path,
        'echo \'{"used_percent": 1}\'\necho "quota service down" >&2\nexit 1\n',
    )
    code, out = seat(folder, "route", "claude", "opus")
    assert code == WAIT
    assert out.startswith("wait: ") and "quota service down" in out


def test_a_missing_source_is_wait(tmp_path: Path) -> None:
    """Catches a plugin with no source installed answering go."""
    folder = install(tmp_path, None)
    code, out = seat(folder, "route", "claude", "opus")
    assert code == WAIT
    assert out.startswith("wait: ") and "usage-source" in out


@pytest.mark.parametrize(
    "output",
    [
        "not json",
        "[12]",
        "{}",
        '{"used_percent": "12"}',
        '{"used_percent": true}',
        '{"used_percent": -1}',
        '{"used_percent": 150}',
        '{"used_percent": NaN}',
    ],
)
def test_unreadable_output_is_wait(tmp_path: Path, output: str) -> None:
    """Catches output that isn't a usage reading being read as low usage."""
    folder = install(tmp_path, reading(output))
    code, out = seat(folder, "route", "claude", "opus")
    assert (code, out[:6]) == (WAIT, "wait: "), out


@pytest.mark.parametrize(
    "args",
    [
        [],
        ["route"],
        ["route", "claude"],
        ["route", "claude", "opus", "high"],
        ["check", "claude", "opus"],
        ["route", "../claude", "opus"],
        ["route", "claude", "opus; rm -rf x"],
        ["route", "", "opus"],
    ],
)
def test_bad_arguments_are_refused_as_wait(tmp_path: Path, args: list[str]) -> None:
    """Catches a mistyped call reaching the source or reading as go."""
    folder = install(tmp_path, "echo ran > ran\necho '{\"used_percent\": 1}'\n")
    code, out = seat(folder, *args)
    assert (code, out[:6]) == (BAD_ARGUMENTS, "wait: "), out
    assert not (folder / "ran").exists()


def test_the_source_gets_the_vendor_and_model_and_runs_in_its_folder(
    tmp_path: Path,
) -> None:
    """Catches the plugin asking about the wrong member, or a source that
    can't find its own files."""
    folder = install(
        tmp_path, 'echo "$(pwd -P) $*" > asked\necho \'{"used_percent": 5}\'\n'
    )
    assert seat(folder, "route", "codex", "gpt-6.1-sol")[0] == GO
    assert (folder / "asked").read_text() == f"{folder.resolve()} codex gpt-6.1-sol\n"


def test_the_shipped_example_source_answers_go(tmp_path: Path) -> None:
    """Catches the example source drifting from the contract seat reads."""
    folder = install(tmp_path, None)
    shutil.copy(PLUGIN / "usage-source.example", folder / "usage-source")
    code, out = seat(folder, "route", "claude", "opus")
    assert code == GO, out
