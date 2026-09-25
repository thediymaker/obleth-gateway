//! Reading a serving container's per-replica concurrency off its command
//! line, for `kubernetes`-source models that do not state it.
//!
//! A table of known inference servers and the setting each uses for "requests
//! served at once". Only an explicit value counts: a server's built-in default
//! is never assumed, since it changes between versions and hardware. A model's
//! own `per_replica_max_in_flight` always wins over anything found here.

use super::kube::Container;

/// One known server's concurrency setting.
pub(crate) struct ConcurrencyRule {
    /// Shown in the discovery view next to the value it produced.
    pub server: &'static str,
    /// Command-line flags, matched as `--flag N` or `--flag=N`.
    pub flags: &'static [&'static str],
    /// Environment variables the server also reads the setting from, matched
    /// only with a literal `value` on the container.
    pub env: &'static [&'static str],
}

/// Tried in order, container by container; the first container with a match
/// decides. The more specific flags come first: `--parallel` means other
/// things to other programs, so llama.cpp is last.
pub(crate) const RULES: &[ConcurrencyRule] = &[
    ConcurrencyRule {
        server: "vLLM",
        flags: &["--max-num-seqs", "--max_num_seqs"],
        env: &[],
    },
    ConcurrencyRule {
        server: "SGLang",
        flags: &["--max-running-requests", "--max_running_requests"],
        env: &[],
    },
    ConcurrencyRule {
        server: "TGI",
        flags: &["--max-concurrent-requests"],
        env: &["MAX_CONCURRENT_REQUESTS"],
    },
    ConcurrencyRule {
        server: "llama.cpp",
        flags: &["--parallel", "-np"],
        env: &["LLAMA_ARG_N_PARALLEL"],
    },
];

/// A value found on a container, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Detected {
    pub value: usize,
    /// Human-readable rule, e.g. `vLLM --max-num-seqs`.
    pub rule: String,
}

/// The first known concurrency setting on any of `containers`.
pub(crate) fn detect(containers: &[Container]) -> Option<Detected> {
    containers.iter().find_map(detect_in)
}

fn detect_in(container: &Container) -> Option<Detected> {
    let tokens = tokens(container);
    for rule in RULES {
        for flag in rule.flags {
            if let Some(value) = flag_value(&tokens, flag, container) {
                return Some(Detected {
                    value,
                    rule: format!("{} {flag}", rule.server),
                });
            }
        }
        for name in rule.env {
            let literal = container
                .env
                .iter()
                .find(|e| e.name == *name)
                .and_then(|e| e.value.as_deref());
            if let Some(value) = literal.and_then(|v| parse_value(v, container)) {
                return Some(Detected {
                    value,
                    rule: format!("{} {name}", rule.server),
                });
            }
        }
    }
    None
}

/// The container's command and args as one token stream. Each entry is split
/// on whitespace too, so a whole command line passed to a shell
/// (`["bash", "-c", "vllm serve m --max-num-seqs 8"]`) reads the same as one
/// given as separate args. Shell line continuations are dropped.
fn tokens(container: &Container) -> Vec<&str> {
    container
        .command
        .iter()
        .chain(container.args.iter())
        .flat_map(|entry| entry.split_whitespace())
        .filter(|t| *t != "\\")
        .collect()
}

/// The value after the last `flag` (`--flag N` or `--flag=N`) that parses,
/// since a later flag overrides an earlier one on a command line.
fn flag_value(tokens: &[&str], flag: &str, container: &Container) -> Option<usize> {
    let mut found = None;
    for (i, token) in tokens.iter().enumerate() {
        let token = unquote(token);
        let raw = if token == flag {
            tokens.get(i + 1).copied()
        } else {
            token
                .strip_prefix(flag)
                .and_then(|rest| rest.strip_prefix('='))
        };
        if let Some(v) = raw.and_then(|raw| parse_value(raw, container)) {
            found = Some(v);
        }
    }
    found
}

/// A positive integer within bounds, possibly quoted, or a Kubernetes
/// `$(VAR)` reference to one of the container's literal env values. A shell
/// expansion (`${VAR}`) is not resolved and does not parse.
fn parse_value(raw: &str, container: &Container) -> Option<usize> {
    let raw = unquote(raw.trim());
    let raw = match raw.strip_prefix("$(").and_then(|r| r.strip_suffix(')')) {
        Some(var) => container
            .env
            .iter()
            .find(|e| e.name == var)
            .and_then(|e| e.value.as_deref())
            .map(|v| unquote(v.trim()))?,
        None => raw,
    };
    let n: i64 = raw.parse().ok()?;
    (1..=obleth_config::capacity::MAX_PER_REPLICA_MAX_IN_FLIGHT)
        .contains(&n)
        .then_some(n as usize)
}

fn unquote(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'')
}

