#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrafficKind { ChatStream, ChatBuffered, Embed }

#[derive(Copy, Clone, Debug)]
pub struct TrafficType {
    pub id: &'static str,
    pub model: &'static str,
    pub kind: TrafficKind,
    pub output_tokens: u32,
    pub weight: u32,
}

/// obench-managed fixture fleet. Names carry the latency-profile keyword the
/// GPU-free benchmark-backend keys off.
pub const FIXTURE_MODELS: &[&str] =
    &["obench-turbo", "obench-base", "obench-code", "obench-large", "obench-embed"];

pub const FIXTURE_GROUPS: &[(&str, u32)] =
    &[("obench-chatbot", 500), ("obench-api", 50), ("obench-analytics", 100)];

/// (tenant name, group, fairshare weight, traffic share)
pub const FIXTURE_TENANTS: &[(&str, &str, u32, u32)] = &[
    ("obench-chatbot", "obench-chatbot", 500, 35),
    ("obench-chatbot-2", "obench-chatbot", 500, 25),
    ("obench-api-batch", "obench-api", 50, 20),
    ("obench-analytics", "obench-analytics", 100, 15),
    ("obench-embeddings", "obench-api", 50, 5),
];

pub const FIXTURE_TRAFFIC: &[TrafficType] = &[
    TrafficType { id: "chat-fast-stream",  model: "obench-turbo", kind: TrafficKind::ChatStream,   output_tokens: 64,  weight: 25 },
    TrafficType { id: "chat-base-stream",  model: "obench-base",  kind: TrafficKind::ChatStream,   output_tokens: 128, weight: 20 },
    TrafficType { id: "chat-base-buffered",model: "obench-base",  kind: TrafficKind::ChatBuffered, output_tokens: 96,  weight: 10 },
    TrafficType { id: "chat-large-stream", model: "obench-large", kind: TrafficKind::ChatStream,   output_tokens: 256, weight: 10 },
    TrafficType { id: "chat-code-stream",  model: "obench-code",  kind: TrafficKind::ChatStream,   output_tokens: 200, weight: 10 },
    TrafficType { id: "embed-batch",       model: "obench-embed", kind: TrafficKind::Embed,        output_tokens: 0,   weight: 25 },
];

/// Pick an index proportional to weights. `r` in [0,1) is supplied by the
/// caller (e.g. rand) so this is deterministic and testable.
pub fn weighted_index(weights: &[u32], r: f64) -> usize {
    let total: u64 = weights.iter().map(|w| *w as u64).sum();
    if total == 0 {
        return 0;
    }
    let mut threshold = r * total as f64;
    for (i, w) in weights.iter().enumerate() {
        threshold -= *w as f64;
        if threshold < 0.0 {
            return i;
        }
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_bucket_low_r() {
        assert_eq!(weighted_index(&[10, 10, 10], 0.0), 0);
    }

    #[test]
    fn picks_last_bucket_high_r() {
        assert_eq!(weighted_index(&[10, 10, 10], 0.999), 2);
    }

    #[test]
    fn respects_weight_boundaries() {
        // weights 1:3 -> r in [0,0.25) -> idx 0, [0.25,1) -> idx 1
        assert_eq!(weighted_index(&[1, 3], 0.20), 0);
        assert_eq!(weighted_index(&[1, 3], 0.30), 1);
    }

    #[test]
    fn zero_weights_safe() {
        assert_eq!(weighted_index(&[0, 0], 0.5), 0);
    }

    #[test]
    fn fixture_catalog_is_nonempty() {
        assert_eq!(FIXTURE_MODELS.len(), 5);
        assert!(FIXTURE_TRAFFIC.iter().any(|t| t.kind == TrafficKind::Embed));
    }
}
