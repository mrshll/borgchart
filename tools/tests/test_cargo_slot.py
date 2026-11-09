"""Exercise the real flock domain with only systemd-run replaced."""

import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

WRAPPER = Path(__file__).resolve().parents[1] / "cargo-slot"
pytestmark = pytest.mark.skipif(shutil.which("flock") is None, reason="requires flock")


@pytest.fixture
def environment(tmp_path: Path) -> dict[str, str]:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "systemd-run"
    fake.write_text(
        "#!/bin/sh\n"
        'printf "%s\\n" "$@" > "$RECORD"\n'
        "[ ! -e /proc/self/fd/9 ] || exit 99\n"
        '[ "$1 $2 $3 $4 $5 $6 $7" = '
        '"--user --scope --quiet -p MemoryMax=8G -p MemorySwapMax=0" ] || exit 98\n'
        'shift 7\nexec "$@"\n'
    )
    fake.chmod(0o755)
    env = dict(os.environ)
    env.pop("CARGO_BUILD_JOBS", None)
    env.update(
        PATH=f"{bin_dir}:{os.environ['PATH']}",
        XDG_RUNTIME_DIR=str(tmp_path / "runtime"),
        XDG_STATE_HOME=str(tmp_path / "state"),
        RECORD=str(tmp_path / "args"),
    )
    return env


def wait_for(path: Path) -> None:
    deadline = time.monotonic() + 10
    while not path.exists():
        assert time.monotonic() < deadline, f"timed out waiting for {path}"
        time.sleep(0.02)


