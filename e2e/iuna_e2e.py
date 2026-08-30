#!/usr/bin/env python3
"""Docker Compose e2e runner and reproducible chain checkpoint manager."""

from __future__ import annotations

import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import urllib.error


ROOT = Path(__file__).resolve().parent.parent
E2E_DIR = ROOT / "e2e"
COMPOSE_FILES = (ROOT / "docker-compose.yml", E2E_DIR / "docker-compose.e2e.yml")
SERVICES = ("bootstrap", "node2", "node3", "node4", "node5", "node6")
DEFAULT_PORTS = {
    "bootstrap": 28661,
    "node2": 28662,
    "node3": 28663,
    "node4": 28664,
    "node5": 28665,
    "node6": 28666,
}
PORT_ENV = {
    "bootstrap": "IUNA_E2E_BOOTSTRAP_PORT",
    "node2": "IUNA_E2E_NODE2_PORT",
    "node3": "IUNA_E2E_NODE3_PORT",
    "node4": "IUNA_E2E_NODE4_PORT",
    "node5": "IUNA_E2E_NODE5_PORT",
    "node6": "IUNA_E2E_NODE6_PORT",
}
SNAPSHOT_FILES = ("chain.sqlite3", "ui_data.sqlite3", "wallet.json", "config.json")
EXPECTED_PROFILE = "iuna-local-e2e-5s-v1"
EXPECTED_BLOCK_MS = 5_000
HTTP_TIMEOUT_SECONDS = 30
NAME_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
_OPENERS: dict[str, urllib.request.OpenerDirector] = {}


class E2EError(RuntimeError):
    pass


def runtime_dir() -> Path:
    return Path(os.environ.get("IUNA_E2E_RUNTIME_DIR", E2E_DIR / ".runtime")).resolve()


def snapshots_dir() -> Path:
    return Path(os.environ.get("IUNA_E2E_SNAPSHOTS_DIR", E2E_DIR / "snapshots")).resolve()


def compose_env() -> dict[str, str]:
    env = os.environ.copy()
    env["IUNA_E2E_RUNTIME_DIR"] = str(runtime_dir())
    return env


def compose_command(*args: str) -> list[str]:
    command = [
        "docker",
        "compose",
        "--project-name",
        os.environ.get("IUNA_E2E_PROJECT", "iuna-e2e"),
    ]
    for compose_file in COMPOSE_FILES:
        command.extend(("--file", str(compose_file)))
    command.extend(args)
    return command


def compose(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        compose_command(*args),
        cwd=ROOT,
        env=compose_env(),
        check=check,
        text=True,
    )


def validate_snapshot_name(name: str) -> str:
    if not NAME_PATTERN.fullmatch(name):
        raise E2EError(
            "snapshot names must start with a lowercase letter or digit and only contain "
            "lowercase letters, digits, '.', '_' or '-'"
        )
    return name


def snapshot_path(name: str) -> Path:
    return snapshots_dir() / validate_snapshot_name(name)


def node_port(service: str) -> int:
    return int(os.environ.get(PORT_ENV[service], DEFAULT_PORTS[service]))


def node_status(service: str) -> dict:
    base_url = f"http://127.0.0.1:{node_port(service)}"
    opener = _OPENERS.get(service)
    if opener is None:
        password = os.environ.get("IUNA_TESTNET_PASSWORD", "testtesttest")
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
        login = urllib.request.Request(
            f"{base_url}/api/auth/login",
            data=urllib.parse.urlencode({"password": password}).encode(),
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                "Origin": base_url,
            },
            method="POST",
        )
        with opener.open(login, timeout=HTTP_TIMEOUT_SECONDS) as response:
            result = json.load(response)
        if not result.get("ok"):
            raise E2EError(f"{service} login failed: {result.get('error', 'unknown error')}")
        _OPENERS[service] = opener
    with opener.open(
        f"{base_url}/api/status", timeout=HTTP_TIMEOUT_SECONDS
    ) as response:
        return json.load(response)


def all_statuses() -> dict[str, dict]:
    return {service: node_status(service) for service in SERVICES}


