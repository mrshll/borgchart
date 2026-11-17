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
    verdict = f"{result}: claude-wake-matrix."
    if result == "PASS":
        verdict = "Every required case passed: this run qualifies a stable rebuild."
    if result == "SKIP":
        verdict = "not qualified (SKIP): claude-wake-matrix. No failures, but the run doesn't qualify the items above."
    rows = [f"| {case} | {result if case == 'claude-wake-matrix' else 'PASS'} | test |" for case in CASES]
    (folder / "matrix.md").write_text(
        f"# Live tier run\n\n- UTC: 2026-01-01T00:00:00.000Z\n- brg: abc1234\n- claude: {version} (Claude Code)\n- codex: codex-cli 0.169.0\n- selection: claude, codex\n\n\n| case | result | detail |\n| --- | --- | --- |\n"
        + "\n".join(rows)
        + f"\n\n**Verdict:** {verdict}\n"
    )


def report(folder: Path, *args: str, **extra: str) -> subprocess.CompletedProcess[str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("BORGCHART_")}
    env.update(GITHUB_REPOSITORY_OWNER="sample-owner", **extra)
    command = ["uv", "run", "--no-project", str(REPORT), str(folder / "latest"), str(folder / "control"), *args]
    return subprocess.run(command, cwd=ROOT, env=env, text=True, capture_output=True, check=False)