@pytest.mark.parametrize("jobs", [None, "3"])
def test_options_jobs_arguments_and_status(
    environment: dict[str, str], jobs: str | None
) -> None:
    if jobs is not None:
        environment["CARGO_BUILD_JOBS"] = jobs
    result = subprocess.run(
        [
            str(WRAPPER),
            "sh",
            "-c",
            'printf "%s:%s" "$CARGO_BUILD_JOBS" "$1"; exit 37',
            "sh",
            "a b",
        ],
        env=environment,
        check=False,
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 37
    assert result.stdout == f"{jobs or '6'}:a b"
    assert Path(environment["RECORD"]).read_text().splitlines()[:7] == [
        "--user",
        "--scope",
        "--quiet",
        "-p",
        "MemoryMax=8G",
        "-p",
        "MemorySwapMax=0",
    ]


@pytest.mark.parametrize("location", ["runtime", "state", "home"])
def test_two_slots_shared_across_working_directories(
    environment: dict[str, str],
    tmp_path: Path,
    location: str,
) -> None:
    if location != "runtime":
        environment.pop("XDG_RUNTIME_DIR")
    if location == "home":
        environment.pop("XDG_STATE_HOME")
        environment["HOME"] = str(tmp_path / "home")
    root = {
        "runtime": tmp_path / "runtime",
        "state": tmp_path / "state",
        "home": tmp_path / "home" / ".local" / "state",
    }[location]
    processes = []
    try:
        for index in range(3):
            cwd = tmp_path / str(index)
            cwd.mkdir()
            processes.append(
                subprocess.Popen(
                    [
                        str(WRAPPER),
                        "sh",
                        "-c",
                        'touch "$1"; while [ ! -e "$2" ]; do sleep .05; done',
                        "sh",
                        str(tmp_path / f"started-{index}"),
                        str(tmp_path / f"release-{index}"),
                    ],
                    cwd=cwd,
                    env=environment,
                )
            )
            if index < 2:
                wait_for(tmp_path / f"started-{index}")
        time.sleep(0.3)
        assert not (tmp_path / "started-2").exists()
        assert sorted(
            path.name for path in (root / "borgchart" / "cargo-slots").iterdir()
        ) == [
            "cargo-slot-0",
            "cargo-slot-1",
        ]
        (tmp_path / "release-0").touch()
        assert processes[0].wait(timeout=10) == 0
        wait_for(tmp_path / "started-2")
    finally:
        for index in range(3):
            (tmp_path / f"release-{index}").touch()
        for process in processes:
            assert process.wait(timeout=10) == 0


def test_background_child_does_not_hold_slot(
    environment: dict[str, str], tmp_path: Path
) -> None:
    # A background child survives its command; neither it nor systemd-run gets fd 9.
    pidfile = tmp_path / "pid"
    subprocess.run(
        [
            str(WRAPPER),
            "sh",
            "-c",
            'setsid sleep 30 >/dev/null 2>&1 & echo $! > "$1"',
            "sh",
            str(pidfile),
        ],
        env=environment,
        check=True,
        timeout=10,
    )
    pid = int(pidfile.read_text())
    try:
        os.kill(pid, 0)
        locks = list(Path(environment["XDG_RUNTIME_DIR"]).rglob("cargo-slot-*"))
        assert locks
        for lock in locks:
            result = subprocess.run(
                ["flock", "-n", str(lock), "true"], check=False, timeout=5
            )
            assert result.returncode == 0
    finally:
        os.kill(pid, 15)


@pytest.mark.parametrize("missing", ["flock", "systemd-run"])
def test_missing_dependency_fails_closed(
    environment: dict[str, str], tmp_path: Path, missing: str
) -> None:
    isolated = tmp_path / "isolated"
    isolated.mkdir()
    for name in ("flock", "systemd-run", "mkdir", "sleep"):
        if name != missing:
            source = shutil.which(name, path=environment["PATH"])
            assert source is not None
            (isolated / name).symlink_to(source)
    environment["PATH"] = str(isolated)
    result = subprocess.run(
        [str(WRAPPER), "/bin/true"],
        env=environment,
        check=False,
        capture_output=True,
        timeout=5,
    )
    assert result.returncode != 0
    assert not Path(environment["RECORD"]).exists()


@pytest.mark.parametrize("dependency", ["flock", "systemd-run"])
def test_dependency_error_is_not_retried_or_bypassed(
    environment: dict[str, str],
    tmp_path: Path,
    dependency: str,
) -> None:
    failing = tmp_path / "bin" / dependency
    failing.write_text("#!/bin/sh\nexit 42\n")
    failing.chmod(0o755)
    marker = tmp_path / "ran"
    result = subprocess.run(
        [str(WRAPPER), "touch", str(marker)],
        env=environment,
        check=False,
        timeout=5,
    )
    assert result.returncode == 42
    assert not marker.exists()


@pytest.mark.parametrize("runtime", ["empty", "unwritable"])
def test_invalid_runtime_does_not_fall_back(
    environment: dict[str, str],
    tmp_path: Path,
    runtime: str,
) -> None:
    directory = tmp_path / "denied"
    directory.mkdir(mode=0o500)
    environment["XDG_RUNTIME_DIR"] = "" if runtime == "empty" else str(directory)
    try:
        result = subprocess.run(
            [str(WRAPPER), "true"],
            env=environment,
            check=False,
            capture_output=True,
            timeout=5,
        )
        assert result.returncode != 0
        assert not Path(environment["RECORD"]).exists()
        assert not Path(environment["XDG_STATE_HOME"]).exists()
    finally:
        directory.chmod(0o700)


def test_existing_readonly_locks(environment: dict[str, str]) -> None:
    directory = Path(environment["XDG_RUNTIME_DIR"]) / "borgchart" / "cargo-slots"
    directory.mkdir(parents=True)
    for index in range(2):
        lock = directory / f"cargo-slot-{index}"
        lock.touch(mode=0o400)
    directory.chmod(0o500)
    try:
        subprocess.run([str(WRAPPER), "true"], env=environment, check=True, timeout=5)
    finally:
        directory.chmod(0o700)


def test_killed_holder_releases_lock(
    environment: dict[str, str], tmp_path: Path
) -> None:
    pidfile = tmp_path / "child-pid"
    holder = subprocess.Popen(
        [str(WRAPPER), "sh", "-c", 'echo $$ > "$1"; exec sleep 30', "sh", str(pidfile)],
        env=environment,
    )
    try:
        wait_for(pidfile)
        holder.kill()
        holder.wait(timeout=5)
        lock = (
            Path(environment["XDG_RUNTIME_DIR"])
            / "borgchart"
            / "cargo-slots"
            / "cargo-slot-0"
        )
        subprocess.run(["flock", "-n", str(lock), "true"], check=True, timeout=5)
    finally:
        if holder.poll() is None:
            holder.kill()
            holder.wait(timeout=5)
        if pidfile.exists():
            os.kill(int(pidfile.read_text()), 15)


@pytest.mark.parametrize("name", ["XDG_RUNTIME_DIR", "XDG_STATE_HOME"])
def test_relative_lock_root_refused(environment: dict[str, str], name: str) -> None:
    if name == "XDG_STATE_HOME":
        environment.pop("XDG_RUNTIME_DIR")
    environment[name] = "relative-directory"
    result = subprocess.run(
        [str(WRAPPER), "true"],
        env=environment,
        capture_output=True,
        check=False,
        timeout=5,
    )
    assert result.returncode != 0
    assert not Path(environment["RECORD"]).exists()
