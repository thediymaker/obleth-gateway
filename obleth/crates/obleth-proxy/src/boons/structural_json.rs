//! Structural JSON compaction (compression redesign, slice 1).
//!
//! Turns an array of like-keyed objects into a marker + CSV header + one CSV
//! row per object (each cell is the value's compact JSON, RFC-4180-escaped).
//! Substituted only when it reconstructs to the exact original `Value` and is
//! strictly shorter; otherwise falls back to lossless minify. Fail-open.

use serde_json::{Map, Value};

/// RFC-4180 field escape: quote the field iff it contains a comma, quote, CR, or
/// LF; double any inner quotes.
fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
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

const TABLE_MARKER: &str = "OBLETH_TABLE rows=";

/// Encode an array of ≥2 like-keyed objects as the table form, or `None` when it
/// doesn't qualify. Does NOT check length or validate — the caller does.
fn try_encode_table(value: &Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    // All elements must be objects with an identical key SET.
    let first = arr[0].as_object()?;
    if first.is_empty() {
        return None;
    }
    let cols: Vec<&String> = first.keys().collect();
    // Column names can't contain literal newlines (would break line-splitting).
    if cols.iter().any(|k| k.contains(['\n', '\r'])) {
        return None;
    }
    for item in arr {
        let obj = item.as_object()?;
        if obj.len() != cols.len() || !cols.iter().all(|k| obj.contains_key(*k)) {
            return None;
        }
    }

    let mut out = String::new();
    out.push_str(TABLE_MARKER);
    out.push_str(&arr.len().to_string());
    out.push('\n');
    // Header: raw column names, CSV-escaped.
    let header: Vec<String> = cols.iter().map(|k| csv_escape(k)).collect();
    out.push_str(&header.join(","));
    out.push('\n');
    // Rows: each cell is the value's compact JSON, CSV-escaped.
    for item in arr {
        let obj = item.as_object().expect("checked above");
        let row: Vec<String> = cols
            .iter()
            .map(|k| csv_escape(&serde_json::to_string(&obj[*k]).unwrap_or_default()))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    Some(out)
}

/// Reconstruct the JSON array from the table form, or `None` if `text` isn't a
/// well-formed table this module produced.
fn reconstruct_table(text: &str) -> Option<Value> {
    let mut lines = text.lines();
    let marker = lines.next()?;
    let n: usize = marker.strip_prefix(TABLE_MARKER)?.trim().parse().ok()?;
    let cols = csv_parse_line(lines.next()?);
    if cols.is_empty() {
        return None;
    }
    let mut rows: Vec<Value> = Vec::with_capacity(n);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let fields = csv_parse_line(line);
        if fields.len() != cols.len() {
            return None;
        }
        let mut obj = Map::new();
        for (col, field) in cols.iter().zip(fields.iter()) {
            let cell: Value = serde_json::from_str(field).ok()?;
            obj.insert(col.clone(), cell);
        }
        rows.push(Value::Object(obj));
    }
    if rows.len() != n {
        return None;
    }
    Some(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn csv_escape_leaves_plain_fields() {
        assert_eq!(csv_escape("42"), "42");
        assert_eq!(csv_escape("true"), "true");
        assert_eq!(csv_escape("null"), "null");
    }

    #[test]
    fn csv_escape_quotes_fields_with_quotes_or_commas() {
        // A JSON string cell like `"alice"` MUST be CSV-quoted so it round-trips.
        assert_eq!(csv_escape("\"alice\""), "\"\"\"alice\"\"\"");
        // A JSON string with a comma, too.
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

    #[test]
    fn csv_escape_parse_roundtrips_json_cells() {
        let cells = ["1", "\"alice\"", "\"a,b\"", "true", "null"];
        let line = cells.iter().map(|c| csv_escape(c)).collect::<Vec<_>>().join(",");
        let parsed = csv_parse_line(&line);
        assert_eq!(parsed, cells.iter().map(|c| c.to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn encode_then_reconstruct_is_exact() {
        let value = json!([
            {"id": 1, "name": "alice", "active": true},
            {"id": 2, "name": "bob", "active": false},
            {"id": 3, "name": "c,d", "active": true}
        ]);
        let table = try_encode_table(&value).expect("should encode");
        // Header line names the columns; marker carries the row count.
        assert!(table.starts_with("OBLETH_TABLE rows=3\n"));
        // Columns are sorted (BTreeMap order), so active,id,name
        assert!(table.contains("active,id,name"));
        // Round-trips to the exact original Value.
        assert_eq!(reconstruct_table(&table), Some(value));
    }

    #[test]
    fn encode_handles_nested_and_null_cells() {
        let value = json!([
            {"k": {"x": 1, "y": 2}, "n": null},
            {"k": {"x": 3, "y": 4}, "n": 5}
        ]);
        let table = try_encode_table(&value).expect("encode");
        assert_eq!(reconstruct_table(&table), Some(value));
    }

    #[test]
    fn encode_rejects_non_uniform_keys() {
        let value = json!([{"a": 1}, {"b": 2}]);
        assert_eq!(try_encode_table(&value), None);
    }

    #[test]
    fn encode_rejects_non_object_array_and_singletons() {
        assert_eq!(try_encode_table(&json!([1, 2, 3])), None);
        assert_eq!(try_encode_table(&json!([{"a": 1}])), None); // <2 rows
        assert_eq!(try_encode_table(&json!({"a": 1})), None); // not an array
    }
}
