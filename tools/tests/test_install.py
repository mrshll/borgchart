"""Exercise the published installer with local release archives."""

import hashlib
import os
import platform
import subprocess
import tarfile
from pathlib import Path

import pytest

INSTALLER = Path(__file__).resolve().parents[2] / "docs/install.sh"
BINARIES = ("brg", "brg-seat", "brg-slack", "brg-timer", "brg-github")


def release(tmp_path: Path) -> tuple[dict[str, str], Path]:
    machine = {"x86_64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}[
        platform.machine()
    ]
    system = {"Linux": "unknown-linux-gnu", "Darwin": "apple-darwin"}[platform.system()]
    package = tmp_path / "package"
    (package / "bin").mkdir(parents=True)
    for name in BINARIES:
        binary = package / "bin" / name
        binary.write_text("#!/bin/sh\nprintf 'fixture executable\\n'\n")
        binary.chmod(0o755)
    (package / "templates").mkdir()
    (package / "templates" / "example.txt").write_text("new template")
    (package / "LICENSE").write_text("fixture license")
    archive = tmp_path / f"brg-0.1.0-{machine}-{system}.tar.gz"
    with tarfile.open(archive, "w:gz") as tar:
        for name in ("bin", "templates", "LICENSE"):
            tar.add(package / name, arcname=name)
    (tmp_path / "SHA256SUMS").write_text(
        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
    )
    home = tmp_path / "home"
    home.mkdir()
    env = {
        "HOME": str(home),
        "PATH": os.environ["PATH"],
        "BRG_VERSION": "v0.1.0",
        "BRG_RELEASE_URL": tmp_path.as_uri(),
    }
    return env, archive


def install(env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["sh", str(INSTALLER)], env=env, capture_output=True, text=True, check=False
    )


def test_install_and_upgrade_preserve_config_and_replace_templates(
    tmp_path: Path,
) -> None:
    env, _ = release(tmp_path)
    # Exercise quoted paths in YAML and spaces in all installation paths.
    env["PREFIX"] = str(tmp_path / "user's applications")
    env["BINDIR"] = str(tmp_path / "command links")
    result = install(env)
    assert result.returncode == 0, result.stderr
    for name in BINARIES:
        result = subprocess.run(
            [str(Path(env["BINDIR"]) / name)],
            capture_output=True,
            text=True,
            check=True,
        )
        assert result.stdout == "fixture executable\n"
    config = Path(env["HOME"]) / ".config/borgchart/config.yaml"
    assert config.exists()
    config.write_text("schema: 1\nauto_trust: false\ntemplates: []\n")
    templates = Path(env["PREFIX"]) / "brg/templates"
    (templates / "obsolete.txt").touch()
    (templates / "example.txt").write_text("old template")
    result = install(env)
    assert result.returncode == 0, result.stderr
    assert config.read_text() == "schema: 1\nauto_trust: false\ntemplates: []\n"
    assert (templates / "example.txt").read_text() == "new template"
    assert not (templates / "obsolete.txt").exists()


@pytest.mark.parametrize("checksum", ["mismatch", "missing"])
def test_bad_checksum_leaves_existing_install_untouched(
    tmp_path: Path, checksum: str
) -> None:
    env, archive = release(tmp_path)
    if checksum == "mismatch":
        (tmp_path / "SHA256SUMS").write_text(f"{'0' * 64}  {archive.name}\n")
    else:
        (tmp_path / "SHA256SUMS").write_text("")
    binary = Path(env["HOME"]) / ".local/share/brg/bin/brg"
    binary.parent.mkdir(parents=True)
    binary.write_text("existing binary")
    result = install(env)
    assert result.returncode != 0
    assert binary.read_text() == "existing binary"
    assert not (Path(env["HOME"]) / ".config/borgchart/config.yaml").exists()