def test_real_green_matrix_opens_qualification_and_matches_claude_prefix(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    result = report(tmp_path, "success", "skipped", "--dry-run")
    assert result.returncode == 0, result.stderr
    assert "passed, not on rules/wake.yaml" in result.stdout and "codex 0.169.0" in result.stdout
    assert "claude 2.1.325 passed" not in result.stdout
    # A case that doesn't apply to a version neither fails nor withholds qualification.
    na = tmp_path / "latest/matrix.md"
    na.write_text(na.read_text().replace("| claude-add-dir-status | PASS |", "| claude-add-dir-status | N/A |"))
    assert "passed, not on rules/wake.yaml" in report(tmp_path, "success", "skipped", "--dry-run").stdout
    for outcome, version in (("PASS", "2.10.0"), ("SKIP", "2.2.0")):
        matrix(tmp_path / "latest", outcome, version)
        path = tmp_path / "latest/matrix.md"
        path.write_text(path.read_text().replace("| SKIP | test |", "| SKIP | isn't qualified |"))
        result = report(tmp_path, "success", "skipped", "--dry-run")
        assert f"claude {version}" in result.stdout and "passed, not on" in result.stdout


def test_control_pass_requires_different_versions_and_covers_any_latest_failure(tmp_path: Path) -> None:
    matrix(tmp_path / "control", version="2.1.324")
    na = tmp_path / "control/matrix.md"
    na.write_text(na.read_text().replace("| claude-add-dir-status | PASS |", "| claude-add-dir-status | N/A |"))
    for outcome in ("FAIL", "BLOCKED", "SKIP"):
        matrix(tmp_path / "latest", outcome)
        assert "breaks claude-wake-matrix" in report(tmp_path, "failure", "success", "--dry-run").stdout
        assert "harness red" in report(tmp_path, "success", "skipped", "--dry-run").stdout
    path = tmp_path / "latest/matrix.md"
    path.write_text(path.read_text().replace("| codex-queue | PASS | test |", ""))
    assert "harness red" in report(tmp_path, "success", "skipped", "--dry-run").stdout
    matrix(tmp_path / "control")
    assert "harness red" in report(tmp_path, "failure", "success", "--dry-run").stdout
    assert "harness red" in report(tmp_path, "failure", "failure", "--dry-run").stdout
    (tmp_path / "latest/matrix.md").unlink()
    assert "breaks (no latest evidence)" in report(tmp_path, "failure", "success", "--dry-run").stdout


def test_secret_in_evidence_prevents_any_gh_call(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    for planted in ("sk-test-not-a-key-0123456789abcdef", "sk-svcacct-fake", "0123456789abcdef"):
        (tmp_path / "latest/screen.txt").write_text(planted)
        result = report(
            tmp_path, "success", "skipped", "--dry-run", OPENAI_API_KEY="sk-test-not-a-key-0123456789abcdef"
        )
        assert result.returncode != 0 and "gh" not in result.stdout and "Refusing publication" in result.stderr
        assert planted not in result.stdout + result.stderr


def test_seed_keeps_keys_private_and_scrub_blocks_planted_copy(tmp_path: Path) -> None:
    fake = tmp_path / "codex"
    fake.write_text(
        '#!/bin/sh\ncase "$1" in\nlogin) cat > "$HOME/.codex/auth.json"; echo "login diagnostic" >&2;;\nexec) echo OK;;\n*) exit 1;;\nesac\n'
    )
    fake.chmod(0o700)
    claude = tmp_path / "claude"
    # Like Claude, --tools takes every argument after it, so the prompt must come before it.
    claude.write_text(
        """#!/bin/sh
print= prompt=
while [ $# -gt 0 ] && [ "$1" != --tools ]; do
  case $1 in -p|--print) print=1 ;; -*) ;; *) prompt=$1 ;; esac
  shift
done
[ -n "$print" ] && [ -n "$prompt" ] && [ "$#" = 2 ] && [ -z "$2" ] || { echo "no prompt" >&2; exit 1; }
echo OK
"""
    )
    claude.chmod(0o700)
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "TMPDIR": str(tmp_path),
        "GITHUB_WORKSPACE": str(ROOT),
        "PATH": f"{tmp_path}:{os.environ['PATH']}",
        "ANTHROPIC_API_KEY": "sk-test-not-a-key-0123456789abcdef",
        "OPENAI_API_KEY": "sk-test-not-a-key-0123456789abcdef",
    }
    seed = ROOT / "tools/vendor-nightly/seed"
    for key in ("ANTHROPIC_API_KEY", "OPENAI_API_KEY"):
        for value in ("", None):
            missing = {k: v for k, v in (env | {key: value}).items() if v is not None}
            result = subprocess.run([str(seed)], env=missing, text=True, capture_output=True, check=False)
            assert result.returncode != 0 and key in result.stderr
    result = subprocess.run([str(seed)], env=env, text=True, capture_output=True, check=False)
    assert result.returncode == 0 and result.stdout == "" and result.stderr == "login diagnostic\n"
    for file in (home / ".claude/api-key", home / ".codex/auth.json", home / ".claude/settings.json"):
        assert file.stat().st_mode & 0o777 == 0o600
    evidence = tmp_path / "evidence"
    evidence.mkdir()
    assert (
        subprocess.run([str(seed), "--scrub", str(evidence)], env=env, capture_output=True, check=False).returncode == 0
    )
    for planted in ("sk-test-not-a-key-0123456789abcdef", "sk-svcacct-fake", "0123456789abcdef"):
        (evidence / "screen").write_text(planted)
        result = subprocess.run(
            [str(seed), "--scrub", str(evidence)], env=env, text=True, capture_output=True, check=False
        )
        assert result.returncode == 1 and planted not in result.stdout + result.stderr
    good_claude, good_codex = claude.read_text(), fake.read_text()
    claude.write_text('#!/bin/sh\nprintf "Startup banner\\nRequest denied: %s\\n\\n" "$ANTHROPIC_API_KEY" >&2\nexit 1\n')
    result = subprocess.run([str(seed)], env=env, text=True, capture_output=True, check=False)
    assert result.returncode != 0 and "API request refused for claude: Request denied:" in result.stderr
    assert "[redacted]" in result.stderr and env["ANTHROPIC_API_KEY"] not in result.stdout + result.stderr
    # Refusals as the CLIs print them (recorded against a local server), and an exit 0 without an answer.
    for vendor, script, message in (
        ("claude", 'echo "Credit balance is too low"; exit 1', "claude API key is out of credit: Credit balance is too low"),
        (
            "codex",
            'printf "Reply OK.\\nERROR: Quota exceeded. Check your plan and billing details.\\n" >&2; exit 1',
            "codex API key is out of credit: ERROR: Quota exceeded.",
        ),
        ("claude", 'echo "Reply OK." >&2; echo "Hello"', "API request refused for claude: Hello"),
    ):
        claude.write_text(f"#!/bin/sh\n{script}\n" if vendor == "claude" else good_claude)
        fake.write_text(good_codex.replace("echo OK", script) if vendor == "codex" else good_codex)
        result = subprocess.run([str(seed)], env=env, text=True, capture_output=True, check=False)
        assert result.returncode == 1 and message in result.stderr, result.stderr


def test_repeats_comment_and_cleared_kinds_close_without_creating_duplicates(tmp_path: Path) -> None:
    matrix(tmp_path / "latest")
    fake = tmp_path / "gh"
    record = tmp_path / "calls"
    fake.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$*" >> "$RECORD"\nif [ "$2" = list ]; then\n echo \'[{"number":7,"body":"<!-- vendor-nightly:qualification -->"},{"number":8,"body":"<!-- vendor-nightly:harness -->"},{"number":9,"body":"<!-- vendor-nightly:regression -->"}]\'\nelse\n[ "$2" != comment ] || cat "$5" >> "$RECORD"\nfi\n'
    )
    fake.chmod(0o700)
    result = report(tmp_path, "success", "skipped", PATH=f"{tmp_path}:{os.environ['PATH']}", RECORD=str(record))
    assert result.returncode == 0, result.stderr
    calls = record.read_text()
    assert "comment 7" in calls and "comment 8" in calls and "close 8" in calls
    assert "create" not in calls and "close 7" not in calls and "@sample-owner" in calls
    matrix(tmp_path / "latest", "BLOCKED")
    matrix(tmp_path / "control", version="2.1.324")
    for latest in ("failure", "cancelled"):
        for control in ("failure", "success", "cancelled"):
            record.unlink(missing_ok=True)
            report(tmp_path, latest, control, PATH=f"{tmp_path}:{os.environ['PATH']}", RECORD=str(record))
            calls = record.read_text() if record.exists() else ""
            assert "close" not in (calls.replace("close 8", "") if control == "success" else calls)
            assert "cancelled" not in (latest, control) or not calls
