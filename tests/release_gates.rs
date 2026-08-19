use std::{fs, path::Path};

const FUZZ_TARGETS: &[&str] = &[
    "p2p_envelope",
    "compact_snapshot",
    "domain_json",
    "stratum_request",
    "wallet_config",
];

fn read_repo_file(path: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

#[test]
fn deployment_release_gate_runs_all_fuzz_targets_with_validated_count() {
    let deployment = read_repo_file("deployment.sh");

    assert!(deployment.contains("local fuzz_runs=\"${IUNA_FUZZ_RUNS:-256}\""));
    assert!(deployment.contains("validate_positive_integer IUNA_FUZZ_RUNS \"$fuzz_runs\""));
    assert!(deployment.contains("if [[ \"${BASH_SOURCE[0]}\" == \"$0\" ]]; then"));
    for target in FUZZ_TARGETS {
        assert!(
            deployment.contains(&format!(
                "cargo run --locked --manifest-path fuzz/Cargo.toml --bin {target} -- -runs=\"$fuzz_runs\" fuzz/corpus/{target}"
            )),
            "deployment release gate does not run fuzz target {target}"
        );
    }
}

#[test]
fn release_documentation_lists_the_deployment_fuzz_gate_targets() {
    let roadmap = read_repo_file("ROADMAP.md");
    let genesis = read_repo_file("docs/genesis.md");
    let security_review = read_repo_file("docs/security-review.md");

    assert!(roadmap.contains("fuzz gate runs with `256` iterations each"));
    assert!(genesis.contains("runs `256` iterations per fuzz target by default"));
    for target in FUZZ_TARGETS {
        assert!(
            roadmap.contains(target)
                && genesis.contains(&format!(
                    "cargo run --locked --manifest-path fuzz/Cargo.toml --bin {target} -- -runs=256 fuzz/corpus/{target}"
                ))
                && security_review.contains(&format!(
                    "cargo run --locked --manifest-path fuzz/Cargo.toml --bin {target} -- -runs=256 fuzz/corpus/{target}"
                )),
            "release documentation is missing fuzz target {target}"
        );
    }
}
