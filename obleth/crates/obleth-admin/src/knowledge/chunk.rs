//! Splitting uploaded documents into embeddable chunks.
//!
//! Chunking is pure and deterministic: the same bytes and parameters always
//! produce the same chunks, which is what makes re-indexing predictable and
//! content-hash dedupe meaningful.

use obleth_tokenizer::Tokenizer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    pub token_count: u32,
}

/// Split `content` into chunks of roughly `target_tokens`, repeating
/// `overlap_tokens` worth of trailing content at the head of the next chunk.
///
/// CSV is split on record boundaries with the header repeated in every chunk;
/// everything else is split on blank-line paragraphs, then on lines when a
/// paragraph alone exceeds the target.
pub fn chunk_text(
    content: &str,
    content_type: &str,
    target_tokens: u32,
    overlap_tokens: u32,
    tk: &dyn Tokenizer,
) -> Vec<Chunk> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    if is_csv(content_type) {
        return chunk_csv(content, target_tokens, tk);
    }
    chunk_prose(content, target_tokens, overlap_tokens, tk)
}

fn is_csv(content_type: &str) -> bool {
    content_type.eq_ignore_ascii_case("text/csv")
        || content_type.eq_ignore_ascii_case("application/csv")
}

/// Units are paragraphs; a paragraph past the target is broken into lines so a
/// single huge block can't produce one unbounded chunk.
fn split_units(content: &str, target_tokens: u32, tk: &dyn Tokenizer) -> Vec<String> {
    let mut units = Vec::new();
    for para in content.split("\n\n") {
        if para.trim().is_empty() {
            continue;
        }
        if tk.count_text(para) <= target_tokens {
            units.push(para.to_string());
        } else {
            for line in para.lines() {
                if !line.trim().is_empty() {
                    units.push(line.to_string());
                }
            }
        }
    }
    units
}

fn chunk_prose(
    content: &str,
    target_tokens: u32,
    overlap_tokens: u32,
    tk: &dyn Tokenizer,
) -> Vec<Chunk> {
    let units = split_units(content, target_tokens, tk);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_tokens = 0u32;

    for unit in units {
        let unit_tokens = tk.count_text(&unit);
        if current_tokens + unit_tokens > target_tokens && !current.is_empty() {
            chunks.push(finish(&current, tk));
            current = overlap_tail(&current, overlap_tokens, tk);
            current_tokens = current.iter().map(|u| tk.count_text(u)).sum();
        }
        current_tokens += unit_tokens;
        current.push(unit);
    }
    if !current.is_empty() {
        chunks.push(finish(&current, tk));
    }
    chunks
}

fn finish(units: &[String], tk: &dyn Tokenizer) -> Chunk {
    let text = units.join("\n\n");
    let token_count = tk.count_text(&text);
    Chunk { text, token_count }
}

/// Trailing units totalling at most `overlap_tokens`, carried into the next
/// chunk so a fact split across a boundary survives in one piece somewhere.
fn overlap_tail(units: &[String], overlap_tokens: u32, tk: &dyn Tokenizer) -> Vec<String> {
    if overlap_tokens == 0 {
        return Vec::new();
    }
    let mut tail = Vec::new();
    let mut total = 0u32;
    for unit in units.iter().rev() {
        let t = tk.count_text(unit);
        if total + t > overlap_tokens {
            break;
        }
        total += t;
        tail.push(unit.clone());
    }
    tail.reverse();
    tail
}

/// CSV: the first line is the header and is repeated in every chunk, because a
/// chunk of bare values with no column names retrieves and reads as noise.
fn chunk_csv(content: &str, target_tokens: u32, tk: &dyn Tokenizer) -> Vec<Chunk> {
    let mut lines = content.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let header_tokens = tk.count_text(header);
    let mut chunks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_tokens = header_tokens;

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let t = tk.count_text(line);
        if current_tokens + t > target_tokens && !current.is_empty() {
            chunks.push(finish_csv(header, &current, tk));
            current.clear();
            current_tokens = header_tokens;
        }
        current_tokens += t;
        current.push(line);
    }
    if !current.is_empty() {
        chunks.push(finish_csv(header, &current, tk));
    }
    chunks
}

fn finish_csv(header: &str, rows: &[&str], tk: &dyn Tokenizer) -> Chunk {
    let mut text = String::from(header);
    for r in rows {
        text.push('\n');
        text.push_str(r);
    }
    let token_count = tk.count_text(&text);
    Chunk { text, token_count }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_tokenizer::HeuristicTokenizer;

    fn tk() -> HeuristicTokenizer {
        HeuristicTokenizer::new()
    }

    #[test]
    fn short_document_is_one_chunk() {
        let chunks = chunk_text("A short policy note.", "text/markdown", 400, 50, &tk());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "A short policy note.");
    }

    #[test]
    fn empty_document_yields_no_chunks() {
        assert!(chunk_text("   \n\n  ", "text/markdown", 400, 50, &tk()).is_empty());
    }

    #[test]
    fn long_document_splits_and_overlaps() {
        // 200 paragraphs of ~10 tokens each, far past a 50-token target.
        let body = (0..200)
            .map(|i| format!("Paragraph {i} contains some words about policy."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = chunk_text(&body, "text/markdown", 50, 10, &tk());
        assert!(chunks.len() > 1, "must split");
        // Overlap means consecutive chunks share trailing/leading content, so
        // a fact sitting on a boundary is retrievable from at least one chunk.
        let first_tail = chunks[0].text.split_whitespace().last().unwrap();
        assert!(
            chunks[1].text.contains(first_tail),
            "chunk 2 must repeat the tail of chunk 1"
        );
    }

    #[test]
    fn no_chunk_greatly_exceeds_the_target() {
        let body = (0..100)
            .map(|i| format!("Line {i} of the manual."))
            .collect::<Vec<_>>()
            .join("\n");
        for c in chunk_text(&body, "text/plain", 40, 5, &tk()) {
            assert!(c.token_count <= 80, "got {} tokens", c.token_count);
        }
    }

    #[test]
    fn csv_splits_on_record_boundaries_and_repeats_the_header() {
        let csv = "code,title,credits\n".to_string()
            + &(0..100)
                .map(|i| format!("CS{i},Course {i},3"))
                .collect::<Vec<_>>()
                .join("\n");
        let chunks = chunk_text(&csv, "text/csv", 40, 0, &tk());
        assert!(chunks.len() > 1);
        for c in &chunks {
            // A chunk cut mid-record retrieves badly and reads as corrupt, and
            // without the header the columns are unlabeled.
            assert!(c.text.starts_with("code,title,credits"));
            assert!(!c.text.ends_with(','));
        }
    }

    #[test]
    fn single_oversized_line_is_still_emitted() {
        // One line longer than the target must not be dropped or loop forever.
        let body = "word ".repeat(500);
        let chunks = chunk_text(&body, "text/plain", 20, 5, &tk());
        assert!(!chunks.is_empty());
    }
}
