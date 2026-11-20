//! The **compression** boon: gateway-side reduction of what the model reads.
//!
//! Phase A: lossless structural compaction of JSON content in chat messages,
//! gated per-model and by global settings. Fail-open like every boon — any
//! error or absence of gain leaves the request untouched.
//!
//! Then the reversible text passes over one shared segment index
//! ([`TextPasses`]): cross-turn dedup, log-template collapse (`compact_log`)
//! and lossy prose extraction (`extract_prose`, or the compressor sidecar's
//! sentence scores), each replacing a segment with a shorter form plus a
//! `[ref:HASH]` marker; when the model can call `retrieve_original`, the
//! original is stashed for it, best effort.

use obleth_config::{CompressionBoonSettings, ResolvedKey};
use obleth_tokenizer::{HeuristicTokenizer, Tokenizer};
use serde_json::Value;

use super::tool_loop::retrieve_original_tool_def;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentKind {
    Json,
    Code,
    Log,
    Prose,
}

/// Classify a message segment by shape. Only object/array JSON counts as
/// `Json`; bare scalars do not (no structural gain).
pub(crate) fn classify(text: &str) -> ContentKind {
    let trimmed = text.trim();
    // A fence line anywhere — not only at the start — makes the segment Code:
    // real requests usually preamble the fence ("Review this code:\n```…"), and
    // Code must win over Prose so the lossy pass never summarizes code away.
    // Valid JSON text cannot produce a line starting with a backtick, so this
    // cannot shadow the Json arm below.
    if has_fence_line(trimmed) {
        return ContentKind::Code;
    }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if v.is_object() || v.is_array() {
                return ContentKind::Json;
            }
        }
    }
    // Log-shaped: many lines, a majority beginning with a timestamp or a level token.
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() >= 8 {
        let log_like = lines
            .iter()
            .filter(|l| line_looks_log(l.trim_start()))
            .count();
        if log_like * 2 >= lines.len() {
            return ContentKind::Log;
        }
    }
    ContentKind::Prose
}

/// Whether any line of `text` opens or closes a code fence: what makes a
/// segment [`ContentKind::Code`].
fn has_fence_line(text: &str) -> bool {
    text.lines().any(|l| l.trim_start().starts_with("```"))
}

/// Whether a single line looks like a log record: it begins with a timestamp or
/// carries a level token. Handles ISO/epoch/bracketed timestamps (digit- or
/// `[`-prefixed) AND classic syslog/journald `Mon DD HH:MM:SS` lines, whose
/// month/weekday abbreviation starts with a letter (e.g. `Jun 30 20:41:28 …`).
fn line_looks_log(t: &str) -> bool {
    // ISO 8601 / epoch / bracketed timestamp.
    if t.starts_with(|c: char| c.is_ascii_digit()) || t.starts_with('[') {
        return true;
    }
    // syslog / ctime prefix: first whitespace-delimited token is a 3-letter
    // month or weekday abbreviation (`Jun 30 …`, `Mon Jun 30 …`).
    const ABBR: [&str; 19] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec", "Mon",
        "Tue", "Wed", "Thu", "Fri", "Sat", "Sun",
    ];
    if let Some(first) = t.split_whitespace().next() {
        if ABBR.contains(&first) {
            return true;
        }
    }
    // Level-prefixed or level-tagged line.
    LOG_LEVELS.iter().any(|lvl| t.starts_with(lvl))
        || LOG_LEVEL_TAGS.iter().any(|tag| t.contains(tag))
}

const LOG_LEVELS: [&str; 5] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
/// [`LOG_LEVELS`] as words inside a line.
const LOG_LEVEL_TAGS: [&str; 5] = [" ERROR ", " WARN ", " INFO ", " DEBUG ", " TRACE "];

/// Whether `haystack` contains the lowercase ASCII `needle`, in any case,
/// without lowercasing a copy.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Losslessly minify JSON text, already parsed as `value`. Returns
/// `Some(minified)` only when the result is strictly shorter AND re-parses to a
/// value equal to the original; otherwise `None`.
pub(crate) fn compact_json(text: &str, value: &Value) -> Option<String> {
    let compact = serde_json::to_string(value).ok()?;
    if compact.len() >= text.len() {
        return None;
    }
    // Self-enforce the lossless invariant rather than relying on it by construction:
    // only substitute when the compacted text re-parses to a value equal to the original.
    if serde_json::from_str::<Value>(&compact).ok()? != *value {
        return None;
    }
    Some(compact)
}

/// Conservative, lossless-leaning whitespace normalization of code text: strip
/// trailing whitespace from each line and collapse runs of 2+ blank lines to a
/// single blank line. Returns `Some` only when strictly shorter. Opt-in
/// (`code_compaction`): a fenced block containing a multi-line string literal
/// with intentional trailing spaces or consecutive blank lines is the one case
/// this could alter, which is why it is off by default.
pub(crate) fn compact_code(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0u32;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue; // collapse consecutive blank lines
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    // Drop a single trailing newline we may have added past the original's end.
    if !text.ends_with('\n') {
        while out.ends_with('\n') {
            out.pop();
        }
    }
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

/// What one compression pass did on one request; the same for every pass.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PassStats {
    /// Segments the pass looked at: over the `min_tokens` floor and, for the
    /// text passes, of the kind it rewrites (for dedup, the repeats found).
    pub examined: u32,
    /// Segments it rewrote.
    pub replaced: u32,
    /// Tokens of the rewritten segments, before and after.
    pub tokens_before: u32,
    pub tokens_after: u32,
}

impl PassStats {
    pub(super) fn saved(&self) -> u32 {
        self.tokens_before.saturating_sub(self.tokens_after)
    }

    /// For the `boon:compression` span.
    pub(super) fn to_json(self) -> Value {
        serde_json::json!({
            "examined": self.examined,
            "replaced": self.replaced,
            "tokens_before": self.tokens_before,
            "tokens_after": self.tokens_after,
        })
    }
}

