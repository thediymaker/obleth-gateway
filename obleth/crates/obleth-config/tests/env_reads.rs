//! Gateway settings are read by `obleth_config::Config`, with one parser and
//! one warning path. Any other read of the environment in the gateway's
//! crates must be listed here with its reason.

use std::path::{Path, PathBuf};

/// Files that may read the environment themselves, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "obleth-proxy/src/replicas.rs",
        "HOSTNAME: the pod's own name, the replica identity",
    ),
    (
        "obleth-admin/src/capacity_discovery/kube.rs",
        "KUBERNETES_SERVICE_HOST/PORT: the in-cluster API address the kubelet sets",
    ),
    (
        "obleth-store/src/crypto.rs",
        "OBLETH_ENCRYPTION_KEY: read where the cipher is built, failing the start when unusable",
    ),
    ("obleth-store/src/lib.rs", "CI, in tests only"),
    (
        "obleth-store/src/settings.rs",
        "the encryption key, in tests only",
    ),
    (
        "obleth-telemetry/src/lib.rs",
        "OBLETH_CLIENT_IP_RETENTION_DAYS: read where the ClickHouse schema is applied",
    ),
    (
        "obleth-provisioner/src/config.rs",
        "the provisioner is its own binary with its own settings",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_config_reads_the_environment() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = krate.path().join("src");
        if krate.file_name() != "obleth-config" && src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    assert!(files.len() > 20, "found only {} files", files.len());
    let mut strays = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        // Integration tests read their own fixtures' variables.
        let reads = text.lines().any(|line| {
            (line.contains("env::var(") || line.contains("env::var_os("))
                && !line.contains("\"OBLETH_TEST_")
        });
        if !reads {
            continue;
        }
        let rel = file
            .strip_prefix(&crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !ALLOWED.iter().any(|(path, _)| *path == rel) {
            strays.push(rel);
        }
    }
    assert!(
        strays.is_empty(),
        "read the setting through obleth_config::Config (or list the file here with its reason): {strays:?}"
    );
}
