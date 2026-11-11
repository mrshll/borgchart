"""Install into isolated prefixes with a stand-in release build."""

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
PROGRAMS = ("brg", "borgchart", "brg-seat")


def run(cwd: Path, env: dict[str, str], *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args, cwd=cwd, env=env, capture_output=True, text=True, check=False, timeout=15
    )


def executable(path: Path, text: str) -> None:
    path.write_text("#!/bin/sh\nset -eu\n" + text)
    path.chmod(0o755)


@pytest.fixture
def checkout(tmp_path: Path) -> tuple[Path, dict[str, str]]:
    repo = tmp_path / "checkout"
    (repo / "tools").mkdir(parents=True)
    (repo / "teams" / "example").mkdir(parents=True)
    (repo / "teams" / "example" / "role.md").write_text("original prompt\n")
    (repo / ".gitignore").write_text("/target/\n")
    shutil.copy2(ROOT / "Makefile", repo)
    shutil.copy2(ROOT / "tools" / "install", repo / "tools")
    executable(
        repo / "tools" / "cargo-slot", 'echo wrapped >> "$BUILD_LOG"\nexec "$@"\n'
    )
    commands = tmp_path / "commands"
    commands.mkdir()
    executable(
        commands / "cargo",
        '[ "$*" = "build --release --workspace --bins --locked" ] ||\n'
        '[ "$*" = "build --release --workspace --bins --locked --message-format=json-render-diagnostics" ]\n'
        'echo build >> "$BUILD_LOG"\n'
        'output=${BUILD_OUTPUT:-"$(pwd)/target/release"}\n'
        'mkdir -p "$output"\n'
        "for name in brg borgchart brg-seat; do\n"
        "  printf '#!/bin/sh\\necho installed\\n' > \"$output/$name\"\n"
        '  chmod +x "$output/$name"\n'
        '  if [ "${OMIT_ARTIFACT:-}" != "$name" ]; then\n'
        '    printf \'{"executable": "%s/%s", "reason": "compiler-artifact"}\\n\' "$output" "$name"\n'
        "  fi\n"
        "done\n",
    )
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("BORGCHART_", "GIT_"))
        and key
        not in (
            "ALLOW_DIRTY",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "BUILD_OUTPUT",
            "OMIT_ARTIFACT",
            "MAKEFLAGS",
            "MFLAGS",
            "CDPATH",
        )
    }
    env.update(
        PREFIX=str(tmp_path / "install prefix"),
        BINDIR=str(tmp_path / "install bin"),
        PATH=f"{commands}:{env['PATH']}",
        BUILD_LOG=str(tmp_path / "build.log"),
        HOME=str(tmp_path / "home"),
    )
    Path(env["HOME"]).mkdir()
    for args in (
        ("init", "-q"),
        ("config", "user.name", "Example"),
        ("config", "user.email", "example@example.invalid"),
        ("config", "commit.gpgsign", "false"),
        ("add", "."),
        ("-c", "core.hooksPath=/dev/null", "commit", "-qm", "Initial example"),
    ):
        result = run(repo, env, "git", *args)
        assert result.returncode == 0, result.stderr
    return repo, env


