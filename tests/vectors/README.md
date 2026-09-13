# Cryptographic audit vectors

`ml_dsa44_audit.json` is a curated subset of two upstream ML-DSA-44 verification corpora. The
source commit, path, and SHA-256 digest of each complete upstream JSON file are recorded inside the
fixture so an auditor can reproduce the selection without trusting this repository.

Included cases:

- NIST ACVP `tcId 11`: valid pure ML-DSA-44 signature with an external context;
- Project Wycheproof `tcId 18`: invalid signature with a repeated hint index, the regression case
  for CVE-2026-24850 / GHSA-5x2r-hc65-25f9;
- Project Wycheproof `tcId 56`: valid signature immediately below the ML-DSA-44 `z` norm limit.

The full upstream files are deliberately not vendored: together they exceed 5 MB. This small set is
a permanent integration regression suite, not evidence that the upstream implementation itself has
been comprehensively audited. Run it with:

```sh
cargo test ml_dsa44_matches_pinned_nist_and_wycheproof_vectors --lib
```

NIST ACVP material is published by the United States government. Project Wycheproof test vectors
are Apache-2.0 licensed.
