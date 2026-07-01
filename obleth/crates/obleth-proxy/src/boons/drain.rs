//! A minimal Drain-style log template miner.
//!
//! Clusters log lines by token count and token similarity, merging the positions
//! that vary into a `<*>` wildcard. Unlike a digit-only mask, this clusters lines
//! whose variable parts are non-numeric too (hostnames, paths, UUIDs, request
//! ids), so varied application logs collapse far better. Deterministic, no model,
//! single-pass per line; state lives only for the duration of one call.

/// Wildcard token used for variable positions in a mined template.
const WILDCARD: &str = "<*>";

/// A token "looks variable" if it contains a digit — pre-wildcard it so a fresh
/// cluster generalizes immediately (Drain's number-token heuristic).
fn is_variable_token(t: &str) -> bool {
    t.chars().any(|c| c.is_ascii_digit())
}

/// Assign each line to a cluster id (index into a conceptual template table).
/// Two lines share a cluster when they have the same token count and their token
/// similarity (matching / total positions, wildcards always match) is at least
/// `sim_threshold`. Templates generalize in place as members are absorbed.
///
/// Returns one cluster id per input line, in input order.
pub(super) fn cluster_ids(lines: &[&str], sim_threshold: f32) -> Vec<usize> {
    // Each cluster is its current template as a token vector.
    let mut templates: Vec<Vec<String>> = Vec::new();
    let mut ids = Vec::with_capacity(lines.len());

    for line in lines {
        let toks: Vec<&str> = line.split_whitespace().collect();
        let n = toks.len();

        let mut best: Option<usize> = None;
        let mut best_sim = sim_threshold;
        for (ci, tmpl) in templates.iter().enumerate() {
            if tmpl.len() != n {
                continue;
            }
            let sim = if n == 0 {
                1.0
            } else {
                let matches = toks
                    .iter()
                    .zip(tmpl.iter())
                    .filter(|(tok, slot)| slot.as_str() == WILDCARD || slot.as_str() == **tok)
                    .count();
                matches as f32 / n as f32
            };
            if sim >= best_sim {
                best_sim = sim;
                best = Some(ci);
            }
        }

        match best {
            Some(ci) => {
                // Generalize: any position that now disagrees becomes a wildcard.
                for (p, tok) in toks.iter().enumerate() {
                    if templates[ci][p] != WILDCARD && templates[ci][p] != *tok {
                        templates[ci][p] = WILDCARD.to_string();
                    }
                }
                ids.push(ci);
            }
            None => {
                let tmpl: Vec<String> = toks
                    .iter()
                    .map(|t| {
                        if is_variable_token(t) {
                            WILDCARD.to_string()
                        } else {
                            (*t).to_string()
                        }
                    })
                    .collect();
                templates.push(tmpl);
                ids.push(templates.len() - 1);
            }
        }
    }

    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clusters_lines_differing_only_in_numeric_fields() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("2026-06-30 INFO request {i} handled in {i}ms"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let ids = cluster_ids(&refs, 0.6);
        // All ten collapse to a single cluster.
        assert!(ids.iter().all(|&id| id == ids[0]));
    }

    #[test]
    fn clusters_lines_differing_in_non_numeric_fields() {
        // The digit-only mask would NOT cluster these (no digits); Drain does,
        // because only two of six positions vary.
        let lines = [
            "user alice logged in from workstation-alpha",
            "user bob logged in from workstation-beta",
            "user carol logged in from workstation-gamma",
            "user dave logged in from workstation-delta",
        ];
        let ids = cluster_ids(&lines, 0.6);
        assert!(
            ids.iter().all(|&id| id == ids[0]),
            "non-numeric vars should cluster"
        );
    }

    #[test]
    fn keeps_structurally_different_lines_apart() {
        let lines = [
            "GET /api/v1/users 200 in 12ms",
            "database connection pool exhausted after retrying",
        ];
        let ids = cluster_ids(&lines, 0.6);
        assert_ne!(ids[0], ids[1]);
    }
}
