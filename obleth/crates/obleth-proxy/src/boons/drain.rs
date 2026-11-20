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
///
/// A line is compared only with the templates of its token count, and at
/// most [`MAX_TEMPLATES`] are kept: a line that matches none once that many
/// exist is a cluster of its own, so the work stays linear in the lines.
pub(super) fn cluster_ids(lines: &[&str], sim_threshold: f32) -> Vec<usize> {
    // Each cluster is its current template as tokens borrowed from the lines.
    let mut templates: Vec<Vec<&str>> = Vec::new();
    // Template ids by token count, oldest first.
    let mut by_len: std::collections::HashMap<usize, Vec<usize>> = Default::default();
    let mut unclustered = MAX_TEMPLATES;
    let mut ids = Vec::with_capacity(lines.len());

    for line in lines {
        let toks: Vec<&str> = line.split_whitespace().collect();
        let n = toks.len();

        let mut best: Option<usize> = None;
        let mut best_sim = sim_threshold;
        for &ci in by_len.get(&n).map_or(&[][..], Vec::as_slice) {
            let sim = if n == 0 {
                1.0
            } else {
                let matches = toks
                    .iter()
                    .zip(&templates[ci])
                    .filter(|(tok, slot)| **slot == WILDCARD || *slot == *tok)
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
                for (slot, tok) in templates[ci].iter_mut().zip(&toks) {
                    if *slot != WILDCARD && slot != tok {
                        *slot = WILDCARD;
                    }
                }
                ids.push(ci);
            }
            None if templates.len() < MAX_TEMPLATES => {
                let tmpl: Vec<&str> = toks
                    .iter()
                    .map(|t| if is_variable_token(t) { WILDCARD } else { t })
                    .collect();
                by_len.entry(n).or_default().push(templates.len());
                ids.push(templates.len());
                templates.push(tmpl);
            }
            None => {
                ids.push(unclustered);
                unclustered += 1;
            }
        }
    }

    ids
}

/// Templates one call keeps (see [`cluster_ids`]). A log with more distinct
/// line shapes than this barely collapses anyway.
const MAX_TEMPLATES: usize = 512;

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

    /// Past the template cap a new shape is a cluster of its own, with an id
    /// no template has, while a line matching an existing template still
    /// joins it.
    #[test]
    fn templates_stop_at_the_cap() {
        // Digit-free words, so no token is pre-wildcarded.
        let word = |mut i: usize| {
            let mut w = String::new();
            loop {
                w.push((b'a' + (i % 26) as u8) as char);
                i /= 26;
                if i == 0 {
                    return w;
                }
            }
        };
        let mut lines: Vec<String> = (0..(MAX_TEMPLATES + 50))
            .map(|i| format!("{} {} end", word(i), word(i + 7)))
            .collect();
        lines.push(lines[0].clone());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let ids = cluster_ids(&refs, 1.0);
        assert_eq!(
            ids[..MAX_TEMPLATES],
            (0..MAX_TEMPLATES).collect::<Vec<_>>()[..]
        );
        let over = &ids[MAX_TEMPLATES..ids.len() - 1];
        assert_eq!(
            over,
            (MAX_TEMPLATES..MAX_TEMPLATES + 50).collect::<Vec<_>>()
        );
        assert_eq!(ids[ids.len() - 1], 0, "a known shape still clusters");
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
