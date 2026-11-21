//! Every variable the gateway's `Config` reads can be set from the Helm chart
//! and from the Compose file, so the two install paths accept the same
//! settings.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use obleth_config::Config;

/// Read but deliberately not offered by an install path.
const NOT_IN_CHART: &[&str] = &[
    // Deprecated spelling of OBLETH_BATCH_RETENTION, read only when that is unset.
    "OBLETH_BATCH_RETENTION_DAYS",
];
const NOT_IN_COMPOSE: &[&str] = &["OBLETH_BATCH_RETENTION_DAYS"];

fn read_by_config() -> BTreeSet<String> {
    let seen = RefCell::new(BTreeSet::new());
    let _ = Config::from_lookup(&|key| {
        seen.borrow_mut().insert(key.to_string());
        None
    });
    let seen = seen.into_inner();
    assert!(seen.len() > 50, "Config read only {} variables", seen.len());
    seen
}

fn repo_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("OBLETH_") {
        let tail = &rest[at..];
        let len = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        out.insert(tail[..len].to_string());
        rest = &tail[len..];
    }
    out
}

/// The `obleth` service's block in the Compose file: from its key to the
/// next service key at the same indent.
fn compose_gateway_service(compose: &str) -> &str {
    let start = compose
        .find("\n  obleth:\n")
        .expect("obleth service in compose");
    let body = &compose[start + 1..];
    let mut end = 0;
    for (i, line) in body.split_inclusive('\n').enumerate() {
        let next_service = line.starts_with("  ")
            && !line.starts_with("   ")
            && !line.trim_start().starts_with('#');
        if i > 0 && next_service {
            break;
        }
        end += line.len();
    }
    &body[..end]
}

fn missing(read: &BTreeSet<String>, offered: &BTreeSet<String>, allowed: &[&str]) -> Vec<String> {
    read.iter()
        .filter(|k| !offered.contains(*k) && !allowed.contains(&k.as_str()))
        .cloned()
        .collect()
}

#[test]
fn chart_sets_every_variable_config_reads() {
    let read = read_by_config();
    let chart = names(&format!(
        "{}{}{}",
        repo_file("deploy/k8s/obleth/templates/obleth.yaml"),
        repo_file("deploy/k8s/obleth/templates/_helpers.tpl"),
        repo_file("deploy/k8s/obleth/templates/secret.yaml"),
    ));
    let gaps = missing(&read, &chart, NOT_IN_CHART);
    assert!(gaps.is_empty(), "the chart cannot set: {gaps:?}");
}

#[test]
fn compose_passes_every_variable_config_reads() {
    let read = read_by_config();
    let compose = repo_file("deploy/docker/docker-compose.yml");
    let offered = names(compose_gateway_service(&compose));
    let gaps = missing(&read, &offered, NOT_IN_COMPOSE);
    assert!(gaps.is_empty(), "Compose does not pass through: {gaps:?}");
}