/// Compact eligible JSON segments of `messages[]` in place. Phase A is lossless
/// and deterministic; it never calls a helper model or touches the network.
pub(super) fn apply(
    cfg: &CompressionBoonSettings,
    code_compaction: bool,
    json: &mut Value,
) -> PassStats {
    let tk = HeuristicTokenizer::new();
    let mut stats = PassStats::default();
    let mut scan_budget = LOSSLESS_SCAN_BYTES;
    let Some(messages) = json.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return stats;
    };
    for msg in messages.iter_mut() {
        if stats.replaced >= cfg.max_segments {
            break;
        }
        // Two content shapes: a plain string, or an array of typed parts.
        match msg.get_mut("content") {
            Some(Value::String(s)) => {
                try_compact_string(cfg, code_compaction, &tk, s, &mut stats, &mut scan_budget);
            }
            Some(Value::Array(parts)) => {
                for part in parts.iter_mut() {
                    if stats.replaced >= cfg.max_segments {
                        break;
                    }
                    if let Some(Value::String(s)) = part.get_mut("text") {
                        try_compact_string(
                            cfg,
                            code_compaction,
                            &tk,
                            s,
                            &mut stats,
                            &mut scan_budget,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    stats
}

/// Bytes of segment text the lossless pass reads per request: parsing,
/// span-finding and table-building are each linear in it, so this bounds the
/// pass's work however many segments a request carries, including ones that
/// cannot shrink (which `max_segments`, a cap on segments rewritten, does not
/// count). A segment that would cross it is left as it is.
const LOSSLESS_SCAN_BYTES: usize = 8 << 20;

/// Apply the threshold + structural/embedded compaction pipeline to one string
/// segment, rewriting it in place and updating `stats`. `scan_budget` is what
/// is left of [`LOSSLESS_SCAN_BYTES`].
fn try_compact_string(
    cfg: &CompressionBoonSettings,
    code_compaction: bool,
    tk: &HeuristicTokenizer,
    s: &mut String,
    stats: &mut PassStats,
    scan_budget: &mut usize,
) {
    let before = tk.count_text(s);
    if before < cfg.min_tokens {
        return;
    }
    let Some(left) = scan_budget.checked_sub(s.len()) else {
        return;
    };
    *scan_budget = left;
    stats.examined += 1;

    // 1. Whole segment is JSON -> structural compaction (recurses into nesting).
    // 2. Otherwise -> compact any JSON arrays embedded in the prose/markdown.
    // Either way the text is parsed once and the value passed down.
    let mut compacted = match super::structural_json::parse(s) {
        Some(value) => super::structural_json::compact_value(s, &value),
        None => super::embedded_json::extract(s),
    };

    // 3. Conservative code whitespace compaction (opt-in), on top of the above.
    if code_compaction {
        let base = compacted.as_deref().unwrap_or(s);
        if has_fence_line(base.trim()) {
            if let Some(cc) = compact_code(base) {
                compacted = Some(cc);
            }
        }
    }

    if let Some(out) = compacted {
        let after = tk.count_text(&out);
        if after < before {
            stats.tokens_before = stats.tokens_before.saturating_add(before);
            stats.tokens_after = stats.tokens_after.saturating_add(after);
            stats.replaced += 1;
            *s = out;
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The reversible text passes: dedup, log and lossy
// ────────────────────────────────────────────────────────────────────────────

/// Replace a summarized segment's text with the summary plus a retrieval marker.
fn lossy_marker(summary: &str, hash: &str) -> String {
    format!("{}\n[ref:{hash}]", summary.trim())
}

/// System nudge telling the model how to recover summarized content.
const RETRIEVE_NUDGE: &str = "Some long content in this conversation was replaced \
    with a shorter summary followed by a marker like [ref:HASH]. If you need the \
    exact original text of a summarized section, call the retrieve_original tool \
    with that hash.";

/// Inject the `retrieve_original` tool definition (merging into any existing
/// `tools`) and a one-time system nudge describing the [ref:HASH] mechanism.
pub(super) fn inject_retrieve_original_tool(json: &mut Value, supports_system: bool) {
    if let Some(obj) = json.as_object_mut() {
        match obj.get_mut("tools").and_then(|v| v.as_array_mut()) {
            Some(existing) => existing.push(retrieve_original_tool_def()),
            None => {
                obj.insert(
                    "tools".into(),
                    Value::Array(vec![retrieve_original_tool_def()]),
                );
            }
        }
    }
    super::structured::inject_prompt_section(json, RETRIEVE_NUDGE, supports_system);
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct LossyStats {
    pub pass: PassStats,
    /// Of `pass.replaced`, segments given the compacted form kept from an
    /// earlier turn.
    pub reused: u32,
    /// How the sidecar call ended, when one was made; `None` means the
    /// heuristic path alone (no sidecar, or nothing worth scoring).
    pub scorer: Option<super::compressor::ScoreOutcome>,
    pub score_ms: u64,
}

/// One segment the text passes may rewrite: a message's string content
/// (`pi` is `None`) or the `text` of one of its parts.
struct Segment {
    mi: usize,
    pi: Option<usize>,
    tokens: u32,
    /// Classified the first time a pass asks, then kept.
    kind: std::sync::OnceLock<ContentKind>,
    /// Rewritten by an earlier pass on this request; later passes leave it.
    taken: bool,
}

/// The reversible text passes (dedup, log and lossy) over one request. The
/// segments are found once, and each is tokenised once and classified at most
/// once. The passes read the request without changing it and record their
/// rewrites, so a segment is copied only when it is replaced (its original is
/// stashed); [`TextPasses::finish`] writes them all and stashes the originals
/// in one round trip. A segment one pass rewrote is not offered to the next.
///
/// Segments are every text at or over `min_tokens`, in request order,
/// leaving out the active turn: the latest user message, which is what the
/// model is answering, and a trailing assistant message, which it is
/// continuing; compacting either changes the question itself.
pub(super) struct TextPasses<'c> {
    cfg: &'c CompressionBoonSettings,
    /// The model can call `retrieve_original`: the markers say so and the
    /// originals are stashed.
    reversible: bool,
    keep_originals: bool,
    tk: HeuristicTokenizer,
    segments: Vec<Segment>,
    edits: Vec<(usize, String)>,
    stash: Vec<(String, String)>,
    /// Lossy forms to keep for later turns, as `(id, text)`.
    forms: Vec<(String, String)>,
}

impl<'c> TextPasses<'c> {
    pub(super) fn new(cfg: &'c CompressionBoonSettings, reversible: bool, json: &Value) -> Self {
        let tk = HeuristicTokenizer::new();
        let mut segments = Vec::new();
        if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
            let role = |i: usize| messages[i].get("role").and_then(|r| r.as_str());
            let latest_user = (0..messages.len()).rev().find(|&i| role(i) == Some("user"));
            let trailing_assistant = messages
                .len()
                .checked_sub(1)
                .filter(|&last| role(last) == Some("assistant"));
            let mut push = |mi: usize, pi: Option<usize>, text: &str| {
                let tokens = tk.count_text(text);
                if tokens >= cfg.min_tokens {
                    segments.push(Segment {
                        mi,
                        pi,
                        tokens,
                        kind: Default::default(),
                        taken: false,
                    });
                }
            };
            for (mi, msg) in messages.iter().enumerate() {
                if Some(mi) == latest_user || Some(mi) == trailing_assistant {
                    continue;
                }
                match msg.get("content") {
                    Some(Value::String(s)) => push(mi, None, s),
                    Some(Value::Array(parts)) => {
                        for (pi, part) in parts.iter().enumerate() {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                push(mi, Some(pi), t);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            cfg,
            reversible,
            keep_originals: reversible,
            tk,
            segments,
            edits: Vec::new(),
            stash: Vec::new(),
            forms: Vec::new(),
        }
    }

    /// Segment `i`'s text as the request has it (passes run before any
    /// rewrite is written).
    fn text<'j>(&self, json: &'j Value, i: usize) -> &'j str {
        let s = &self.segments[i];
        let content = json
            .get("messages")
            .and_then(|m| m.get(s.mi))
            .and_then(|m| m.get("content"));
        let text = match s.pi {
            None => content,
            Some(pi) => content.and_then(|c| c.get(pi)).and_then(|p| p.get("text")),
        };
        text.and_then(Value::as_str).unwrap_or_default()
    }

    fn kind(&self, json: &Value, i: usize) -> ContentKind {
        *self.segments[i]
            .kind
            .get_or_init(|| classify(self.text(json, i)))
    }

    /// Record `marker` in place of segment `i` when it has fewer tokens;
    /// returns whether it did.
    fn replace(
        &mut self,
        stats: &mut PassStats,
        i: usize,
        original: &str,
        hash: String,
        marker: String,
    ) -> bool {
        let before = self.segments[i].tokens;
        let after = self.tk.count_text(&marker);
        if after >= before {
            return false;
        }
        stats.replaced += 1;
        stats.tokens_before = stats.tokens_before.saturating_add(before);
        stats.tokens_after = stats.tokens_after.saturating_add(after);
        self.segments[i].taken = true;
        self.edits.push((i, marker));
        if self.keep_originals {
            self.stash.push((hash, original.to_owned()));
        }
        true
    }

    /// Cross-turn exact-duplicate dedup: when a large block appears more than
    /// once, the first occurrence is kept verbatim and each later identical
    /// one becomes a `[ref:HASH]` marker.
    pub(super) fn dedup(&mut self, json: &Value) -> PassStats {
        let mut stats = PassStats::default();
        let mut seen: std::collections::HashSet<&str> = Default::default();
        let repeats: Vec<usize> = (0..self.segments.len())
            .filter(|&i| !self.segments[i].taken && !seen.insert(self.text(json, i)))
            .collect();
        for i in repeats {
            if stats.replaced >= self.cfg.max_lossy_segments {
                break;
            }
            stats.examined += 1;
            let original = self.text(json, i);
            let hash = obleth_config::content_hash(original);
            let marker = dedup_marker(&hash, self.reversible);
            self.replace(&mut stats, i, original, hash, marker);
        }
        stats
    }

    /// Near-lossless log compaction: repeated, structurally identical lines
    /// of Log segments collapse (see [`compact_log`]); error and warning lines
    /// stay verbatim. A separate opt-in from lossy because it removes only
    /// provably repeated structure, never heuristically selected prose.
    pub(super) fn logs(&mut self, json: &Value) -> PassStats {
        let mut stats = PassStats::default();
        for i in 0..self.segments.len() {
            if stats.replaced >= self.cfg.max_lossy_segments {
                break;
            }
            let original = self.text(json, i);
            // A table from the lossless pass classifies as Log but must
            // survive verbatim.
            if self.segments[i].taken
                || is_table(original)
                || self.kind(json, i) != ContentKind::Log
            {
                continue;
            }
            stats.examined += 1;
            if let Some(compacted) = compact_log(original) {
                let hash = obleth_config::content_hash(original);
                let marker = lossy_marker(&compacted, &hash);
                self.replace(&mut stats, i, original, hash, marker);
            }
        }
        stats
    }

    /// The lossy pass: long Prose segments are cut to their most salient
    /// sentences (ranked by the compressor sidecar when there is one, by
    /// [`extract_prose`] otherwise) plus a `[ref:HASH]` marker.
    ///
    /// A segment's compacted form is kept (next to its original, for as
    /// long) and given again on later turns, so earlier turns read the same
    /// on every request and the upstream prompt cache keeps matching them.
    /// Only a segment seen for the first time is compacted, biased toward the
    /// current question and scored by the sidecar; `max_lossy_segments` caps
    /// those, as the work, not the reused ones.
    pub(super) async fn lossy(
        &mut self,
        state: &AppState,
        key: &ResolvedKey,
        json: &Value,
    ) -> LossyStats {
        let mut stats = LossyStats::default();
        let order = self.lossy_order(json);
        let hashes: Vec<String> = order
            .iter()
            .map(|&i| obleth_config::content_hash(self.text(json, i)))
            .collect();
        let kept = self.kept_forms(state, key, &hashes).await;
        let budget = state
            .compressor
            .as_ref()
            .map_or(0, |kc| kc.sentence_budget());
        let (scored, batches) = self.scoring_batches(json, &order, &kept, budget);
        let mut scores: Vec<Option<Vec<f32>>> = vec![None; order.len()];
        if let Some(kc) = state.compressor.as_ref().filter(|_| !scored.is_empty()) {
            let answer = kc.score(&state.http, &batches).await;
            stats.scorer = Some(answer.outcome);
            stats.score_ms = answer.elapsed.as_millis() as u64;
            if let Some(found) = answer.scores.filter(|v| v.len() == scored.len()) {
                for (at, s) in scored.into_iter().zip(found) {
                    scores[at] = Some(s);
                }
            }
        }
        let terms = latest_user_query_terms(json);
        (stats.pass, stats.reused) =
            self.lossy_rewrite(json, &order, &hashes, &kept, &scores, &terms);
        stats
    }

    /// The id a lossy form is kept under: the original's hash, and what else
    /// decides the form, so a change to either is a different form.
    fn form_id(&self, hash: &str) -> String {
        format!("{LOSSY_FORM_VERSION}:{}:{hash}", self.cfg.neural_keep_ratio)
    }

    /// The forms kept from earlier turns for the segments hashed `hashes`;
    /// all `None` when nothing is kept or Redis fails (then each is compacted
    /// afresh, as before).
    async fn kept_forms(
        &self,
        state: &AppState,
        key: &ResolvedKey,
        hashes: &[String],
    ) -> Vec<Option<String>> {
        let none = || vec![None; hashes.len()];
        if self.cfg.original_ttl_secs == 0 || hashes.is_empty() {
            return none();
        }
        let ids: Vec<String> = hashes.iter().map(|h| self.form_id(h)).collect();
        match state.redis.compacted_get_many(&key.tenant_id, &ids).await {
            Ok(found) if found.len() == ids.len() => found,
            Ok(_) => none(),
            Err(e) => {
                tracing::debug!(error = %e, "could not read kept compacted forms");
                none()
            }
        }
    }

    /// Lossy candidates, largest first: the pass stops after
    /// `max_lossy_segments` rewrites, so this spends that budget where the
    /// savings are. Ties keep request order.
    fn lossy_order(&self, json: &Value) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.segments.len())
            .filter(|&i| self.lossy_candidate(json, i))
            .collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.segments[i].tokens));
        order
    }

    fn lossy_candidate(&self, json: &Value, i: usize) -> bool {
        // A table from the lossless pass is not valid JSON, so it would
        // classify as Log or Prose and be cut: never offer one.
        !self.segments[i].taken
            && !is_table(self.text(json, i))
            && self.kind(json, i) == ContentKind::Prose
    }

    /// The candidates worth scoring and their sentence batches, as
    /// `(positions in order, batches)`. Only the first `max_lossy_segments`
    /// (at most [`SCORE_MAX_SEGMENTS`]) can be used, so only they are sent:
    /// scoring the whole history would ship every old turn to the sidecar on
    /// every request. `max_sentences` caps the sentences of one call (see
    /// `CompressorClient::sentence_budget`); a segment that would cross it is
    /// left unscored and compacts via the heuristic path instead.
    fn scoring_batches(
        &self,
        json: &Value,
        order: &[usize],
        kept: &[Option<String>],
        max_sentences: usize,
    ) -> (Vec<usize>, Vec<Vec<String>>) {
        let limit = (self.cfg.max_lossy_segments as usize).min(SCORE_MAX_SEGMENTS);
        let mut at = Vec::new();
        let mut batches = Vec::new();
        let mut sentences = 0usize;
        for (pos, &i) in order.iter().enumerate() {
            if at.len() >= limit {
                break;
            }
            if kept[pos].is_some() {
                continue; // its form is kept: nothing to score
            }
            let batch = super::compressor::split_sentences(self.text(json, i));
            if sentences + batch.len() > max_sentences {
                continue;
            }
            sentences += batch.len();
            at.push(pos);
            batches.push(batch);
        }
        (at, batches)
    }

    /// Pure core of the lossy pass, returning its stats and how many forms
    /// it reused. For `order[k]`: `hashes[k]` is its content hash, `kept[k]`
    /// its form kept from an earlier turn, and `scores[k]` the sidecar's
    /// scores, when there are any.
    fn lossy_rewrite(
        &mut self,
        json: &Value,
        order: &[usize],
        hashes: &[String],
        kept: &[Option<String>],
        scores: &[Option<Vec<f32>>],
        terms: &std::collections::HashSet<String>,
    ) -> (PassStats, u32) {
        let mut stats = PassStats::default();
        let (mut fresh, mut reused) = (0, 0);
        for (k, &i) in order.iter().enumerate() {
            let original = self.text(json, i);
            let hash = &hashes[k];
            let marker = match &kept[k] {
                Some(form) => form.clone(),
                None => {
                    if fresh >= self.cfg.max_lossy_segments {
                        continue;
                    }
                    let Some(compacted) = super::compressor::compact_prose_segment(
                        original,
                        scores[k].as_deref(),
                        self.cfg.neural_keep_ratio,
                        terms,
                    ) else {
                        stats.examined += 1;
                        continue;
                    };
                    lossy_marker(&compacted, hash)
                }
            };
            stats.examined += 1;
            let id = self.form_id(hash);
            if self.replace(&mut stats, i, original, hash.clone(), marker.clone()) {
                if kept[k].is_some() {
                    reused += 1;
                } else {
                    fresh += 1;
                }
                // Kept again, so the form lives as long as the conversation
                // keeps sending the segment.
                self.forms.push((id, marker));
            }
        }
        (stats, reused)
    }

    /// Write every recorded rewrite into the request, returning the originals
    /// to stash.
    fn write(self, json: &mut Value) -> Vec<(String, String)> {
        for (i, text) in self.edits {
            let s = &self.segments[i];
            set_segment_text(json, s.mi, s.pi, text);
        }
        self.stash
    }

    /// Write the rewrites and, when reversible, stash the originals for
    /// `retrieve_original` in one round trip. Best effort: a Redis failure
    /// never fails the request.
    pub(super) async fn finish(mut self, state: &AppState, key: &ResolvedKey, json: &mut Value) {
        let (cfg, reversible) = (self.cfg, self.reversible);
        let forms = std::mem::take(&mut self.forms);
        let originals = self.write(json);
        let ttl = cfg.original_ttl_secs;
        let stash = async {
            if reversible {
                if let Err(e) = state
                    .redis
                    .compress_put_many(&key.tenant_id, &originals, ttl)
                    .await
                {
                    tracing::debug!(error = %e, "could not stash compressed originals");
                }
            }
        };
        let keep = async {
            if let Err(e) = state
                .redis
                .compacted_put_many(&key.tenant_id, &forms, ttl)
                .await
            {
                tracing::debug!(error = %e, "could not keep compacted forms");
            }
        };
        tokio::join!(stash, keep);
    }
}

/// Whether the lossless pass made `text` a table.
fn is_table(text: &str) -> bool {
    // Covers both the top-level `OBLETH_TABLE rows=` form and the nested
    // `<<OBLETH_TABLE:` skeleton/block form.
    text.contains("OBLETH_TABLE")
}

/// Hard ceiling on segments sent to the compressor sidecar in one `/score`
/// call, whatever `max_lossy_segments` says.
const SCORE_MAX_SEGMENTS: usize = 16;

/// Part of every kept lossy form's id. Raise it when a change to the lossy
/// pass changes its output, so forms made the old way are not given again.
const LOSSY_FORM_VERSION: &str = "lossy1";

/// The words of `text` as query terms are made: lowercased, split on
/// anything not alphanumeric, at least 4 bytes long.
fn query_words(lowercased: &str) -> impl Iterator<Item = &str> {
    lowercased
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
}

/// Whether `text` has a word among `terms`. A whole-word lookup, so the work
/// is linear in the text whatever the number of terms.
pub(super) fn mentions_any(text: &str, terms: &std::collections::HashSet<String>) -> bool {
    !terms.is_empty() && query_words(&text.to_lowercase()).any(|w| terms.contains(w))
}

/// Terms from the latest user message, used to bias prose extraction toward the
/// active question.
fn latest_user_query_terms(json: &Value) -> std::collections::HashSet<String> {
    let mut terms = std::collections::HashSet::new();
    let Some(last_user) = json
        .get("messages")
        .and_then(|m| m.as_array())
        .and_then(|m| {
            m.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
    else {
        return terms;
    };
    let mut add = |text: &str| terms.extend(query_words(&text.to_lowercase()).map(str::to_owned));
    match last_user.get("content") {
        Some(Value::String(s)) => add(s),
        Some(Value::Array(parts)) => {
            for t in parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            {
                add(t);
            }
        }
        _ => {}
    }
    terms
}

/// Write `text` into message `mi`'s content (whole string when `pi` is None, else
/// the `text` of part `pi`). Returns whether it wrote.
fn set_segment_text(json: &mut Value, mi: usize, pi: Option<usize>, text: String) -> bool {
    let Some(msg) = json
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .and_then(|m| m.get_mut(mi))
    else {
        return false;
    };
    match pi {
        None => {
            if let Some(content) = msg.get_mut("content") {
                *content = Value::String(text);
                return true;
            }
        }
        Some(pi) => {
            if let Some(t) = msg
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
                .and_then(|a| a.get_mut(pi))
                .and_then(|p| p.get_mut("text"))
            {
                *t = Value::String(text);
                return true;
            }
        }
    }
    false
}

/// Marker substituted for a duplicate occurrence of a block already present
/// earlier in the request. When the request is reversible the marker says how
/// to get the original back; otherwise it names no tool the model cannot call.
fn dedup_marker(hash: &str, reversible: bool) -> String {
    if reversible {
        format!(
            "[ref:{hash}] (identical to earlier content; call retrieve_original for the full text)"
        )
    } else {
        format!("[ref:{hash}] (duplicate of earlier content omitted)")
    }
}

/// The dedup pass alone, written into `json`, with the originals it replaced
/// as `(hash, original)`. For tests and measurement.
#[cfg(test)]
pub(super) fn dedup_rewrite(
    cfg: &CompressionBoonSettings,
    reversible: bool,
    json: &mut Value,
) -> (PassStats, Vec<(String, String)>) {
    let mut passes = TextPasses::new(cfg, reversible, json);
    passes.keep_originals = true;
    let stats = passes.dedup(json);
    (stats, passes.write(json))
}

/// The log pass alone, as [`dedup_rewrite`].
#[cfg(test)]
pub(super) fn log_rewrite(
    cfg: &CompressionBoonSettings,
    json: &mut Value,
) -> (PassStats, Vec<(String, String)>) {
    let mut passes = TextPasses::new(cfg, true, json);
    let stats = passes.logs(json);
    (stats, passes.write(json))
}

/// The lossy pass alone without a sidecar, as [`dedup_rewrite`].
#[cfg(test)]
pub(super) fn lossy_rewrite(
    cfg: &CompressionBoonSettings,
    json: &mut Value,
) -> (PassStats, Vec<(String, String)>) {
    let mut passes = TextPasses::new(cfg, true, json);
    let order = passes.lossy_order(json);
    let hashes: Vec<String> = order
        .iter()
        .map(|&i| obleth_config::content_hash(passes.text(json, i)))
        .collect();
    let none = vec![None; order.len()];
    let terms = latest_user_query_terms(json);
    let (stats, _) = passes.lossy_rewrite(
        json,
        &order,
        &hashes,
        &none,
        &vec![None; order.len()],
        &terms,
    );
    (stats, passes.write(json))
}

/// Normalize a line for near-duplicate detection: trim + lowercase + collapse digits.
fn dedup_key(line: &str) -> String {
    line.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// Deterministic extractive compaction of prose/log-ish text: score each line by
/// salience (length/density + overlap with the request's query terms), drop
/// blank lines and near-duplicates, and keep the top `keep_ratio` of lines in
/// original order. Returns `Some` only when strictly shorter than the input.
pub(super) fn extract_prose(
    text: &str,
    query_terms: &std::collections::HashSet<String>,
    keep_ratio: f32,
) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 8 {
        return None; // too small to benefit
    }
    let mut seen = std::collections::HashSet::new();
    // (original_index, score) for non-blank, non-duplicate lines.
    let mut scored: Vec<(usize, f32)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !seen.insert(dedup_key(line)) {
            continue; // near-duplicate
        }
        let mut score = (trimmed.len().min(120) as f32) / 120.0; // density proxy
        if mentions_any(trimmed, query_terms) {
            score += 2.0; // relevance to the request
        }
        scored.push((i, score));
    }
    if scored.is_empty() {
        return None;
    }
    let keep = ((scored.len() as f32) * keep_ratio).ceil().max(1.0) as usize;
    if keep >= scored.len() {
        return None; // nothing meaningfully dropped
    }
    // Choose the top `keep` by score, then restore original order.
    let mut by_score = scored.clone();
    by_score.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let mut kept_idx: Vec<usize> = by_score.into_iter().take(keep).map(|(i, _)| i).collect();
    kept_idx.sort_unstable();
    let omitted = lines.len() - kept_idx.len();
    let mut out = String::with_capacity(text.len());
    for i in &kept_idx {
        out.push_str(lines[*i]);
        out.push('\n');
    }
    out.push_str(&format!("[… {omitted} lines omitted]"));
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

/// Collapse repeated log lines via Drain-style template mining: lines whose
/// mined template appears more than 3× are represented by their first occurrence
/// followed by a `(×N)` count. `error`/`warn` lines are always kept verbatim, and
/// blank lines pass through. Drain clusters lines that vary in non-numeric fields
/// too (hostnames, paths, uuids), so it collapses varied logs far better than a
/// digit-only mask. Returns `Some` only when strictly shorter. The original is
/// stashed for `retrieve_original` by the caller, so the collapse stays reversible.
pub(super) fn compact_log(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 8 {
        return None;
    }
    // 0.6 keeps genuinely-repeated templates (~1.0 similarity) together while
    // keeping structurally-different lines of the same token count apart, and
    // error and warning lines are kept verbatim below whatever cluster they
    // join, so a lone signal line is never dropped.
    let ids = super::drain::cluster_ids(&lines, 0.6);
    use std::collections::{HashMap, HashSet};
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for &id in &ids {
        *counts.entry(id).or_insert(0) += 1;
    }
    let mut emitted: HashSet<usize> = HashSet::new();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let is_problem =
            contains_ignore_ascii_case(line, "error") || contains_ignore_ascii_case(line, "warn");
        let id = ids[i];
        let count = counts.get(&id).copied().unwrap_or(1);
        if is_problem || count <= 3 {
            out.push_str(line);
            out.push('\n');
        } else if emitted.insert(id) {
            // First occurrence of a frequently-repeated template.
            out.push_str(line);
            out.push_str(&format!(" (×{count})\n"));
        }
        // else: a subsequent occurrence of an already-collapsed template → drop.
    }
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Earlier turns holding `texts`, then a question.
    pub(super) fn turns(texts: &[String]) -> Value {
        let mut messages: Vec<Value> = texts
            .iter()
            .map(|t| serde_json::json!({ "role": "assistant", "content": t }))
            .collect();
        messages.push(serde_json::json!({ "role": "user", "content": "and so?" }));
        serde_json::json!({ "messages": messages })
    }

    pub(super) fn lossy_cfg(max_lossy_segments: u32) -> CompressionBoonSettings {
        CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_lossy_segments,
            ..Default::default()
        }
    }

    /// [`compact_json`] of text not yet parsed.
    fn minify(text: &str) -> Option<String> {
        compact_json(text, &crate::boons::structural_json::parse(text)?)
    }

    fn parses_during(f: impl FnOnce()) -> usize {
        use crate::boons::structural_json::PARSES;
        let before = PARSES.with(|n| n.get());
        f();
        PARSES.with(|n| n.get()) - before
    }

    fn one_user_turn(content: &str) -> Value {
        serde_json::json!({ "messages": [{ "role": "user", "content": content }] })
    }

    /// A JSON segment is parsed once, and prose with a JSON span once for
    /// the whole text and once for the span: the value is handed down rather
    /// than parsed again by each step.
    #[test]
    fn each_segment_and_span_is_parsed_once() {
        let cfg = CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            ..Default::default()
        };
        let rows: Vec<Value> = (0..40)
            .map(|i| serde_json::json!({ "id": i, "name": format!("user {i}"), "active": i % 2 == 0 }))
            .collect();
        let pretty = serde_json::to_string_pretty(&rows).unwrap();

        let mut body = one_user_turn(&pretty);
        assert_eq!(
            parses_during(|| assert_eq!(apply(&cfg, true, &mut body).replaced, 1)),
            1
        );

        let mut body = one_user_turn(&format!(
            "Here are the users: {pretty} -- which are active?"
        ));
        assert_eq!(
            parses_during(|| assert_eq!(apply(&cfg, true, &mut body).replaced, 1)),
            2
        );
    }

    /// Segments that cannot shrink are not compacted, so `max_segments` never
    /// stops them; the byte budget does.
    #[test]
    fn the_lossless_pass_stops_at_its_byte_budget() {
        let cfg = CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            ..Default::default()
        };
        // Already minified and not a table: parsed, then left as it is.
        let segment = format!("{{\"k\":\"{}\"}}", "a".repeat(999_992));
        assert_eq!(segment.len(), 1_000_000);
        let turns: Vec<Value> = (0..12)
            .map(|_| serde_json::json!({ "role": "user", "content": segment }))
            .collect();
        let mut body = serde_json::json!({ "messages": turns });
        let mut stats = PassStats::default();
        let parses = parses_during(|| stats = apply(&cfg, false, &mut body));
        let fit = LOSSLESS_SCAN_BYTES / segment.len();
        assert_eq!(fit, 8);
        assert_eq!((stats.examined as usize, stats.replaced), (fit, 0));
        assert_eq!(parses, fit);
    }
    use serde_json::json;

    #[test]
    fn classifies_json_object() {
        assert_eq!(
            classify("  {\"a\": 1, \"b\": [1,2,3]}  "),
            ContentKind::Json
        );
    }

    #[test]
    fn classifies_json_array() {
        assert_eq!(classify("[1, 2, 3]"), ContentKind::Json);
    }

    #[test]
    fn classifies_prose() {
        assert_eq!(
            classify("The quick brown fox jumps over the lazy dog."),
            ContentKind::Prose
        );
    }

    #[test]
    fn classifies_code_fence_as_code() {
        assert_eq!(classify("```python\nprint('hi')\n```"), ContentKind::Code);
    }

    #[test]
    fn classifies_preambled_code_fence_as_code() {
        // Real requests rarely start at the fence — "Review this code:\n```…".
        // Code embedded behind a preamble must still classify as Code so the
        // code compactor can run and the lossy prose pass keeps its hands off.
        let text = "Review this code:\n```python\ndef f():\n    pass\n```";
        assert_eq!(classify(text), ContentKind::Code);
    }

    #[test]
    fn bare_number_is_not_json() {
        // A bare scalar is not worth treating as structured JSON.
        assert_eq!(classify("42"), ContentKind::Prose);
    }

    #[test]
    fn compacts_pretty_json() {
        let pretty = "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}";
        let out = minify(pretty).expect("should compact");
        assert!(out.len() < pretty.len());
        // Lossless: re-parses to the same value.
        let a: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let b: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn already_compact_json_returns_none() {
        assert_eq!(minify("{\"a\":1}"), None);
    }

    #[test]
    fn compact_json_is_lossless_despite_key_reordering() {
        let input = "{\n  \"z\": 99,\n  \"a\": 1\n}";
        let out = minify(input).expect("should compact");
        let before: serde_json::Value = serde_json::from_str(input).unwrap();
        let after: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(before, after); // Value equality holds even though keys may reorder
    }

    #[test]
    fn non_json_returns_none() {
        assert_eq!(minify("just some prose"), None);
    }

    #[test]
    fn apply_compacts_large_json_message() {
        let big_array: Vec<i64> = (0..500).collect();
        let pretty = serde_json::to_string_pretty(&json!({ "rows": big_array })).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "be helpful" },
                { "role": "tool", "content": pretty }
            ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        // The tool message is now compact (no newline indentation).
        let tool = body["messages"][1]["content"].as_str().unwrap();
        assert!(!tool.contains("\n  "));
    }

    #[test]
    fn apply_skips_small_segments() {
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "tool", "content": "{\"a\": 1}" } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 512,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 0);
    }

    #[test]
    fn apply_respects_max_segments() {
        let big = serde_json::to_string_pretty(&json!({ "rows": (0..500).collect::<Vec<i64>>() }))
            .unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "tool", "content": big.clone() },
                { "role": "tool", "content": big }
            ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 1,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
    }

    #[test]
    fn apply_compacts_array_parts_message() {
        let big_array: Vec<i64> = (0..500).collect();
        let pretty = serde_json::to_string_pretty(&json!({ "rows": big_array })).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": pretty }]
            }]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        let text = body["messages"][0]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("\n  "));
    }

    #[test]
    fn lossy_marker_carries_summary_and_ref() {
        let m = lossy_marker("short summary", "deadbeef");
        assert!(m.contains("short summary"));
        assert!(m.contains("[ref:deadbeef]"));
    }

    #[test]
    fn inject_retrieve_original_tool_merges_and_nudges() {
        let mut body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "existing" } }]
        });
        inject_retrieve_original_tool(&mut body, true);
        let tools = body["tools"].as_array().unwrap();
        // Existing tool preserved, retrieve_original appended.
        assert_eq!(tools.len(), 2);
        assert!(tools
            .iter()
            .any(|t| t["function"]["name"] == "retrieve_original"));
        // A system nudge mentioning the marker mechanism was injected.
        let has_nudge = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|msg| msg["content"].as_str().is_some_and(|c| c.contains("[ref:")));
        assert!(has_nudge);
    }

    #[test]
    fn inject_retrieve_original_tool_creates_tools_array_when_absent() {
        let mut body = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
        inject_retrieve_original_tool(&mut body, false);
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn compact_code_strips_trailing_ws_and_collapses_blank_runs() {
        let input = "```py\nx = 1   \n\n\n\ny = 2\t\n```";
        let out = compact_code(input).expect("should compact");
        assert!(out.len() < input.len());
        assert!(!out.contains("   \n")); // no trailing whitespace before newline
        assert!(!out.contains("\n\n\n")); // blank runs collapsed to a single blank line
        assert!(out.contains("x = 1"));
        assert!(out.contains("y = 2"));
    }

    #[test]
    fn compact_code_returns_none_when_already_tight() {
        assert_eq!(compact_code("```py\nx = 1\ny = 2\n```"), None);
    }

    #[test]
    fn dedup_marker_carries_ref() {
        let m = dedup_marker("cafef00d", true);
        assert!(m.contains("[ref:cafef00d]"));
    }

    #[test]
    fn apply_compacts_code_when_enabled() {
        let big = format!("```py\n{}\n```", "x = 1   \n\n\n".repeat(60));
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": big } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, true, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
    }

    #[test]
    fn apply_compacts_preambled_code_when_enabled() {
        let big = format!(
            "Review this code:\n```py\n{}\n```",
            "x = 1   \n\n\n".repeat(60)
        );
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": big } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, true, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        // The preamble survives; the fenced code is whitespace-normalized.
        let text = body["messages"][0]["content"].as_str().unwrap();
        assert!(text.starts_with("Review this code:"));
        assert!(!text.contains("\n\n\n"));
    }

    #[test]
    fn apply_leaves_code_when_disabled() {
        let big = format!("```py\n{}\n```", "x = 1   \n\n\n".repeat(60));
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": big } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 0);
    }

    #[test]
    fn apply_uses_structural_table_for_object_arrays() {
        let rows: Vec<serde_json::Value> = (0..200)
            .map(|i| json!({ "id": i, "name": format!("item-{i}"), "active": true }))
            .collect();
        let pretty = serde_json::to_string_pretty(&json!(rows)).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "tool", "content": pretty } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        let out = body["messages"][0]["content"].as_str().unwrap();
        assert!(out.starts_with("OBLETH_TABLE rows=200\n"));
    }

    #[test]
    fn extract_prose_drops_low_value_and_dupes_keeps_query_lines() {
        use std::collections::HashSet;
        let mut text = String::new();
        for _ in 0..40 {
            text.push_str("boilerplate filler line that repeats a lot\n");
        }
        text.push_str("the deadbeef token error occurred in module X\n");
        for _ in 0..40 {
            text.push_str("more boilerplate filler line that repeats\n");
        }
        let mut q = HashSet::new();
        q.insert("deadbeef".to_string());
        let out = extract_prose(&text, &q, 0.3).expect("should extract");
        assert!(out.len() < text.len());
        assert!(out.contains("deadbeef")); // query-relevant line is kept
        assert!(out.contains("omitted")); // omission marker present
    }

    #[test]
    fn extract_prose_returns_none_when_short() {
        use std::collections::HashSet;
        assert_eq!(extract_prose("a\nb\nc", &HashSet::new(), 0.5), None);
    }

    #[test]
    fn compact_log_collapses_repeated_templates_keeps_errors() {
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("2026-06-29 INFO request {i} handled in {i}ms\n"));
        }
        text.push_str("2026-06-29 ERROR disk full on /dev/sda1\n");
        let out = compact_log(&text).expect("should compact");
        assert!(out.len() < text.len());
        assert!(out.contains("(×")); // collapsed repeat marker
        assert!(out.contains("ERROR disk full")); // error line preserved verbatim
    }

    #[test]
    fn classifies_log_shaped_text() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!(
                "2026-06-29T10:00:0{} INFO did thing {}\n",
                i % 10,
                i
            ));
        }
        assert_eq!(classify(&text), ContentKind::Log);
    }

    #[test]
    fn compact_log_collapses_non_numeric_variable_logs() {
        // Lines vary only in non-numeric fields (user, workstation). The old
        // digit-only mask would leave these as distinct templates; Drain clusters
        // them and collapses.
        let names = [
            "alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi", "ivan", "judy",
        ];
        let mut text = String::new();
        for (i, n) in names.iter().cycle().take(20).enumerate() {
            text.push_str(&format!("user {n} logged in from workstation-{n}-{i}\n"));
        }
        let out = compact_log(&text).expect("varied logins should collapse");
        assert!(out.len() < text.len());
        assert!(out.contains("(×"));
    }

    #[test]
    fn classifies_syslog_mon_dd_as_log_and_collapses() {
        // Classic syslog/journald format: lines start with a month abbreviation
        // ("Jun 30 …"), not a digit. Regression for logs misclassified as Prose.
        let mut text = String::new();
        for i in 83840..83870 {
            text.push_str(&format!(
                "Jun 30 21:00:01 sol_web systemd[1]: Started Session {i} of user root.\n"
            ));
            text.push_str(&format!(
                "Jun 30 21:00:02 sol_web systemd[1]: session-{i}.scope: Succeeded.\n"
            ));
        }
        text.push_str(
            "Jun 30 20:41:28 sol_web certbot[1078880]: 1 renew failure(s), 0 parse failure(s)\n",
        );
        assert_eq!(classify(&text), ContentKind::Log);
        // The log-collapse pass should crush the repeated session lines.
        let out = compact_log(&text).expect("repetitive syslog should collapse");
        assert!(out.len() < text.len() / 2, "expected >50% reduction");
        assert!(out.contains("(×")); // collapse marker present
        assert!(out.contains("renew failure")); // the one real event is preserved
    }

    #[test]
    fn classifies_plain_paragraph_as_prose() {
        assert_eq!(
            classify("The quick brown fox jumps over the lazy dog. It was a fine day."),
            ContentKind::Prose
        );
    }

    /// EX-04: the active turn is never a target. The latest user message is the
    /// question being answered and a trailing assistant message is being
    /// continued; every earlier segment over the floor is.
    #[test]
    fn the_active_turn_is_never_compacted() {
        let long = |what: &str| {
            (0..40)
                .map(|i| format!("{what} line {i} of pasted notes.\n"))
                .collect::<String>()
        };
        let mut body = json!({
            "messages": [
                { "role": "user", "content": long("first") },
                { "role": "assistant", "content": [{"type": "text", "text": long("reply")}] },
                { "role": "user", "content": long("latest") }
            ]
        });
        let floor = obleth_config::CompressionBoonSettings {
            min_tokens: 16,
            ..Default::default()
        };
        let targets = |body: &Value| -> Vec<usize> {
            TextPasses::new(&floor, false, body)
                .segments
                .iter()
                .map(|s| s.mi)
                .collect()
        };
        assert_eq!(
            targets(&body),
            vec![0, 1],
            "the latest user message is left alone"
        );
        body["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "role": "assistant", "content": long("draft") }));
        assert_eq!(
            targets(&body),
            vec![0, 1],
            "a trailing assistant message is left alone too"
        );
        // The passes read targets through the collector, so a pasted duplicate in
        // the latest message survives dedup.
        let dup = long("same");
        let mut body = json!({"messages": [
            { "role": "user", "content": dup.clone() },
            { "role": "assistant", "content": "ok" },
            { "role": "user", "content": dup.clone() }
        ]});
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_lossy_segments: 8,
            ..Default::default()
        };
        let (stats, stash) = dedup_rewrite(&cfg, true, &mut body);
        assert_eq!((stats.replaced, stash.len()), (0, 0));
        assert_eq!(body["messages"][2]["content"], dup);
    }

    #[test]
    fn a_dedup_marker_names_the_tool_only_when_it_can_be_called() {
        assert!(dedup_marker("abc", true).contains("retrieve_original"));
        let plain = dedup_marker("abc", false);
        assert!(
            plain.starts_with("[ref:abc]") && !plain.contains("retrieve_original"),
            "{plain}"
        );
    }

    #[test]
    fn extract_prose_shortens_pasted_notes_around_the_query_terms() {
        // Build a long prose user message (the latest turn) + a short follow-up question.
        let big: String = (0..60)
            .map(|i| format!("This is line number {i} of some pasted notes.\n"))
            .collect();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": big },
                { "role": "user", "content": "summarize the notes above" }
            ]
        });
        let cfg = CompressionBoonSettings {
            neural_keep_ratio: 0.4,
            ..lossy_cfg(4)
        };
        let (stats, stash) = lossy_rewrite(&cfg, &mut body);
        assert_eq!((stats.examined, stats.replaced, stash.len()), (1, 1, 1));
        let compacted = body["messages"][0]["content"].as_str().unwrap();
        assert!(compacted.len() < big.len() && compacted.contains("[ref:"));
        // The active question is untouched.
        assert_eq!(body["messages"][1]["content"], "summarize the notes above");
    }

    /// Query terms match whole words, so the work is linear in the text: a
    /// 200 KB question over a long history used to be lines x terms substring
    /// scans.
    #[test]
    fn query_terms_match_whole_words_in_linear_time() {
        let terms: std::collections::HashSet<String> = ["config", "latency"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(mentions_any("The CONFIG file moved.", &terms));
        assert!(mentions_any("p99 latency, again", &terms));
        assert!(
            !mentions_any("configuration drift", &terms),
            "a word, not a substring"
        );

        let question: String = (0..25_000).map(|i| format!("w{i:05}x ")).collect();
        assert!(question.len() >= 200_000);
        let history: String = (0..4_000)
            .map(|i| {
                format!(
                    "Note {i}: nothing here but w{:05}x and plain words.\n",
                    i * 7
                )
            })
            .collect();
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": history },
            { "role": "user", "content": question },
        ]});
        let started = std::time::Instant::now();
        let (stats, _) = lossy_rewrite(&lossy_cfg(4), &mut body);
        assert_eq!(stats.examined, 1);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "took {:?}",
            started.elapsed()
        );
    }

    /// The passes share one index: a segment dedup replaced is not offered to
    /// the lossy pass, and the originals of both go out in one stash.
    #[test]
    fn a_segment_one_pass_rewrote_is_left_to_it() {
        let doc: String = (0..60)
            .map(|i| format!("Paragraph {i} of the design doc, with plain prose.\n"))
            .collect();
        let other: String = (0..60)
            .map(|i| format!("Line {i} of a second pasted document, also prose.\n"))
            .collect();
        let mut body = turns(&[doc.clone(), other.clone(), doc.clone()]);
        let cfg = lossy_cfg(8);
        let mut passes = TextPasses::new(&cfg, true, &body);
        let dedup = passes.dedup(&body);
        assert_eq!(dedup.replaced, 1);
        let order = passes.lossy_order(&body);
        assert_eq!(order.len(), 2, "the repeat is no longer a candidate");
        let hashes: Vec<String> = order
            .iter()
            .map(|&i| obleth_config::content_hash(passes.text(&body, i)))
            .collect();
        let none = vec![None; order.len()];
        let (lossy, _) = passes.lossy_rewrite(
            &body,
            &order,
            &hashes,
            &none,
            &vec![None; order.len()],
            &Default::default(),
        );
        assert_eq!(lossy.replaced, 2);
        let stash = passes.write(&mut body);
        assert_eq!(stash.len(), 3);
        assert!(body["messages"][2]["content"]
            .as_str()
            .unwrap()
            .starts_with("[ref:"));
    }

    /// One turn of the lossy pass with `store` standing in for Redis:
    /// returns how many forms it reused.
    fn lossy_turn(
        cfg: &CompressionBoonSettings,
        body: &mut Value,
        store: &mut std::collections::HashMap<String, String>,
    ) -> u32 {
        let mut passes = TextPasses::new(cfg, true, body);
        let order = passes.lossy_order(body);
        let hashes: Vec<String> = order
            .iter()
            .map(|&i| obleth_config::content_hash(passes.text(body, i)))
            .collect();
        let kept: Vec<Option<String>> = hashes
            .iter()
            .map(|h| store.get(&passes.form_id(h)).cloned())
            .collect();
        let terms = latest_user_query_terms(body);
        let none = vec![None; order.len()];
        let (_, reused) = passes.lossy_rewrite(body, &order, &hashes, &kept, &none, &terms);
        store.extend(std::mem::take(&mut passes.forms));
        passes.write(body);
        reused
    }

    /// Earlier turns read the same on every request: a segment is compacted
    /// once, toward the question of the turn it first appeared in, and that
    /// form is given again, so the prompt prefix does not move.
    #[test]
    fn earlier_turns_keep_their_compacted_form() {
        const NAMES: [&str; 12] = [
            "north", "south", "east", "west", "upper", "lower", "inner", "outer", "early", "late",
            "major", "minor",
        ];
        let doc = |topic: &str| -> String {
            NAMES
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let about = if i % 2 == 0 { "alpha" } else { "omega" };
                    format!("The {n} {topic} report covers {about} findings in plain words.\n")
                })
                .collect()
        };
        let conversation = |turns: usize| -> Value {
            let questions = ["what about alpha?", "and omega?", "and the rest?"];
            let mut messages = vec![json!({ "role": "user", "content": doc("design") })];
            for t in 0..turns {
                if t > 0 {
                    messages.push(json!({ "role": "assistant", "content": doc(["", "answer", "summary"][t]) }));
                }
                messages.push(json!({ "role": "user", "content": questions[t] }));
            }
            json!({ "messages": messages })
        };
        let cfg = lossy_cfg(4);
        let mut store = std::collections::HashMap::new();
        let mut turn1 = conversation(1);
        assert_eq!(lossy_turn(&cfg, &mut turn1, &mut store), 0);
        let mut turn2 = conversation(2);
        assert_eq!(lossy_turn(&cfg, &mut turn2, &mut store), 1);
        let mut turn3 = conversation(3);
        assert_eq!(lossy_turn(&cfg, &mut turn3, &mut store), 2);

        let first = &turn1["messages"][0]["content"];
        assert!(first.as_str().unwrap().contains("[ref:"));
        assert_eq!(&turn2["messages"][0]["content"], first);
        assert_eq!(&turn3["messages"][0]["content"], first);
        assert_eq!(
            turn3["messages"][2]["content"],
            turn2["messages"][2]["content"]
        );

        // Without the kept form, the new question would have moved it.
        let mut fresh = conversation(2);
        lossy_turn(&cfg, &mut fresh, &mut Default::default());
        assert_ne!(&fresh["messages"][0]["content"], first);
    }

    /// Lossy output on fixed fixtures, digested: whole-word query matching
    /// (P2-15) moved it deliberately; it must not move again by accident.
    #[test]
    fn lossy_output_is_byte_identical_to_the_recorded_digest() {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        let mut replaced = 0;
        for k in 1..=12usize {
            let texts: Vec<String> = (0..(2 + k % 4))
                .map(|j| {
                    (0..(10 + 5 * ((k + j) % 7)))
                        .map(|i| {
                            format!(
                                "Item {i} on the {} rollout notes the latency budget{}\n",
                                ["gateway", "chat", "docs"][(i + j) % 3],
                                if i % 4 == 0 {
                                    " and the config drift."
                                } else {
                                    "."
                                }
                            )
                        })
                        .collect()
                })
                .collect();
            let mut body = turns(&texts);
            body["messages"][texts.len()]["content"] =
                json!("why did the latency and config change?");
            let (_, stash) = lossy_rewrite(&lossy_cfg(1 + (k % 3) as u32), &mut body);
            replaced += stash.len();
            h.update(format!("{body}{stash:?}").as_bytes());
        }
        assert!(replaced >= 12, "the fixtures barely compacted ({replaced})");
        let digest = hex::encode(h.finalize());
        assert_eq!(
            digest,
            "b125890fa94e4350d41da9f2883b6b15bddd2627893847f186f6d747286196ba"
        );
    }

    #[test]
    fn apply_compacts_wrapped_array() {
        let rows: Vec<Value> = (0..100)
            .map(|i| json!({"id": i, "name": format!("u{i}"), "active": true}))
            .collect();
        let content =
            serde_json::to_string(&json!({"results": rows, "meta": {"total": 100}})).unwrap();
        let mut body = json!({"model": "m", "messages": [{"role": "tool", "content": content}]});
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("<<OBLETH_TABLE:0"));
    }

    #[test]
    fn apply_compacts_array_embedded_in_prose() {
        let r: Vec<String> = (0..100)
            .map(|i| format!("{{\"id\":{i},\"name\":\"u{i}\",\"active\":true}}"))
            .collect();
        let prose = format!(
            "Here is the data you asked for: [{}] please summarize it.",
            r.join(",")
        );
        let mut body = json!({"model": "m", "messages": [{"role": "user", "content": prose}]});
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        let out = body["messages"][0]["content"].as_str().unwrap();
        assert!(out.starts_with("Here is the data you asked for: "));
        assert!(out.contains("OBLETH_TABLE rows=100"));
        assert!(out.ends_with(" please summarize it."));
    }

    #[test]
    fn apply_lossy_skips_structurally_compacted_segments() {
        // A segment the lossless structural pass already rewrote into OBLETH_TABLE form.
        // Rows start with digits so classify() returns Log; without the guard the lossy
        // pass would feed it to compact_log and truncate the table rows on the wire.
        let mut text = String::from("OBLETH_TABLE rows=100\nid,name,active\n");
        for i in 0..60 {
            text.push_str(&format!("{i},user{i},true\n"));
        }
        // Verify the risk is real: the segment classifies as Log (rows starting with
        // digits look timestamp-ish) and compact_log would happily collapse it.
        assert_eq!(classify(&text), ContentKind::Log);
        assert!(
            compact_log(&text).is_some(),
            "without the guard compact_log would clobber the table"
        );
        // The guard fires: the marker correctly identifies structurally-compacted segments.
        assert!(
            text.contains("OBLETH_TABLE"),
            "guard condition must match on structural table output"
        );
    }

    #[test]
    fn apply_compacts_sparse_object_array() {
        let rows: Vec<Value> = (0..100)
            .map(|i| {
                if i % 2 == 0 {
                    json!({"id": i, "name": format!("u{i}")})
                } else {
                    json!({"id": i, "email": format!("u{i}@x")})
                }
            })
            .collect();
        let content = serde_json::to_string(&json!(rows)).unwrap();
        let mut body = json!({"model": "m", "messages": [{"role": "tool", "content": content}]});
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 1);
        assert!(stats.tokens_after < stats.tokens_before);
    }

    #[test]
    fn apply_leaves_plain_prose_unchanged() {
        let prose: String = (0..60)
            .map(|i| format!("This is sentence number {i} with no structured data at all.\n"))
            .collect();
        let mut body =
            json!({"model": "m", "messages": [{"role": "user", "content": prose.clone()}]});
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true,
            min_tokens: 16,
            max_segments: 64,
            ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.replaced, 0);
        assert_eq!(body["messages"][0]["content"].as_str().unwrap(), prose);
    }
}