@pytest.mark.parametrize(
    "platform,wrapped", [("Linux", True), ("Linux", False), ("Darwin", False)]
)
def test_upgrade_keeps_old_revision_and_repeat_does_not_rewrite_it(
    checkout: tuple[Path, dict[str, str]], platform: str, wrapped: bool
) -> None:
    repo, env = checkout
    commands = Path(env["PATH"].split(":")[0])
    executable(commands / "uname", f"echo {platform}\n")
    if platform == "Darwin":
        # Exercise the Darwin branch on Linux; Apple's mv source separately
        # confirms -h uses rename on the destination symlink itself.
        executable(
            commands / "mv",
            'if [ "$1" = -fh ]; then shift; exec /bin/mv -fT "$@"; fi\n'
            'exec /bin/mv "$@"\n',
        )
    elif not wrapped:
        (repo / "tools" / "cargo-slot").chmod(0o644)
        assert run(repo, env, "git", "add", ".").returncode == 0
        assert (
            run(
                repo,
                env,
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "-qm",
                "No wrapper",
            ).returncode
            == 0
        )
    prefix, bindir = Path(env["PREFIX"]), Path(env["BINDIR"])
    revision = run(repo, env, "git", "rev-parse", "HEAD").stdout.strip()
    first = run(repo, env, "make", "install")
    assert first.returncode == 0, first.stderr
    assert first.stdout.splitlines().count(f"teams: [{prefix}/brg-current/teams]") == 1
    old = prefix / f"brg-{revision}"
    assert (old / "REVISION").read_text() == f"{revision}\n"
    assert (old / "teams/example/role.md").read_text() == "original prompt\n"
    original_inode = (old / "bin/brg").stat().st_ino
    link_inodes = {name: (bindir / name).lstat().st_ino for name in PROGRAMS}
    for name in PROGRAMS:
        assert (bindir / name).readlink() == prefix / "brg-current/bin" / name
        assert run(repo, env, str(bindir / name)).stdout == "installed\n"
    assert ("wrapped" in Path(env["BUILD_LOG"]).read_text()) == wrapped
    assert run(repo, env, "make", "install").returncode == 0
    assert (old / "bin/brg").stat().st_ino == original_inode
    assert {name: (bindir / name).lstat().st_ino for name in PROGRAMS} == link_inodes
    (repo / "teams/example/role.md").write_text("updated prompt\n")
    assert run(repo, env, "git", "add", ".").returncode == 0
    assert (
        run(
            repo,
            env,
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "Update example",
        ).returncode
        == 0
    )
    revision = run(repo, env, "git", "rev-parse", "HEAD").stdout.strip()
    upgraded = run(repo, env, "make", "install")
    assert upgraded.returncode == 0, upgraded.stderr
    assert (prefix / "brg-current").readlink() == Path(f"brg-{revision}")
    assert (
        prefix / "brg-current/teams/example/role.md"
    ).read_text() == "updated prompt\n"
    assert (old / "bin/brg").stat().st_ino == original_inode
    assert not list(old.glob("brg-*"))
    assert not list(old.glob(".brg-*"))


