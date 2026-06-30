//! Extract and compact JSON spans embedded inside otherwise-prose segments.
//! Surrounding bytes are preserved; only the JSON spans are rewritten. Lossless
//! per span (each compacted span reconstructs to the same value) and fail-open.

use serde_json::Value;

use super::structural_json;

/// Find balanced JSON spans inside `text` and replace each with its compacted
/// form. Returns `Some(new_text)` when at least one span compacted to a strictly
/// shorter form; else `None`.
pub(super) fn extract(text: &str) -> Option<String> {
    let spans = find_spans(text);
    if spans.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    let mut changed = false;
    for (start, end) in spans {
        if start < last {
            continue; // overlap guard
        }
        let original = &text[start..end];
        if let Some(compacted) = structural_json::compact(original) {
            if compacted.len() < original.len() {
                out.push_str(&text[last..start]);
                out.push_str(&compacted);
                last = end;
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// Byte spans of balanced `{...}`/`[...]` that parse as JSON containing at least
/// one qualifying table array.
fn find_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'{' || b == b'[' {
            if let Some(end) = match_balanced(bytes, i) {
                if let Ok(v) = serde_json::from_str::<Value>(&text[i..end]) {
                    if contains_qualifying_array(&v) {
                        spans.push((i, end));
                        i = end;
                        continue;
                    }
                }
            }
        }
        i += utf8_len(b);
    }
    spans
}

/// Index one past the bracket that closes the one at `start`, or `None`.
/// String- and escape-aware; balances only the opener's own bracket kind (inner
/// brackets of the other kind are validated by the subsequent serde parse).
fn match_balanced(bytes: &[u8], start: usize) -> Option<usize> {
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn contains_qualifying_array(v: &Value) -> bool {
    match v {
        Value::Array(arr) => {
            structural_json::is_qualifying_array(arr) || arr.iter().any(contains_qualifying_array)
        }
        Value::Object(map) => map.values().any(contains_qualifying_array),
        _ => false,
    }
}

/// Byte length of the UTF-8 char beginning with lead byte `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(n: usize) -> String {
        let r: Vec<String> = (0..n)
            .map(|i| format!("{{\"id\":{i},\"name\":\"u{i}\",\"ok\":true}}"))
            .collect();
        format!("[{}]", r.join(","))
    }

    #[test]
    fn extracts_array_embedded_in_prose() {
        let text = format!("Here is the data: {} please summarize it.", rows(50));
        let out = extract(&text).expect("embedded array compacts");
        assert!(out.starts_with("Here is the data: "));
        assert!(out.contains("OBLETH_TABLE rows=50"));
        assert!(out.ends_with(" please summarize it."));
        assert!(out.len() < text.len());
    }

    #[test]
    fn extracts_from_json_fence() {
        let text = format!("```json\n{}\n```", rows(50));
        let out = extract(&text).expect("fenced array compacts");
        assert!(out.starts_with("```json\n"));
        assert!(out.contains("OBLETH_TABLE rows=50"));
        assert!(out.trim_end().ends_with("```"));
    }

    #[test]
    fn ignores_brackets_inside_strings() {
        // A JSON string containing a bracket must not break span matching.
        let r: Vec<String> = (0..40)
            .map(|i| format!("{{\"id\":{i},\"s\":\"a]b}}c\"}}"))
            .collect();
        let text = format!("data: [{}] end", r.join(","));
        let out = extract(&text).expect("compacts despite brackets in strings");
        assert!(out.contains("OBLETH_TABLE rows=40"));
        assert!(out.ends_with(" end"));
    }

    #[test]
    fn returns_none_when_no_qualifying_json() {
        assert_eq!(extract("just prose, no data here at all."), None);
        // A small/non-tabular object is not worth extracting.
        assert_eq!(extract("config is {\"a\": 1} today"), None);
    }
}
