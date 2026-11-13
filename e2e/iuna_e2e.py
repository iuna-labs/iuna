#!/usr/bin/env python3
"""Run Iuna's complete test gate and live TCP end-to-end tests."""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parent.parent


def run(*command: str) -> None:
    print(f"+ {' '.join(command)}", flush=True)
    subprocess.run(command, cwd=ROOT, check=True)


def run_tests() -> None:
    run("cargo", "test", "--all-targets", "--all-features")
    run("cargo", "test", "--manifest-path", "src-tauri/Cargo.toml")
    run("node", "--test", "tests/wallet-app.test.cjs", "tests/website-content.test.cjs")


def run_tcp_test() -> None:
    run(
        "cargo",
        "test",
        "--features",
        "e2e",
        "--test",
        "properties",
        "--",
        "--ignored",
        "--nocapture",
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "scope",
        choices=("all", "test", "tcp"),
        default="all",
        nargs="?",
        help="gate to run (default: all)",
    )
    args = parser.parse_args()

    if args.scope in ("all", "test"):
        run_tests()
    if args.scope in ("all", "tcp"):
        run_tcp_test()


if __name__ == "__main__":
    main()