def test_dirty_checkout_refuses_until_override_and_never_reuses_dirty_install(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    (repo / "untracked").touch()
    refused = run(repo, env, "make", "install")
    assert refused.returncode != 0
    assert not Path(env["BUILD_LOG"]).exists()
    assert not Path(env["PREFIX"]).exists()
    env["ALLOW_DIRTY"] = "1"
    prefix = Path(env["PREFIX"])
    revision = run(repo, env, "git", "rev-parse", "HEAD").stdout.strip()
    previous = None
    for _ in range(2):
        installed = run(repo, env, "make", "install")
        assert installed.returncode == 0, installed.stderr
        current = (prefix / "brg-current").resolve()
        assert current.name.startswith(f"brg-{revision}-dirty-")
        assert "dirty" in (current / "REVISION").read_text()
        assert current != previous
        if previous is not None:
            assert previous.is_dir()
        previous = current


@pytest.mark.parametrize("name", PROGRAMS)
def test_existing_regular_binary_is_preserved_without_switching_current(
    checkout: tuple[Path, dict[str, str]], name: str
) -> None:
    repo, env = checkout
    assert run(repo, env, "make", "install").returncode == 0
    current = (Path(env["PREFIX"]) / "brg-current").readlink()
    binary = Path(env["BINDIR"]) / name
    binary.unlink()
    binary.write_text("keep this file\n")
    env["ALLOW_DIRTY"] = "1"
    (repo / "untracked").touch()
    assert run(repo, env, "make", "install").returncode != 0
    assert binary.read_text() == "keep this file\n"
    assert (Path(env["PREFIX"]) / "brg-current").readlink() == current


def test_incomplete_copy_never_publishes_revision_or_switches_current(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    assert run(repo, env, "make", "install").returncode == 0
    prefix = Path(env["PREFIX"])
    previous = (prefix / "brg-current").readlink()
    (repo / "teams/example/role.md").write_text("new prompt\n")
    assert run(repo, env, "git", "add", ".").returncode == 0
    assert (
        run(
            repo,
            env,
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "Update example",
        ).returncode
        == 0
    )
    revision = run(repo, env, "git", "rev-parse", "HEAD").stdout.strip()
    commands = Path(env["PATH"].split(":")[0])
    executable(commands / "cp", "exit 42\n")
    assert run(repo, env, "make", "install").returncode != 0
    assert not (prefix / f"brg-{revision}").exists()
    assert (prefix / "brg-current").readlink() == previous
    assert not list(prefix.glob(".brg-install.*"))


def test_defaults_install_under_isolated_home(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    env.pop("PREFIX")
    env.pop("BINDIR")
    result = run(repo, env, "make", "install")
    assert result.returncode == 0, result.stderr
    home = Path(env["HOME"])
    prefix = home / ".local/share"
    assert (prefix / "brg-current/bin/brg").is_file()
    assert (home / ".local/bin/brg").readlink() == prefix / "brg-current/bin/brg"
    assert result.stdout.splitlines().count(f"teams: [{prefix}/brg-current/teams]") == 1


def test_reported_artifact_path_wins_over_stale_default_build(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    stale = repo / "target/release"
    stale.mkdir(parents=True)
    for name in PROGRAMS:
        executable(stale / name, "echo stale\n")
    env["BUILD_OUTPUT"] = str(repo / "target/other directory/example-triple/release")
    result = run(repo, env, "make", "install")
    assert result.returncode == 0, result.stderr
    for name in PROGRAMS:
        installed = run(repo, env, str(Path(env["BINDIR"]) / name))
        assert installed.stdout == "installed\n"


def test_held_install_lock_refuses_without_removing_another_installers_lock(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    prefix = Path(env["PREFIX"])
    lock = prefix / ".brg-install.lock"
    lock.mkdir(parents=True)
    result = run(repo, env, "make", "install")
    assert result.returncode != 0
    assert lock.is_dir()
    assert not (prefix / "brg-current").exists()
    assert not Path(env["BUILD_LOG"]).exists()


@pytest.mark.parametrize("name", PROGRAMS)
def test_missing_artifact_refuses_publication_and_releases_owned_lock(
    checkout: tuple[Path, dict[str, str]], name: str
) -> None:
    repo, env = checkout
    env["OMIT_ARTIFACT"] = name
    result = run(repo, env, "make", "install")
    assert result.returncode != 0
    prefix = Path(env["PREFIX"])
    assert not (prefix / "brg-current").exists()
    assert not list(prefix.glob("brg-*"))
    assert not (prefix / ".brg-install.lock").exists()


def test_json_escaped_artifact_paths_refuse_instead_of_copying_stale_binaries(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    output = repo / 'target/quoted"directory/release'
    output.mkdir(parents=True)
    messages = []
    for name in PROGRAMS:
        executable(output / name, "echo fresh\n")
        messages.append(
            json.dumps(
                {"reason": "compiler-artifact", "executable": str(output / name)},
                separators=(",", ":"),
            )
        )
    commands = Path(env["PATH"].split(":")[0])
    executable(
        commands / "cargo",
        "cat <<'ARTIFACTS'\n" + "\n".join(messages) + "\nARTIFACTS\n",
    )
    result = run(repo, env, "make", "install")
    assert result.returncode != 0
    prefix = Path(env["PREFIX"])
    assert not (prefix / "brg-current").exists()
    assert not list(prefix.glob("brg-*"))
    assert not (prefix / ".brg-install.lock").exists()


def test_direct_install_from_elsewhere_handles_cdpath_and_relative_destinations(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    env.update(PREFIX="target/install share", BINDIR="target/install bin", CDPATH="/")
    result = run(repo.parent, env, str(repo / "tools/install"))
    assert result.returncode == 0, result.stderr
    prefix = repo / env["PREFIX"]
    assert (repo / env["BINDIR"] / "brg").readlink() == prefix / "brg-current/bin/brg"
    assert result.stdout.splitlines().count(f"teams: [{prefix}/brg-current/teams]") == 1


def test_clean_install_archives_head_while_dirty_override_copies_working_teams(
    checkout: tuple[Path, dict[str, str]],
) -> None:
    repo, env = checkout
    prompt = repo / "teams/example/role.md"
    prompt.write_text("unrecorded prompt\n")
    # Model a worktree edit hidden from status; the clean install still uses HEAD.
    assert (
        run(
            repo, env, "git", "update-index", "--assume-unchanged", str(prompt)
        ).returncode
        == 0
    )
    result = run(repo, env, "make", "install")
    assert result.returncode == 0, result.stderr
    prefix = Path(env["PREFIX"])
    assert (
        prefix / "brg-current/teams/example/role.md"
    ).read_text() == "original prompt\n"
    assert (
        run(
            repo, env, "git", "update-index", "--no-assume-unchanged", str(prompt)
        ).returncode
        == 0
    )
    env["ALLOW_DIRTY"] = "1"
    result = run(repo, env, "make", "install")
    assert result.returncode == 0, result.stderr
    assert (
        prefix / "brg-current/teams/example/role.md"
    ).read_text() == "unrecorded prompt\n"
