//! Structural JSON compaction (compression redesign, slice 1).
//!
//! Turns an array of like-keyed objects into a marker + CSV header + one CSV
//! row per object (each cell is the value's compact JSON, RFC-4180-escaped).
//! Substituted only when it reconstructs to the exact original `Value` and is
//! strictly shorter; otherwise falls back to lossless minify. Fail-open.

use serde_json::{Map, Value};

/// RFC-4180 field escape: quote the field iff it contains a comma, CR, or
/// LF; double any inner quotes.
fn csv_escape(field: &str) -> String {
    if field.contains([',', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Parse one CSV physical line into fields, honoring RFC-4180 quoting (a quoted
/// field may contain commas; `""` is an escaped quote).
fn csv_parse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    let mut in_quotes = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else {
            match c {
                '"' => in_quotes = true,
                ',' => fields.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_leaves_plain_fields() {
        assert_eq!(csv_escape("42"), "42");
        assert_eq!(csv_escape("true"), "true");
        assert_eq!(csv_escape("\"alice\""), "\"alice\""); // a JSON string cell, no comma → unchanged
    }

    #[test]
    fn csv_escape_quotes_and_doubles() {
        // A JSON string value containing a comma: serde gives `"a,b"`, which must be CSV-quoted.
        assert_eq!(csv_escape("\"a,b\""), "\"\"\"a,b\"\"\"");
    }

    #[test]
    fn csv_parse_roundtrips_quoted_commas() {
        let fields = csv_parse_line("1,\"\"\"a,b\"\"\",true");
        assert_eq!(fields, vec!["1".to_string(), "\"a,b\"".to_string(), "true".to_string()]);
    }

    #[test]
    fn csv_parse_plain_line() {
        assert_eq!(csv_parse_line("1,alice,true"), vec!["1", "alice", "true"]);
    }
}
