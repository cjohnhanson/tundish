"""The Python publish path and the PyPI command reach the same store."""

import json
import subprocess
import sys
from pathlib import Path

import tundish


def cli(db: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "tundish._cli_main",
            "--db",
            str(db),
            "--config",
            str(db.with_suffix(".yml")),
            *args,
        ],
        capture_output=True,
        text=True,
    )


def test_publish_then_pull(tmp_path: Path) -> None:
    db = tmp_path / "t.db"
    cfg = tmp_path / "t.yml"
    assert cli(db, "subscribe", "feed", "--as", "a", "--queue", "slack").returncode == 0
    first = tundish.publish(
        "slack", {"text": "hi", "n": 1}, key="C123", db=db, config=cfg
    )
    second = tundish.publish("slack", ["a", 2], db=db, config=cfg)
    assert second > first
    out = cli(db, "pull", "feed", "--as", "a")
    assert out.returncode == 0, out.stderr
    assert f"[slack#{first}] C123" in out.stdout
    assert json.dumps({"text": "hi", "n": 1}, separators=(",", ":")) in out.stdout


def test_bad_payload_is_a_value_error(tmp_path: Path) -> None:
    try:
        tundish.publish("q", {1, 2}, db=tmp_path / "t.db", config=tmp_path / "t.yml")
    except TypeError:
        return  # json.dumps refuses a set before Rust sees it
    raise AssertionError("a set is not JSON")


def test_version() -> None:
    assert tundish.__version__