def compact_status(status: dict) -> dict:
    return {
        "height": status["chain"]["height"],
        "tip_hash": status["chain"]["tip_hash"],
        "finalized_height": status["chain"].get("finalized_height"),
        "profile": status["launch_profile"]["profile_id"],
        "target_block_ms": status["mining"]["vdf_target_block_ms"],
    }


def assert_e2e_profile(statuses: dict[str, dict]) -> None:
    errors = []
    for service, status in statuses.items():
        profile = status["launch_profile"]["profile_id"]
        block_ms = status["mining"]["vdf_target_block_ms"]
        if profile != EXPECTED_PROFILE or block_ms != EXPECTED_BLOCK_MS:
            errors.append(f"{service}: profile={profile}, target_block_ms={block_ms}")
    if errors:
        raise E2EError("not running the isolated 5s e2e build: " + "; ".join(errors))


def read_chain_metadata(path: Path) -> dict:
    if not path.exists():
        raise E2EError(f"chain database does not exist: {path}")
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=2)
    try:
        row = connection.execute(
            "SELECT height, tip_hash, updated_at_ms FROM chain_snapshots WHERE id = 1"
        ).fetchone()
    finally:
        connection.close()
    if row is None:
        raise E2EError(f"chain database has no persisted snapshot: {path}")
    return {"height": row[0], "tip_hash": row[1], "updated_at_ms": row[2]}


def wait_for_persisted_height(target: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    database = runtime_dir() / "bootstrap" / "chain.sqlite3"
    last_height = None
    while time.monotonic() < deadline:
        try:
            metadata = read_chain_metadata(database)
            last_height = metadata["height"]
            if last_height >= target:
                return metadata
        except sqlite3.Error:
            pass
        time.sleep(0.1)
    raise E2EError(
        f"timed out waiting for persisted bootstrap height {target}; last height={last_height}"
    )


def materialize_chain_checkpoint(
    source: Path, chain_destination: Path, ui_destination: Path, height: int
) -> None:
    subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--features",
            "e2e",
            "--bin",
            "iuna-e2e-checkpoint",
            "--",
            str(source),
            str(chain_destination),
            str(ui_destination),
            str(height),
        ],
        cwd=ROOT,
        check=True,
        text=True,
    )


def wait_for_height(target: int, timeout: float, converge: bool) -> dict[str, dict]:
    deadline = time.monotonic() + timeout
    last_summary = "nodes unavailable"
    while time.monotonic() < deadline:
        try:
            statuses = all_statuses()
            assert_e2e_profile(statuses)
            tips = {
                (status["chain"]["height"], status["chain"]["tip_hash"])
                for status in statuses.values()
            }
            heights = [status["chain"]["height"] for status in statuses.values()]
            last_summary = ", ".join(
                f"{service}={status['chain']['height']}" for service, status in statuses.items()
            )
            reached = min(heights) >= target
            if reached and (not converge or len(tips) == 1):
                return statuses
        except (E2EError, OSError, KeyError, urllib.error.URLError) as error:
            last_summary = str(error)
        time.sleep(0.25)
    raise E2EError(f"timed out waiting for height {target}: {last_summary}")


def sqlite_backup(source: Path, destination: Path) -> None:
    source_connection = sqlite3.connect(f"file:{source}?mode=ro", uri=True, timeout=5)
    destination_connection = sqlite3.connect(destination)
    try:
        source_connection.backup(destination_connection)
        destination_connection.execute("PRAGMA journal_mode = DELETE")
    finally:
        destination_connection.close()
        source_connection.close()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def snapshot_checksums(directory: Path) -> dict[str, str]:
    return {
        str(path.relative_to(directory)): sha256(path)
        for path in sorted(directory.glob("*/*"))
        if path.is_file()
    }


def verify_snapshot(directory: Path) -> dict:
    manifest_path = directory / "manifest.json"
    if not manifest_path.exists():
        raise E2EError(f"snapshot has no manifest: {directory}")
    manifest = json.loads(manifest_path.read_text())
    expected = manifest.get("sha256", {})
    actual = snapshot_checksums(directory)
    if actual != expected:
        raise E2EError(f"snapshot checksum mismatch: {directory}")
    return manifest


