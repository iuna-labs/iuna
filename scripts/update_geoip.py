#!/usr/bin/env python3
"""Build the bundled IP-to-country database from PDDL user-country CSVs."""

from __future__ import annotations

import argparse
import csv
import ipaddress
import struct
import tempfile
import time
import urllib.request
from pathlib import Path


IPV4_URL = "https://github.com/sapics/ip-location-db/releases/download/latest/user-country-ipv4-cidr.csv"
IPV6_URL = "https://github.com/sapics/ip-location-db/releases/download/latest/user-country-ipv6-cidr.csv"
MAGIC = b"IUNAGEO2"


def download(url: str, destination: Path) -> None:
    request = urllib.request.Request(url, headers={"User-Agent": "iuna-geoip-updater/1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        destination.write_bytes(response.read())


def records_from_csv(path: Path, version: int) -> list[tuple[int, int, bytes, bytes]]:
    records: list[tuple[int, int, bytes, bytes]] = []
    with path.open(newline="", encoding="ascii") as source:
        for line_number, row in enumerate(csv.reader(source), 1):
            if len(row) != 2:
                raise ValueError(f"{path}:{line_number}: expected CIDR,country")
            cidr, country_text = row
            country = country_text.upper().encode("ascii")
            if len(country) != 2 or not country.isalpha():
                raise ValueError(f"{path}:{line_number}: invalid country code")
            network = ipaddress.ip_network(cidr, strict=True)
            if network.version != version:
                raise ValueError(f"{path}:{line_number}: unexpected IP version")
            records.append((version, network.prefixlen, country, network.network_address.packed))
    return records


def write_database(records: list[tuple[int, int, bytes, bytes]], output: Path) -> None:
    groups: dict[tuple[int, int], dict[bytes, bytes]] = {}
    for version, prefix, country, address in records:
        group = groups.setdefault((version, prefix), {})
        previous = group.setdefault(address, country)
        if previous != country:
            raise ValueError(f"conflicting countries for {address.hex()}/{prefix}")

    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("wb") as database:
        database.write(MAGIC)
        for version, max_prefix in ((4, 32), (6, 128)):
            for prefix in range(max_prefix + 1):
                database.write(struct.pack("<I", len(groups.get((version, prefix), ()))))
        for version, max_prefix in ((4, 32), (6, 128)):
            for prefix in range(max_prefix + 1):
                for address, country in sorted(groups.get((version, prefix), {}).items()):
                    database.write(address)
                    database.write(country)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ipv4-csv", type=Path)
    parser.add_argument("--ipv6-csv", type=Path)
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("src/ip_geolocation/embedded/ip-country.bin"),
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="iuna-geoip-") as temporary_directory:
        temporary = Path(temporary_directory)
        ipv4_csv = args.ipv4_csv or temporary / "user-country-ipv4-cidr.csv"
        ipv6_csv = args.ipv6_csv or temporary / "user-country-ipv6-cidr.csv"
        if args.ipv4_csv is None:
            download(IPV4_URL, ipv4_csv)
            time.sleep(1)
        if args.ipv6_csv is None:
            download(IPV6_URL, ipv6_csv)

        records = records_from_csv(ipv4_csv, 4)
        records.extend(records_from_csv(ipv6_csv, 6))
        write_database(records, args.output)
        print(f"wrote {len(records)} prefixes to {args.output}")


if __name__ == "__main__":
    main()