#[cfg(test)]
mod tests {
    use super::super::kube::EnvVar;
    use super::*;

    fn container(command: &[&str], args: &[&str]) -> Container {
        Container {
            name: "server".into(),
            command: command.iter().map(|s| s.to_string()).collect(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
        }
    }

    fn found(c: &Container) -> Option<(usize, String)> {
        detect(std::slice::from_ref(c)).map(|d| (d.value, d.rule))
    }

    #[test]
    fn separate_and_joined_flag_forms_are_read() {
        let c = container(&["vllm", "serve", "m"], &["--max-num-seqs", "8"]);
        assert_eq!(found(&c), Some((8, "vLLM --max-num-seqs".into())));
        let c = container(&[], &["serve", "m", "--max-num-seqs=16"]);
        assert_eq!(found(&c), Some((16, "vLLM --max-num-seqs".into())));
        let c = container(
            &["python", "-m", "vllm.entrypoints.openai.api_server"],
            &["--max_num_seqs", "'32'"],
        );
        assert_eq!(found(&c), Some((32, "vLLM --max_num_seqs".into())));
    }

    #[test]
    fn a_command_line_inside_a_shell_string_is_read() {
        let c = container(
            &["/bin/bash", "-lc"],
            &["set -e\nexec vllm serve /models/m \\\n  --tensor-parallel-size 4 \\\n  --max-num-seqs \\\n  4 \\\n  --port 8000"],
        );
        assert_eq!(found(&c), Some((4, "vLLM --max-num-seqs".into())));
    }

    #[test]
    fn a_later_flag_overrides_an_earlier_one() {
        let c = container(&[], &["--max-num-seqs", "8", "--max-num-seqs", "12"]);
        assert_eq!(found(&c), Some((12, "vLLM --max-num-seqs".into())));
    }

    #[test]
    fn each_known_server_is_recognised() {
        let c = container(
            &["python", "-m", "sglang.launch_server"],
            &["--max-running-requests", "48"],
        );
        assert_eq!(
            found(&c),
            Some((48, "SGLang --max-running-requests".into()))
        );
        let c = container(
            &["text-generation-launcher"],
            &["--max-concurrent-requests=64"],
        );
        assert_eq!(
            found(&c),
            Some((64, "TGI --max-concurrent-requests".into()))
        );
        let c = container(&["llama-server"], &["-m", "/m.gguf", "-np", "4"]);
        assert_eq!(found(&c), Some((4, "llama.cpp -np".into())));
        let c = container(&["llama-server"], &["--parallel", "2"]);
        assert_eq!(found(&c), Some((2, "llama.cpp --parallel".into())));
    }

    #[test]
    fn literal_env_values_are_read_and_references_resolved() {
        let mut c = container(&["text-generation-launcher"], &[]);
        c.env.push(EnvVar {
            name: "MAX_CONCURRENT_REQUESTS".into(),
            value: Some("96".into()),
        });
        assert_eq!(found(&c), Some((96, "TGI MAX_CONCURRENT_REQUESTS".into())));

        let mut c = container(&["vllm", "serve"], &["--max-num-seqs", "$(SEQS)"]);
        c.env.push(EnvVar {
            name: "SEQS".into(),
            value: Some("24".into()),
        });
        assert_eq!(found(&c), Some((24, "vLLM --max-num-seqs".into())));

        // A value from a secret or config map (no literal) is not read.
        let mut c = container(&["text-generation-launcher"], &[]);
        c.env.push(EnvVar {
            name: "MAX_CONCURRENT_REQUESTS".into(),
            value: None,
        });
        assert_eq!(found(&c), None);
    }

    #[test]
    fn unusable_values_are_ignored() {
        for args in [
            &["--max-num-seqs", "${MAX_SEQS:-8}"][..],
            &["--max-num-seqs", "0"],
            &["--max-num-seqs", "-1"],
            &["--max-num-seqs", "many"],
            &["--max-num-seqs"],
            &["--max-num-seqs", "1000000"],
            &["--max-num-seqs-per-step", "8"],
        ] {
            assert_eq!(found(&container(&["vllm"], args)), None, "{args:?}");
        }
    }

    #[test]
    fn a_server_without_a_known_flag_has_no_value() {
        let c = container(&["python", "app.py"], &["--port", "8000", "--workers", "2"]);
        assert_eq!(found(&c), None);
    }

    #[test]
    fn the_first_container_with_a_setting_decides() {
        let sidecar = container(&["envoy"], &["--concurrency", "2"]);
        let server = container(&["vllm", "serve"], &["--max-num-seqs", "6"]);
        let other = container(&["vllm", "serve"], &["--max-num-seqs", "99"]);
        let d = detect(&[sidecar, server, other]).expect("found");
        assert_eq!(d.value, 6);
    }
}