def capture_snapshot(name: str, height: int, timeout: float, force: bool) -> None:
    destination = snapshot_path(name)
    if destination.exists() and not force:
        raise E2EError(f"snapshot already exists: {destination}; pass --force to replace it")
    assert_e2e_profile({"bootstrap": node_status("bootstrap")})
    live_metadata = wait_for_persisted_height(height, timeout)
    compose("pause", *SERVICES)
    temp_path: Path | None = None
    try:
        snapshots_dir().mkdir(parents=True, exist_ok=True)
        temp_path = Path(tempfile.mkdtemp(prefix=f".{name}-", dir=snapshots_dir()))
        canonical_database = temp_path / ".canonical-chain.sqlite3"
        canonical_ui_database = temp_path / ".canonical-ui-data.sqlite3"
        materialize_chain_checkpoint(
            runtime_dir() / "bootstrap" / "chain.sqlite3",
            canonical_database,
            canonical_ui_database,
            height,
        )
        nodes = {}
        for service in SERVICES:
            source_dir = runtime_dir() / service
            target_dir = temp_path / service
            target_dir.mkdir()
            sqlite_backup(canonical_database, target_dir / "chain.sqlite3")
            sqlite_backup(canonical_ui_database, target_dir / "ui_data.sqlite3")
            for filename in SNAPSHOT_FILES[2:]:
                source = source_dir / filename
                if not source.exists():
                    raise E2EError(f"required {service} snapshot file is missing: {source}")
                shutil.copy2(source, target_dir / filename)
            nodes[service] = read_chain_metadata(target_dir / "chain.sqlite3")

        for database in (canonical_database, canonical_ui_database):
            for suffix in ("", "-shm", "-wal"):
                candidate = Path(f"{database}{suffix}")
                if candidate.exists():
                    candidate.unlink()

        canonical = nodes["bootstrap"]
        manifest = {
            "format": 1,
            "name": name,
            "height": canonical["height"],
            "tip_hash": canonical["tip_hash"],
            "profile_id": EXPECTED_PROFILE,
            "target_block_ms": EXPECTED_BLOCK_MS,
            "source_height": live_metadata["height"],
            "nodes": nodes,
            "sha256": snapshot_checksums(temp_path),
        }
        (temp_path / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        if destination.exists():
            shutil.rmtree(destination)
        temp_path.rename(destination)
        temp_path = None
        print(f"captured {name} at height {height}: {destination}")
    finally:
        if temp_path is not None:
            shutil.rmtree(temp_path, ignore_errors=True)
        compose("unpause", *SERVICES, check=False)


def restore_snapshot(name: str) -> None:
    source = snapshot_path(name)
    manifest = verify_snapshot(source)
    if manifest.get("profile_id") != EXPECTED_PROFILE:
        raise E2EError(
            f"snapshot profile is {manifest.get('profile_id')}, expected {EXPECTED_PROFILE}"
        )
    compose("down", "--remove-orphans", check=False)
    base = runtime_dir()
    base.mkdir(parents=True, exist_ok=True)
    for service in SERVICES:
        target = base / service
        if target.exists():
            shutil.rmtree(target)
        target.mkdir(parents=True)
        for filename in SNAPSHOT_FILES:
            snapshot_file = source / service / filename
            if filename == "ui_data.sqlite3" and not snapshot_file.exists():
                continue
            shutil.copy2(snapshot_file, target / filename)
    print(f"restored {name} at height {manifest['height']} into {base}")


def reset_runtime() -> None:
    compose("down", "--remove-orphans", check=False)
    base = runtime_dir()
    for service in SERVICES:
        target = base / service
        if target.exists():
            shutil.rmtree(target)
    _OPENERS.clear()
    print(f"cleared mutable e2e node data under {base}")


def start(build: bool) -> None:
    runtime_dir().mkdir(parents=True, exist_ok=True)
    for service in SERVICES:
        (runtime_dir() / service).mkdir(parents=True, exist_ok=True)
    args = ["up", "--detach", "--wait", "--wait-timeout", "600"]
    if build:
        args.append("--build")
    args.extend(SERVICES)
    compose(*args)
    statuses = all_statuses()
    assert_e2e_profile(statuses)
    print(json.dumps({name: compact_status(value) for name, value in statuses.items()}, indent=2))


def capture_plan(plan_path: Path, timeout: float, force: bool) -> None:
    plan = json.loads(plan_path.read_text())
    for checkpoint in plan:
        name = validate_snapshot_name(checkpoint["name"])
        height = int(checkpoint["height"])
        destination = snapshot_path(name)
        if destination.exists() and not force:
            print(f"keeping existing snapshot {name}")
            continue
        capture_snapshot(name, height, timeout, force)


def smoke(name: str, through: int, timeout: float, build: bool, keep: bool) -> None:
    restore_snapshot(name)
    try:
        start(build)
        statuses = wait_for_height(through, timeout, converge=True)
        summary = {service: compact_status(status) for service, status in statuses.items()}
        print(f"e2e smoke passed through height {through}")
        print(json.dumps(summary, indent=2))
    except Exception:
        compose("logs", "--tail", "200", *SERVICES, check=False)
        raise
    finally:
        if not keep:
            compose("down", "--remove-orphans", check=False)


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)

    up = commands.add_parser("up", help="start the isolated six-node e2e network")
    up.add_argument("--build", action="store_true")
    up.add_argument("--snapshot", help="restore this checkpoint before starting")

    commands.add_parser("down", help="stop the e2e network without deleting runtime data")
    commands.add_parser("reset", help="stop the network and delete mutable e2e node data")
    commands.add_parser("status", help="show authenticated live status for every node")

    wait = commands.add_parser("wait", help="wait until every node reaches a height")
    wait.add_argument("height", type=int)
    wait.add_argument("--timeout", type=float, default=600)
    wait.add_argument("--converge", action="store_true")

    capture = commands.add_parser("capture", help="capture all node identities and chain DBs")
    capture.add_argument("name")
    capture.add_argument("--height", type=int, required=True)
    capture.add_argument("--timeout", type=float, default=7_200)
    capture.add_argument("--force", action="store_true")

    restore = commands.add_parser("restore", help="replace runtime data with a checkpoint")
    restore.add_argument("name")

    plan = commands.add_parser("capture-plan", help="capture each checkpoint in a JSON plan")
    plan.add_argument("--plan", type=Path, default=E2E_DIR / "checkpoints.json")
    plan.add_argument("--timeout", type=float, default=7_200)
    plan.add_argument("--force", action="store_true")

    test = commands.add_parser("smoke", help="restore a checkpoint and test network convergence")
    test.add_argument("--from", dest="snapshot", required=True)
    test.add_argument("--through", type=int, required=True)
    test.add_argument("--timeout", type=float, default=600)
    test.add_argument("--build", action="store_true")
    test.add_argument("--keep", action="store_true")
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "up":
            if args.snapshot:
                restore_snapshot(args.snapshot)
            start(args.build)
        elif args.command == "down":
            compose("down", "--remove-orphans")
        elif args.command == "reset":
            reset_runtime()
        elif args.command == "status":
            statuses = all_statuses()
            assert_e2e_profile(statuses)
            print(json.dumps({name: compact_status(value) for name, value in statuses.items()}, indent=2))
        elif args.command == "wait":
            statuses = wait_for_height(args.height, args.timeout, args.converge)
            print(json.dumps({name: compact_status(value) for name, value in statuses.items()}, indent=2))
        elif args.command == "capture":
            capture_snapshot(args.name, args.height, args.timeout, args.force)
        elif args.command == "restore":
            restore_snapshot(args.name)
        elif args.command == "capture-plan":
            capture_plan(args.plan, args.timeout, args.force)
        elif args.command == "smoke":
            smoke(args.snapshot, args.through, args.timeout, args.build, args.keep)
        return 0
    except (E2EError, OSError, sqlite3.Error, subprocess.CalledProcessError) as error:
        print(f"e2e error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