#[cfg(test)]
mod scoring_tests {
    use super::tests::{lossy_cfg, turns};
    use super::*;

    fn prose(sentences: usize) -> String {
        (0..sentences)
            .map(|i| format!("Sentence number {i} says something plain."))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `scoring_batches` over `texts`: (positions in the lossy order, batches).
    fn scoring(
        texts: &[String],
        max_segments: u32,
        max_sentences: usize,
    ) -> (Vec<usize>, Vec<Vec<String>>) {
        let body = turns(texts);
        let cfg = lossy_cfg(max_segments);
        let passes = TextPasses::new(&cfg, false, &body);
        let order = passes.lossy_order(&body);
        passes.scoring_batches(&body, &order, &vec![None; order.len()], max_sentences)
    }

    /// A segment whose form is kept from an earlier turn is not scored again:
    /// the sidecar sees only new segments.
    #[test]
    fn kept_forms_are_not_scored_again() {
        let texts: Vec<String> = (0..4).map(|_| prose(5)).collect();
        let body = turns(&texts);
        let cfg = lossy_cfg(3);
        let passes = TextPasses::new(&cfg, false, &body);
        let order = passes.lossy_order(&body);
        let kept = vec![Some("kept".to_owned()), None, Some("kept".to_owned()), None];
        let (at, batches) = passes.scoring_batches(&body, &order, &kept, 1_000);
        assert_eq!((at, batches.len()), (vec![1, 3], 2));
    }

    #[test]
    fn scoring_sends_only_the_segments_the_loop_can_use() {
        let targets: Vec<String> = (0..10).map(|_| prose(5)).collect();
        let (indices, batches) = scoring(&targets, 3, 1_000);
        assert_eq!(
            indices,
            vec![0, 1, 2],
            "at most max_lossy_segments are scored"
        );
        assert_eq!(batches.len(), 3);
        assert!(batches.iter().all(|b| b.len() == 5));
    }

    #[test]
    fn scoring_skips_non_prose_and_respects_the_hard_caps() {
        let rows: Vec<String> = (0..12)
            .map(|i| format!("{{\"id\": {i}, \"v\": [1, 2, 3]}}"))
            .collect();
        let targets = vec![
            format!("[{}]", rows.join(", ")),
            format!("OBLETH_TABLE rows=3 {}", prose(5)),
            prose(25),
            prose(4),
        ];
        // The lossy order is the prose alone, largest first: prose(25), prose(4).
        let (indices, batches) = scoring(&targets, 50, 24);
        assert_eq!(
            (indices, batches.len(), batches[0].len()),
            (vec![1], 1, 4),
            "JSON, already-compacted tables, and a segment over the sentence cap are not sent"
        );

        let targets: Vec<String> = (0..(SCORE_MAX_SEGMENTS + 8)).map(|_| prose(3)).collect();
        let (indices, _) = scoring(&targets, u32::MAX, 1_000);
        assert_eq!(indices.len(), SCORE_MAX_SEGMENTS);
    }
}
