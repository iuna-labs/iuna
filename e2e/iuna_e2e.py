#!/usr/bin/env python3
"""Docker Compose e2e runner and reproducible chain checkpoint manager."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
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
SYNC_SERVICE = "syncnode"
REFERENCE_MINING_ENV = "IUNA_E2E_REFERENCE_MINING_ENABLED"
SERVICE_IPS = {
    "bootstrap": "172.29.0.10",
    "node2": "172.29.0.11",
    "node3": "172.29.0.12",
    "node4": "172.29.0.13",
    "node5": "172.29.0.14",
    "node6": "172.29.0.15",
}
PARTITION_GROUPS = (SERVICES[:3], SERVICES[3:])
DEFAULT_PORTS = {
    "bootstrap": 28661,
    "node2": 28662,
    "node3": 28663,
    "node4": 28664,
    "node5": 28665,
    "node6": 28666,
    SYNC_SERVICE: 28667,
}
PORT_ENV = {
    "bootstrap": "IUNA_E2E_BOOTSTRAP_PORT",
    "node2": "IUNA_E2E_NODE2_PORT",
    "node3": "IUNA_E2E_NODE3_PORT",
    "node4": "IUNA_E2E_NODE4_PORT",
    "node5": "IUNA_E2E_NODE5_PORT",
    "node6": "IUNA_E2E_NODE6_PORT",
    SYNC_SERVICE: "IUNA_E2E_SYNCNODE_PORT",
}
SNAPSHOT_FILES = ("chain.sqlite3", "ui_data.sqlite3", "wallet.json", "config.json")
EXPECTED_PROFILE = "iuna-local-e2e-5s-v1"
EXPECTED_BLOCK_MS = 5_000
HTTP_TIMEOUT_SECONDS = 30
NAME_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
_OPENERS: dict[str, urllib.request.OpenerDirector] = {}


@dataclass(frozen=True)
class Scenario:
    snapshot: str
    through: int
    minimum_finalized_height: int | None = None
    canonical_height: int | None = None
    require_fixture_hash: bool = False
    leader_burn_minimum_height: int | None = None


SCENARIOS = {
    "fallback-activation": Scenario(
        snapshot="pre-fallback-invalidation",
        through=300,
    ),
    "objective-finality": Scenario(
        snapshot="pre-objective-finality",
        through=1_001,
        minimum_finalized_height=1_000,
        canonical_height=1_000,
    ),
    "checkpoint-restart": Scenario(
        snapshot="first-objective-checkpoint",
        through=1_007,
        minimum_finalized_height=1_000,
        canonical_height=1_000,
        require_fixture_hash=True,
        leader_burn_minimum_height=1_002,
    ),
}
SPECIAL_SCENARIOS = ("sync-resilience", "partition-recovery")
POST_ACTIVATION_SCENARIOS = (
    "objective-finality",
    "checkpoint-restart",
    *SPECIAL_SCENARIOS,
)


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


def create_evidence_run(base: Path | None, scenario: str) -> tuple[Path | None, dict]:
    started_at = datetime.now(timezone.utc)
    report = {
        "format": 1,
        "scenario": scenario,
        "started_at": started_at.isoformat(),
        "git_commit": subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip(),
        "git_dirty": bool(
            subprocess.run(
                ["git", "status", "--porcelain"],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
        ),
        "tracked_tree_sha256": tracked_tree_sha256(),
        "phases": {},
        "outcome": "running",
    }
    if base is None:
        return None, report
    run = base.resolve() / f"{started_at.strftime('%Y%m%dT%H%M%S.%fZ')}-{scenario}"
    run.mkdir(parents=True, exist_ok=False)
    write_evidence_report(run, report)
    return run, report


def tracked_tree_sha256() -> str:
    tracked = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    ).stdout.split(b"\0")
    digest = hashlib.sha256()
    for encoded_path in tracked:
        if not encoded_path:
            continue
        path = ROOT / os.fsdecode(encoded_path)
        digest.update(encoded_path)
        digest.update(b"\0")
        if path.exists():
            contents = path.read_bytes()
            digest.update(len(contents).to_bytes(8, "big"))
            digest.update(contents)
        else:
            digest.update(b"missing")
    return digest.hexdigest()


def write_evidence_report(run: Path | None, report: dict) -> None:
    if run is None:
        return
    temporary = run / "report.json.tmp"
    temporary.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    temporary.replace(run / "report.json")


def capture_evidence_logs(
    run: Path | None, services: tuple[str, ...] = SERVICES
) -> None:
    if run is None:
        return
    result = subprocess.run(
        compose_command("logs", "--no-color", *services),
        cwd=ROOT,
        env=compose_env(),
        check=False,
        capture_output=True,
        text=True,
    )
    (run / "nodes.log").write_text(result.stdout + result.stderr)


def evidence_block(block: dict) -> dict:
    return {
        key: block.get(key)
        for key in (
            "height",
            "hash",
            "prev_hash",
            "timestamp_ms",
            "finalizer_mode",
            "finalizer_rank",
            "miner",
        )
    }


def evidence_statuses(statuses: dict[str, dict]) -> dict[str, dict]:
    return {service: compact_status(status) for service, status in statuses.items()}


def container_command(
    service: str, *args: str, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        compose_command("exec", "-T", service, *args),
        cwd=ROOT,
        env=compose_env(),
        check=check,
        text=True,
        stdout=None if check else subprocess.DEVNULL,
        stderr=None if check else subprocess.DEVNULL,
    )


def clear_partition() -> None:
    for service in SERVICES:
        for builtin, chain in (
            ("INPUT", "IUNA_E2E_INPUT"),
            ("OUTPUT", "IUNA_E2E_OUTPUT"),
        ):
            container_command(
                service, "iptables", "-D", builtin, "-j", chain, check=False
            )
            container_command(service, "iptables", "-F", chain, check=False)
            container_command(service, "iptables", "-X", chain, check=False)


def apply_partition() -> None:
    clear_partition()
    left, right = PARTITION_GROUPS
    for group, blocked_group in ((left, right), (right, left)):
        for service in group:
            for builtin, chain in (
                ("INPUT", "IUNA_E2E_INPUT"),
                ("OUTPUT", "IUNA_E2E_OUTPUT"),
            ):
                container_command(service, "iptables", "-N", chain)
                container_command(service, "iptables", "-I", builtin, "1", "-j", chain)
            for blocked_service in blocked_group:
                blocked_ip = SERVICE_IPS[blocked_service]
                container_command(
                    service,
                    "iptables",
                    "-A",
                    "IUNA_E2E_INPUT",
                    "-s",
                    blocked_ip,
                    "-j",
                    "REJECT",
                )
                container_command(
                    service,
                    "iptables",
                    "-A",
                    "IUNA_E2E_OUTPUT",
                    "-d",
                    blocked_ip,
                    "-j",
                    "REJECT",
                )
    print(f"partition active: {left} | {right}", flush=True)


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


def node_json(service: str, path: str) -> object:
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
        f"{base_url}{path}", timeout=HTTP_TIMEOUT_SECONDS
    ) as response:
        return json.load(response)


def node_form(service: str, path: str, values: dict[str, object]) -> dict:
    node_json(service, "/api/status")
    base_url = f"http://127.0.0.1:{node_port(service)}"
    request = urllib.request.Request(
        f"{base_url}{path}",
        data=urllib.parse.urlencode(values).encode(),
        headers={"Content-Type": "application/x-www-form-urlencoded", "Origin": base_url},
        method="POST",
    )
    with _OPENERS[service].open(request, timeout=HTTP_TIMEOUT_SECONDS) as response:
        result = json.load(response)
    if not isinstance(result, dict) or not result.get("ok"):
        raise E2EError(f"{service} form request failed for {path}: {result}")
    return result


def node_status(service: str) -> dict:
    result = node_json(service, "/api/status")
    if not isinstance(result, dict):
        raise E2EError(f"{service} returned a non-object status response")
    return result


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


def assert_converged(statuses: dict[str, dict]) -> None:
    tips = {
        (status["chain"]["height"], status["chain"]["tip_hash"])
        for status in statuses.values()
    }
    if len(tips) != 1:
        summary = ", ".join(
            f"{service}={status['chain']['height']}:{status['chain']['tip_hash'][:12]}"
            for service, status in statuses.items()
        )
        raise E2EError(f"nodes did not converge: {summary}")


def assert_finality(statuses: dict[str, dict], minimum_height: int | None) -> None:
    checkpoints = {
        (status["chain"].get("finalized_height"), status["chain"].get("finalized_hash"))
        for status in statuses.values()
    }
    if len(checkpoints) != 1:
        raise E2EError(f"nodes disagree about objective finality: {checkpoints}")
    finalized_height, finalized_hash = next(iter(checkpoints))
    if minimum_height is None:
        if finalized_height is not None or finalized_hash is not None:
            raise E2EError(
                f"objective finality activated too early at height {finalized_height}"
            )
        return
    if finalized_height is None or finalized_height < minimum_height or not finalized_hash:
        raise E2EError(
            f"expected a finalized checkpoint at or above {minimum_height}, "
            f"got {finalized_height}:{finalized_hash}"
        )
    block_hashes = {
        block_at_height(service, finalized_height)["hash"] for service in SERVICES
    }
    if block_hashes != {finalized_hash}:
        raise E2EError(
            f"finalized hash does not identify block {finalized_height}: {block_hashes}"
        )


def block_at_height(service: str, height: int) -> dict:
    result = node_json(service, f"/api/blocks?before_height={height + 1}&limit=1")
    if not isinstance(result, list) or len(result) != 1 or result[0].get("height") != height:
        raise E2EError(f"{service} block API did not return block {height}")
    return result[0]


def recent_blocks(service: str, limit: int = 100) -> list[dict]:
    result = node_json(service, f"/api/blocks?limit={limit}")
    if not isinstance(result, list):
        raise E2EError(f"{service} block API returned a non-list response")
    return result


def wait_for_partition_recovery(
    after_heights: dict[str, int], timeout: float
) -> tuple[dict[str, dict], dict[str, int]]:
    deadline = time.monotonic() + timeout
    last_summary = "nodes unavailable"
    left, right = PARTITION_GROUPS
    while time.monotonic() < deadline:
        try:
            statuses = all_statuses()
            assert_e2e_profile(statuses)
            group_tips = []
            recovery_heights: dict[str, int] = {}
            for label, group in (("left", left), ("right", right)):
                tips = {
                    (
                        statuses[service]["chain"]["height"],
                        statuses[service]["chain"]["tip_hash"],
                    )
                    for service in group
                }
                if len(tips) != 1:
                    break
                group_tip = next(iter(tips))
                group_tips.append(group_tip)
                recoveries = [
                    int(block["height"])
                    for block in recent_blocks(group[0])
                    if block.get("finalizer_mode") == "recovery"
                    and int(block.get("height", -1)) > after_heights[label]
                ]
                if recoveries:
                    recovery_heights[label] = max(recoveries)
            last_summary = ", ".join(
                f"{service}={status['chain']['height']}:{status['chain']['tip_hash'][:12]}"
                for service, status in statuses.items()
            )
            if (
                len(group_tips) == 2
                and group_tips[0] != group_tips[1]
                and set(recovery_heights) == {"left", "right"}
            ):
                return statuses, recovery_heights
        except (
            E2EError,
            OSError,
            KeyError,
            ValueError,
            urllib.error.URLError,
        ) as error:
            last_summary = str(error)
        time.sleep(0.25)
    raise E2EError(
        "timed out waiting for divergent recovery blocks in both partitions; "
        + last_summary
    )


def automatic_finalization_settings(statuses: dict[str, dict]) -> dict[str, dict]:
    return {
        service: {
            "enabled": bool(statuses[service]["mining"]["automatic"]),
            "amount": int(statuses[service]["mining"]["burn_per_block"]),
            "fee_per_byte": int(statuses[service]["mining"]["automatic_burn_fee"]),
            "recovery_top_rank_percent": int(
                statuses[service]["mining"]["recovery_vdf_top_rank_percent"]
            ),
        }
        for service in SERVICES
    }


def configure_partition_recovery_workers(
    statuses: dict[str, dict], settings: dict[str, dict], timeout: float
) -> dict[str, str]:
    for label, group in (("left", PARTITION_GROUPS[0]), ("right", PARTITION_GROUPS[1])):
        tips = {statuses[service]["chain"]["tip_hash"] for service in group}
        if len(tips) != 1:
            raise E2EError(f"cannot select {label} recovery worker before convergence")
    if not all(values["enabled"] for values in settings.values()):
        raise E2EError("partition recovery requires automatic finalization on every node")

    for service in SERVICES:
        mining = statuses[service]["mining"]
        node_form(
            service,
            "/api/settings/burn-per-block",
            {
                "enabled": "false",
                "amount": mining["burn_per_block"],
                "fee_per_byte": mining["automatic_burn_fee"],
            },
        )

    # The automatic-finalizer loop polls this setting and cancels any in-flight
    # VDF. Let every disabled node observe it, then absorb a block that may have
    # crossed the publication boundary concurrently with the settings update.
    time.sleep(2)
    current = all_statuses()
    target = max(status["chain"]["height"] for status in current.values())
    settled = wait_for_height(target, timeout, converge=True)

    workers = {}
    for label, group in (("left", PARTITION_GROUPS[0]), ("right", PARTITION_GROUPS[1])):
        candidates = [
            service
            for service in group
            if not settled[service]["mining"]["wallet_is_current_leader"]
        ]
        if not candidates:
            raise E2EError(f"cannot select a non-leader {label} recovery worker")
        workers[label] = candidates[0]

    for worker in workers.values():
        worker_settings = settings[worker]
        node_form(
            worker,
            "/api/settings/recovery-vdf",
            {"top_rank_percent": 0},
        )
        node_form(
            worker,
            "/api/settings/burn-per-block",
            {
                "enabled": "true",
                "amount": worker_settings["amount"],
                "fee_per_byte": worker_settings["fee_per_byte"],
            },
        )
    print(
        "partition recovery workers: " + json.dumps(workers, sort_keys=True),
        flush=True,
    )
    return workers


def restore_automatic_finalization(settings: dict[str, dict]) -> None:
    for service, values in settings.items():
        node_form(
            service,
            "/api/settings/recovery-vdf",
            {"top_rank_percent": values["recovery_top_rank_percent"]},
        )
        node_form(
            service,
            "/api/settings/burn-per-block",
            {
                "enabled": "true" if values["enabled"] else "false",
                "amount": values["amount"],
                "fee_per_byte": values["fee_per_byte"],
            },
        )


def wait_for_ticket_after(height: int, timeout: float) -> tuple[dict[str, dict], dict]:
    deadline = time.monotonic() + timeout
    last_summary = "nodes unavailable"
    while time.monotonic() < deadline:
        try:
            statuses = all_statuses()
            assert_e2e_profile(statuses)
            assert_converged(statuses)
            tickets = [
                block
                for block in recent_blocks(SERVICES[0])
                if block.get("finalizer_mode") == "ticket"
                and int(block.get("height", -1)) > height
                and int(block.get("finalizer_rank", -1)) == 0
            ]
            if tickets:
                ticket = min(tickets, key=lambda block: int(block["height"]))
                finalized_height = statuses[SERVICES[0]]["chain"].get(
                    "finalized_height"
                )
                if finalized_height is not None and finalized_height >= height:
                    return statuses, ticket
            last_summary = ", ".join(
                f"{service}={status['chain']['height']}"
                for service, status in statuses.items()
            )
        except (
            E2EError,
            OSError,
            KeyError,
            ValueError,
            urllib.error.URLError,
        ) as error:
            last_summary = str(error)
        time.sleep(0.25)
    raise E2EError(
        f"timed out waiting for a rank-0 ticket after {height}; {last_summary}"
    )


def assert_leader_uses_burn_from_height(
    block_height: int, minimum_height: int
) -> None:
    block = block_at_height(SERVICES[0], block_height)
    leader_proof = block.get("leader_proof")
    if block.get("finalizer_mode") != "ticket" or not isinstance(leader_proof, dict):
        raise E2EError(f"block {block_height} was not finalized by a burn ticket")

    ticket_id = leader_proof.get("ticket_id")
    if not isinstance(ticket_id, str) or not ticket_id:
        raise E2EError(f"block {block_height} has no valid leader ticket ID")

    for height in range(minimum_height, block_height):
        source = block_at_height(SERVICES[0], height)
        if any(
            transaction.get("kind") == "burn"
            and transaction.get("signature") == ticket_id
            for transaction in source.get("transactions", [])
        ):
            print(
                f"block {block_height} leader ticket comes from burn at height {height}"
            )
            return

    raise E2EError(
        f"block {block_height} leader ticket does not come from a burn at or after "
        f"height {minimum_height}"
    )


def assert_api_health(
    statuses: dict[str, dict], through: int, services: tuple[str, ...] = SERVICES
) -> None:
    for service in services:
        blocks = node_json(service, "/api/blocks?limit=3")
        health = node_json(service, "/api/network/health")
        peers = node_json(service, "/api/peers?limit=100")
        mempool = node_json(service, "/api/mempool?limit=100")
        wallet = node_json(service, "/api/wallet/transactions?limit=3")
        if not isinstance(blocks, list) or not blocks:
            raise E2EError(f"{service} block API returned no blocks")
        if max(block.get("height", -1) for block in blocks) < through:
            raise E2EError(f"{service} block API has not projected height {through}")
        if not isinstance(health, dict) or health.get("local_height", -1) < through:
            raise E2EError(f"{service} network health is behind height {through}")
        for label, page in (("peers", peers), ("mempool", mempool), ("wallet", wallet)):
            if not isinstance(page, dict) or not isinstance(page.get("items"), list):
                raise E2EError(f"{service} {label} API returned an invalid page")
        if health.get("local_tip_hash") != statuses[service]["chain"]["tip_hash"]:
            # The chain is live; advancing between the status and health requests is valid.
            if health.get("local_height", 0) <= statuses[service]["chain"]["height"]:
                raise E2EError(f"{service} status and network health disagree about the tip")


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


def remove_node_databases(service: str) -> None:
    directory = runtime_dir() / service
    for filename in ("chain.sqlite3", "ui_data.sqlite3"):
        database = directory / filename
        for suffix in ("", "-wal", "-shm", "-journal"):
            candidate = Path(f"{database}{suffix}")
            if candidate.exists():
                candidate.unlink()


def restore_node_databases(
    service: str, snapshot: str, fixture_service: str | None = None
) -> dict:
    source = snapshot_path(snapshot)
    verify_snapshot(source)
    target = runtime_dir() / service
    source_service = fixture_service or service
    remove_node_databases(service)
    for filename in ("chain.sqlite3", "ui_data.sqlite3"):
        shutil.copy2(source / source_service / filename, target / filename)
    return read_chain_metadata(target / "chain.sqlite3")


def wait_for_active_sync(service: str, minimum_target: int, timeout: float) -> dict:
    deadline = time.monotonic() + timeout
    last_summary = "node unavailable"
    while time.monotonic() < deadline:
        try:
            health = node_json(service, "/api/network/health")
            start = health.get("sync_start_height")
            validated = health.get("sync_validated_height")
            target = health.get("sync_target_height")
            last_summary = (
                f"local={health.get('local_height')}, start={start}, "
                f"validated={validated}, target={target}"
            )
            if (
                isinstance(start, int)
                and isinstance(validated, int)
                and isinstance(target, int)
                and target >= minimum_target
                and validated < target
            ):
                return {
                    "local_height": health.get("local_height"),
                    "start_height": start,
                    "validated_height": validated,
                    "target_height": target,
                }
        except (
            E2EError,
            OSError,
            KeyError,
            ValueError,
            urllib.error.URLError,
        ) as error:
            last_summary = str(error)
        time.sleep(0.01)
    raise E2EError(
        f"timed out waiting for active range sync on {service}; {last_summary}"
    )


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


def wait_for_height(
    target: int,
    timeout: float,
    converge: bool,
    services: tuple[str, ...] = SERVICES,
) -> dict[str, dict]:
    deadline = time.monotonic() + timeout
    last_summary = "nodes unavailable"
    while time.monotonic() < deadline:
        try:
            statuses = {service: node_status(service) for service in services}
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


def test_snapshots(plan_path: Path = E2E_DIR / "checkpoints.json") -> None:
    plan = json.loads(plan_path.read_text())
    expected_names = {checkpoint["name"] for checkpoint in plan}
    scenario_snapshots = {scenario.snapshot for scenario in SCENARIOS.values()}
    missing_scenarios = scenario_snapshots - expected_names
    if missing_scenarios:
        raise E2EError(f"scenarios reference snapshots outside the plan: {missing_scenarios}")

    for checkpoint in plan:
        name = validate_snapshot_name(checkpoint["name"])
        expected_height = int(checkpoint["height"])
        directory = snapshot_path(name)
        manifest = verify_snapshot(directory)
        if manifest.get("name") != name or manifest.get("height") != expected_height:
            raise E2EError(
                f"{name} manifest identity mismatch: "
                f"{manifest.get('name')} at {manifest.get('height')}"
            )
        if manifest.get("profile_id") != EXPECTED_PROFILE:
            raise E2EError(f"{name} uses profile {manifest.get('profile_id')}")
        if manifest.get("target_block_ms") != EXPECTED_BLOCK_MS:
            raise E2EError(f"{name} uses target {manifest.get('target_block_ms')}ms")
        nodes = manifest.get("nodes", {})
        if set(nodes) != set(SERVICES):
            raise E2EError(f"{name} does not contain exactly the six e2e nodes")
        for service in SERVICES:
            metadata = read_chain_metadata(directory / service / "chain.sqlite3")
            recorded = nodes[service]
            if metadata != recorded:
                raise E2EError(f"{name}/{service} chain metadata differs from its manifest")
            if metadata["height"] != expected_height:
                raise E2EError(
                    f"{name}/{service} is at {metadata['height']}, expected {expected_height}"
                )
            if metadata["tip_hash"] != manifest.get("tip_hash"):
                raise E2EError(f"{name}/{service} has a different canonical tip")
    print(f"e2e snapshots passed ({len(plan)} checkpoints)")


def assert_canonical_block(height: int, require_fixture_hash: bool) -> None:
    hashes = {block_at_height(service, height)["hash"] for service in SERVICES}
    if len(hashes) != 1:
        raise E2EError(f"nodes disagree about canonical block {height}: {hashes}")
    if not require_fixture_hash:
        return
    checkpoint = next(
        (
            item
            for item in json.loads((E2E_DIR / "checkpoints.json").read_text())
            if int(item["height"]) == height
        ),
        None,
    )
    if checkpoint is None:
        raise E2EError(f"no checkpoint fixture records canonical block {height}")
    expected_hash = verify_snapshot(snapshot_path(checkpoint["name"]))["tip_hash"]
    if hashes != {expected_hash}:
        raise E2EError(
            f"canonical block {height} differs from the checkpoint: {hashes}"
        )


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
    _OPENERS.clear()
    compose("down", "--remove-orphans", check=False)
    base = runtime_dir()
    base.mkdir(parents=True, exist_ok=True)
    for service in SERVICES:
        target = base / service
        target.mkdir(parents=True, exist_ok=True)
        for existing in target.iterdir():
            if existing.is_dir() and not existing.is_symlink():
                shutil.rmtree(existing)
            else:
                existing.unlink()
        for filename in SNAPSHOT_FILES:
            snapshot_file = source / service / filename
            if filename == "ui_data.sqlite3" and not snapshot_file.exists():
                continue
            shutil.copy2(snapshot_file, target / filename)
    print(f"restored {name} at height {manifest['height']} into {base}")


def reset_runtime() -> None:
    compose("down", "--remove-orphans", check=False)
    base = runtime_dir()
    for service in (*SERVICES, SYNC_SERVICE):
        target = base / service
        if target.exists():
            shutil.rmtree(target)
    _OPENERS.clear()
    print(f"cleared mutable e2e node data under {base}")


def start(build: bool) -> None:
    _OPENERS.clear()
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


def run_scenario(
    name: str, scenario: Scenario, timeout: float, build: bool, keep: bool
) -> None:
    print(
        f"running e2e scenario {name}: {scenario.snapshot} -> {scenario.through}",
        flush=True,
    )
    restore_snapshot(scenario.snapshot)
    try:
        start(build)
        statuses = wait_for_height(scenario.through, timeout, converge=True)
        assert_converged(statuses)
        assert_finality(statuses, scenario.minimum_finalized_height)
        if scenario.canonical_height is not None:
            assert_canonical_block(
                scenario.canonical_height, scenario.require_fixture_hash
            )
        if scenario.leader_burn_minimum_height is not None:
            assert_leader_uses_burn_from_height(
                scenario.through, scenario.leader_burn_minimum_height
            )
        assert_api_health(statuses, scenario.through)
        print(f"e2e scenario {name} passed")
    except Exception:
        compose("logs", "--tail", "200", *SERVICES, check=False)
        raise
    finally:
        if not keep:
            compose("down", "--remove-orphans", check=False)


def run_sync_resilience_scenario(
    timeout: float, build: bool, keep: bool, evidence_dir: Path | None
) -> None:
    name = "sync-resilience"
    service = SYNC_SERVICE
    sync_services = (*SERVICES, service)
    evidence_run, evidence = create_evidence_run(evidence_dir, name)
    print(
        "running e2e scenario sync-resilience: interrupted empty bootstrap and stale range sync",
        flush=True,
    )
    previous_reference_mining = os.environ.get(REFERENCE_MINING_ENV)
    os.environ[REFERENCE_MINING_ENV] = "false"
    try:
        # Sync interruption is the variable under test. Start the reference chain
        # paused so a slow deployment host cannot turn range validation into an
        # unrelated moving-tip fork race while the seventh node catches up.
        restore_snapshot("first-objective-checkpoint")
        start(build)
        initial = wait_for_height(1_001, timeout, converge=True)
        initial_height = min(
            status["chain"]["height"] for status in initial.values()
        )
        evidence["phases"]["initial"] = evidence_statuses(initial)
        write_evidence_report(evidence_run, evidence)

        compose("rm", "--force", "--stop", service, check=False)
        _OPENERS.pop(service, None)
        sync_directory = runtime_dir() / service
        if sync_directory.exists():
            shutil.rmtree(sync_directory)
        sync_directory.mkdir(parents=True)
        chain_database = sync_directory / "chain.sqlite3"
        evidence["phases"]["empty_bootstrap_started"] = {
            "service": service,
            "source_height": initial_height,
            "chain_snapshot_present": False,
        }
        write_evidence_report(evidence_run, evidence)
        compose("up", "--detach", "--no-deps", service)
        time.sleep(0.05)
        compose("kill", "--signal", "SIGKILL", service)
        _OPENERS.pop(service, None)
        try:
            interrupted = read_chain_metadata(chain_database)
        except (E2EError, sqlite3.Error):
            interrupted = None
        if interrupted is not None and interrupted["height"] >= initial_height:
            raise E2EError(
                "empty bootstrap completed before it could be interrupted; "
                "increase the fixture height"
            )
        evidence["phases"]["empty_bootstrap_interrupted"] = {
            "signal": "SIGKILL",
            "persisted_height": (
                interrupted["height"] if interrupted is not None else None
            ),
        }
        write_evidence_report(evidence_run, evidence)
        compose("start", service)
        empty_synced = wait_for_height(
            initial_height, timeout, converge=True, services=sync_services
        )
        assert_converged(empty_synced)
        evidence["phases"]["empty_bootstrap_resumed"] = {
            "interrupted_before_target_persisted": True,
            "nodes": evidence_statuses(empty_synced),
        }
        write_evidence_report(evidence_run, evidence)

        stale_target = min(empty_synced[node]["chain"]["height"] for node in SERVICES)
        compose("stop", service)
        _OPENERS.pop(service, None)
        stale = restore_node_databases(
            service, "pre-fallback-invalidation", fixture_service="node6"
        )
        if stale["height"] >= stale_target:
            raise E2EError(
                f"stale fixture height {stale['height']} is not below target {stale_target}"
            )
        evidence["phases"]["stale_range_started"] = {
            "service": service,
            "stale_snapshot": "pre-fallback-invalidation",
            "stale_height": stale["height"],
            "stale_tip_hash": stale["tip_hash"],
            "minimum_target_height": stale_target,
        }
        write_evidence_report(evidence_run, evidence)
        compose("up", "--detach", "--no-deps", service)
        _OPENERS.pop(service, None)
        progress = wait_for_active_sync(service, stale_target, timeout)
        compose("kill", "--signal", "SIGKILL", service)
        _OPENERS.pop(service, None)
        interrupted = read_chain_metadata(chain_database)
        if interrupted["height"] >= progress["target_height"]:
            raise E2EError(
                "stale range sync reached its target before process interruption"
            )
        evidence["phases"]["stale_range_interrupted"] = {
            **progress,
            "signal": "SIGKILL",
            "persisted_height": interrupted["height"],
            "persisted_tip_hash": interrupted["tip_hash"],
        }
        write_evidence_report(evidence_run, evidence)
        compose("start", service)
        stale_synced = wait_for_height(
            stale_target, timeout, converge=True, services=sync_services
        )
        assert_converged(stale_synced)
        assert_api_health(stale_synced, stale_target, services=sync_services)
        evidence["phases"]["stale_range_resumed"] = {
            "nodes": evidence_statuses(stale_synced),
        }
        evidence["outcome"] = "passed"
        evidence["finished_at"] = datetime.now(timezone.utc).isoformat()
        write_evidence_report(evidence_run, evidence)
        capture_evidence_logs(evidence_run, sync_services)
        print(
            f"e2e scenario {name} passed: empty bootstrap and range sync "
            f"resumed through at least height {stale_target}",
            flush=True,
        )
    except Exception as error:
        evidence["outcome"] = "failed"
        evidence["finished_at"] = datetime.now(timezone.utc).isoformat()
        evidence["error"] = {
            "type": type(error).__name__,
            "message": str(error),
        }
        write_evidence_report(evidence_run, evidence)
        capture_evidence_logs(evidence_run, sync_services)
        compose("logs", "--tail", "300", *sync_services, check=False)
        raise
    finally:
        try:
            if not keep:
                compose("down", "--remove-orphans", check=False)
        finally:
            if previous_reference_mining is None:
                os.environ.pop(REFERENCE_MINING_ENV, None)
            else:
                os.environ[REFERENCE_MINING_ENV] = previous_reference_mining


def run_partition_recovery_scenario(
    timeout: float, build: bool, keep: bool, evidence_dir: Path | None
) -> None:
    name = "partition-recovery"
    evidence_run, evidence = create_evidence_run(evidence_dir, name)
    print(
        "running e2e scenario partition-recovery: physical 3-3 split, heal and restart",
        flush=True,
    )
    mining_settings = None
    restore_snapshot("first-objective-checkpoint")
    try:
        start(build)
        initial = wait_for_height(1_001, timeout, converge=True)
        evidence["phases"]["initial"] = evidence_statuses(initial)
        write_evidence_report(evidence_run, evidence)
        mining_settings = automatic_finalization_settings(initial)
        recovery_workers = configure_partition_recovery_workers(
            initial, mining_settings, timeout
        )
        apply_partition()
        partitioned_start = all_statuses()
        left, right = PARTITION_GROUPS
        partition_boundaries = {
            label: max(
                partitioned_start[service]["chain"]["height"] for service in group
            )
            for label, group in (("left", left), ("right", right))
        }
        evidence["phases"]["partition_started"] = {
            "boundaries": partition_boundaries,
            "recovery_workers": recovery_workers,
            "nodes": evidence_statuses(partitioned_start),
        }
        write_evidence_report(evidence_run, evidence)
        partitioned, recovery_heights = wait_for_partition_recovery(
            partition_boundaries, timeout
        )
        evidence["phases"]["partition_recovery"] = {
            "recovery_heights": recovery_heights,
            "nodes": evidence_statuses(partitioned),
        }
        write_evidence_report(evidence_run, evidence)
        print(
            "partition recovery observed: "
            + json.dumps(recovery_heights, sort_keys=True),
            flush=True,
        )

        right_worker = recovery_workers["right"]
        right_mining = mining_settings[right_worker]
        node_form(
            right_worker,
            "/api/settings/burn-per-block",
            {
                "enabled": "false",
                "amount": right_mining["amount"],
                "fee_per_byte": right_mining["fee_per_byte"],
            },
        )
        # Stop the competing branch before reconnecting the islands. Otherwise
        # equally paced recovery workers can keep both branches growing forever.
        time.sleep(2)
        clear_partition()
        heal_target = max(status["chain"]["height"] for status in partitioned.values())
        healed = wait_for_height(heal_target, timeout, converge=True)
        assert_converged(healed)
        restore_automatic_finalization(mining_settings)
        mining_settings = None
        partition_height = max(partition_boundaries.values())
        canonical_recoveries = [
            block
            for block in recent_blocks(SERVICES[0])
            if block.get("finalizer_mode") == "recovery"
            and int(block.get("height", -1)) > partition_height
        ]
        if not canonical_recoveries:
            raise E2EError(
                "healed canonical chain contains no partition recovery block"
            )
        canonical_recovery_height = max(
            int(block["height"]) for block in canonical_recoveries
        )
        canonical_recovery = next(
            block
            for block in canonical_recoveries
            if int(block["height"]) == canonical_recovery_height
        )
        evidence["phases"]["healed"] = {
            "canonical_recovery": evidence_block(canonical_recovery),
            "nodes": evidence_statuses(healed),
        }
        write_evidence_report(evidence_run, evidence)

        restart_height = max(status["chain"]["height"] for status in healed.values())
        compose("restart", "node6")
        _OPENERS.pop("node6", None)
        evidence["phases"]["restart"] = {
            "service": "node6",
            "after_height": restart_height,
        }
        write_evidence_report(evidence_run, evidence)
        resumed, ticket = wait_for_ticket_after(
            max(canonical_recovery_height, restart_height), timeout
        )
        assert_converged(resumed)
        assert_api_health(resumed, int(ticket["height"]))
        evidence["phases"]["resumed"] = {
            "ticket": evidence_block(ticket),
            "nodes": evidence_statuses(resumed),
        }
        evidence["outcome"] = "passed"
        evidence["finished_at"] = datetime.now(timezone.utc).isoformat()
        write_evidence_report(evidence_run, evidence)
        capture_evidence_logs(evidence_run)
        print(
            f"e2e scenario {name} passed: recovery at {canonical_recovery_height}, "
            f"rank-0 ticket resumed at {ticket['height']}",
            flush=True,
        )
    except Exception as error:
        evidence["outcome"] = "failed"
        evidence["finished_at"] = datetime.now(timezone.utc).isoformat()
        evidence["error"] = {
            "type": type(error).__name__,
            "message": str(error),
        }
        write_evidence_report(evidence_run, evidence)
        capture_evidence_logs(evidence_run)
        compose("logs", "--tail", "300", *SERVICES, check=False)
        raise
    finally:
        clear_partition()
        if mining_settings is not None:
            try:
                restore_automatic_finalization(mining_settings)
            except Exception as error:
                print(
                    f"warning: could not restore automatic finalization settings: {error}",
                    file=sys.stderr,
                )
        if not keep:
            compose("down", "--remove-orphans", check=False)


def run_tests(
    name: str, timeout: float, build: bool, keep: bool, evidence_dir: Path | None
) -> None:
    if name in ("snapshots", "all"):
        test_snapshots()
    if name == "all":
        selected = [*SCENARIOS, *SPECIAL_SCENARIOS]
    elif name == "post-activation":
        test_snapshots()
        selected = list(POST_ACTIVATION_SCENARIOS)
    else:
        selected = [name]
    selected = [scenario_name for scenario_name in selected if scenario_name != "snapshots"]
    for index, scenario_name in enumerate(selected):
        scenario_keep = keep and index == len(selected) - 1
        if scenario_name == "partition-recovery":
            run_partition_recovery_scenario(
                timeout,
                build and index == 0,
                scenario_keep,
                evidence_dir,
            )
        elif scenario_name == "sync-resilience":
            run_sync_resilience_scenario(
                timeout,
                build and index == 0,
                scenario_keep,
                evidence_dir,
            )
        else:
            run_scenario(
                scenario_name,
                SCENARIOS[scenario_name],
                timeout,
                build and index == 0,
                scenario_keep,
            )
    print(f"e2e test run passed: {name}")


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

    smoke_test = commands.add_parser(
        "smoke", help="restore a checkpoint and test network convergence"
    )
    smoke_test.add_argument("--from", dest="snapshot", required=True)
    smoke_test.add_argument("--through", type=int, required=True)
    smoke_test.add_argument("--timeout", type=float, default=600)
    smoke_test.add_argument("--build", action="store_true")
    smoke_test.add_argument("--keep", action="store_true")

    tests = commands.add_parser("test", help="run named e2e assertions")
    tests.add_argument(
        "scenario",
        nargs="?",
        default="all",
        choices=(
            "all",
            "post-activation",
            "snapshots",
            *SCENARIOS,
            *SPECIAL_SCENARIOS,
        ),
    )
    tests.add_argument("--timeout", type=float, default=600)
    tests.add_argument("--build", action="store_true")
    tests.add_argument("--keep", action="store_true")
    tests.add_argument(
        "--evidence-dir",
        type=Path,
        help="write scenario phase reports and node logs below this directory",
    )
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
        elif args.command == "test":
            run_tests(
                args.scenario,
                args.timeout,
                args.build,
                args.keep,
                args.evidence_dir,
            )
        return 0
    except (E2EError, OSError, sqlite3.Error, subprocess.CalledProcessError) as error:
        print(f"e2e error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
