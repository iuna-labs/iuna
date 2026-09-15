# Bundled IP geolocation data

`ip-country.bin` is generated from the IPv4 and IPv6 `user-country` CIDR
datasets published by [sapics/ip-location-db](https://github.com/sapics/ip-location-db).
That dataset is dedicated to the public domain under the
[Open Data Commons PDDL 1.0](https://opendatacommons.org/licenses/pddl/1-0/),
which permits use, modification, embedding, and redistribution without an
attribution requirement.

Rust's `include_bytes!` embeds the generated database into the executable at
compile time. It is not a runtime file or web asset, and lookups never use the
network or filesystem.

Update the database from the repository root with:

```sh
python3 scripts/update_geoip.py
```

For a reproducible/offline regeneration from previously downloaded archives:

```sh
python3 scripts/update_geoip.py \
  --ipv4-csv /path/to/user-country-ipv4-cidr.csv \
  --ipv6-csv /path/to/user-country-ipv6-cidr.csv
```

The compact binary format starts with `IUNAGEO2`, followed by record counts for
each IPv4 and IPv6 prefix length and sorted fixed-width network-address/country
records. Lookup checks prefix groups from most to least specific and uses binary
search within each group. This provides longest-prefix-match without allocating
a large in-memory trie at startup.

The source-data provenance is retained here for auditability; neither the PDDL
nor the dataset requires it to be displayed in the application or shipped as a
separate runtime file.
