"""Check the nightly's publication boundary without credentials or network."""

import os
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REPORT = ROOT / "tools/vendor-nightly/report"
CASES = re.findall(r'\("([\w-]+)", Who::', (ROOT / "tests/live/rules.rs").read_text())


def matrix(folder: Path, result: str = "PASS", version: str = "2.1.325") -> None:
    folder.mkdir(exist_ok=True)
    rows = [f"| {case} | {result if case == 'claude-wake-matrix' else 'PASS'} | test |" for case in CASES]
    (folder / "matrix.md").write_text(
        f"# Live tier run\n- claude: {version} (Claude Code)\n- codex: codex-cli 0.169.0\n- selection: claude, codex\n"
        + "\n".join(rows)
        + "\n**Verdict:** Every required case passed: this run qualifies a stable rebuild.\n"
    )


def report(folder: Path, *args: str, **extra: str) -> subprocess.CompletedProcess[str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BORGCHART_")}
    env.update(extra)
    return subprocess.run(
        ["uv", "run", "--no-project", str(REPORT), str(folder / "latest"), str(folder / "control"), *args],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_complete_pass_flags_unverified_codex_but_matches_claude_prefix(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    result = report(tmp_path, "success", "skipped", "--dry-run")
    assert result.returncode == 0, result.stderr
    assert "passed, not on rules/wake.yaml" in result.stdout and "codex 0.169.0" in result.stdout
    assert "claude 2.1.325 passed" not in result.stdout
    matrix(tmp_path / "latest", version="2.10.0")
    assert "claude 2.10.0" in report(tmp_path, "success", "skipped", "--dry-run").stdout


def test_control_pass_distinguishes_regression_from_harness_failure(tmp_path: Path) -> None:
    matrix(tmp_path / "latest", "FAIL")
    matrix(tmp_path / "control")
    assert "breaks claude-wake-matrix" in report(tmp_path, "failure", "success", "--dry-run").stdout
    assert "harness red on verified versions" in report(tmp_path, "failure", "failure", "--dry-run").stdout
    (tmp_path / "latest/matrix.md").unlink()
    assert "harness red on verified versions" in report(tmp_path, "failure", "success", "--dry-run").stdout


def test_partial_or_skipped_matrix_cannot_claim_qualification(tmp_path: Path) -> None:
    matrix(tmp_path / "latest", "SKIP")
    assert "passed, not on" not in report(tmp_path, "success", "skipped", "--dry-run").stdout
    path = tmp_path / "latest/matrix.md"
    path.write_text(path.read_text().replace("| codex-queue | PASS | test |", ""))
    assert "harness red" in report(tmp_path, "success", "skipped", "--dry-run").stdout


def test_secret_in_evidence_prevents_any_gh_call(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    (tmp_path / "latest/screen.txt").write_text("sk-test-not-a-key")
    result = report(tmp_path, "success", "skipped", "--dry-run", OPENAI_API_KEY="sk-test-not-a-key")
    assert result.returncode != 0 and "gh" not in result.stdout
    assert "Refusing publication" in result.stderr
    assert "sk-test-not-a-key" not in result.stdout + result.stderr


def test_seed_keeps_keys_private_and_scrub_blocks_planted_copy(tmp_path: Path) -> None:
    fake = tmp_path / "codex"
    fake.write_text('#!/bin/sh\ncat > "$HOME/.codex/auth.json"\n')
    fake.chmod(0o700)
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "TMPDIR": str(tmp_path),
        "GITHUB_WORKSPACE": str(ROOT),
        "PATH": f"{tmp_path}:{os.environ['PATH']}",
        "ANTHROPIC_API_KEY": "sk-test-not-a-key",
        "OPENAI_API_KEY": "sk-test-not-a-key",
    }
    seed = ROOT / "tools/vendor-nightly/seed"
    result = subprocess.run([str(seed)], env=env, text=True, capture_output=True, check=False)
    assert result.returncode == 0 and "sk-test-not-a-key" not in result.stdout + result.stderr
    for file in (home / ".claude/api-key", home / ".codex/auth.json", home / ".claude/settings.json"):
        assert file.stat().st_mode & 0o777 == 0o600
    evidence = tmp_path / "evidence"
    evidence.mkdir()
    assert (
        subprocess.run([str(seed), "--scrub", str(evidence)], env=env, capture_output=True, check=False).returncode == 0
    )
    (evidence / "screen").write_text("sk-test-not-a-key")
    result = subprocess.run([str(seed), "--scrub", str(evidence)], env=env, text=True, capture_output=True, check=False)
    assert result.returncode == 1 and "sk-test-not-a-key" not in result.stdout + result.stderr


def test_repeats_comment_and_cleared_kinds_close_without_creating_duplicates(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    fake = tmp_path / "gh"
    fake.write_text(
        '#!/bin/sh\nif [ "$2" = list ]; then\n echo \'[{"number":7,"body":"<!-- vendor-nightly:qualification -->"},{"number":8,"body":"<!-- vendor-nightly:harness -->"}]\'\nelse\n printf "%s\\n" "$*" >> "$RECORD"\nfi\n'
    )
    fake.chmod(0o700)
    result = report(
        tmp_path, "success", "skipped", PATH=f"{tmp_path}:{os.environ['PATH']}", RECORD=str(tmp_path / "calls")
    )
    assert result.returncode == 0, result.stderr
    calls = (tmp_path / "calls").read_text()
    assert "comment 7" in calls and "comment 8" in calls and "close 8" in calls
    assert "create" not in calls and "close 7" not in calls


def test_unknown_claude_wake_skip_requests_qualification_without_claiming_typed_pass(tmp_path: Path) -> None:
    matrix(tmp_path / "latest", "SKIP", "2.2.0")
    path = tmp_path / "latest/matrix.md"
    path.write_text(path.read_text().replace("| SKIP | test |", "| SKIP | isn't qualified |"))
    result = report(tmp_path, "success", "skipped", "--dry-run")
    assert result.returncode == 0 and "claude 2.2.0" in result.stdout and "passed, not on" in result.stdout
