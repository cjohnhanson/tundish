"""The `tundish` command, for an install from PyPI."""

import sys

from tundish._native import run_cli


def main() -> None:
    sys.exit(run_cli(["tundish", *sys.argv[1:]]))
