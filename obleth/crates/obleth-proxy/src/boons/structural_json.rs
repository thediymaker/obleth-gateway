//! Structural JSON compaction (compression redesign, slice 1).
//!
//! Turns an array of like-keyed objects into a marker + CSV header + one CSV
//! row per object (each cell is the value's compact JSON, RFC-4180-escaped).
//! Substituted only when it reconstructs to the exact original `Value` and is
//! strictly shorter; otherwise falls back to lossless minify. Fail-open.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

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
const BLOCK_PREFIX: &str = "<<OBLETH_TABLE:";

/// Encode an array of >=2 non-empty objects as `header\nrow\nrow...` using the
/// UNION of keys (sorted). A key absent from an object yields an empty cell; a
/// present value (including JSON null) yields its compact JSON, CSV-escaped.
/// Returns `(row_count, body)` or `None` when the array doesn't qualify.
fn encode_table_body(arr: &[Value]) -> Option<(usize, String)> {
    if arr.len() < 2 {
        return None;
    }
    let mut col_set: BTreeSet<&str> = BTreeSet::new();
    for item in arr {
        let obj = item.as_object()?;
        if obj.is_empty() {
            return None;
        }
        for k in obj.keys() {
            if k.contains(['\n', '\r']) {
                return None; // column names can't contain newlines
            }
            col_set.insert(k.as_str());
        }
    }
    let cols: Vec<&str> = col_set.into_iter().collect();
    let mut out = String::new();
    let header: Vec<String> = cols.iter().map(|k| csv_escape(k)).collect();
    out.push_str(&header.join(","));
    out.push('\n');
    for item in arr {
        let obj = item.as_object().expect("checked above");
        let row: Vec<String> = cols
            .iter()
            .map(|k| match obj.get(*k) {
                Some(v) => csv_escape(&serde_json::to_string(v).unwrap_or_default()),
                None => String::new(), // absent key -> empty cell
            })
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    Some((arr.len(), out))
}

/// Parse a `header\nrows...` table body (no marker line) into `n` objects. An
/// empty field means the key is ABSENT; a non-empty field is the cell's JSON.
fn parse_table_body(body: &str, n: usize) -> Option<Vec<Value>> {
    let mut lines = body.lines();
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
            if field.is_empty() {
                continue; // absent key
            }
            let cell: Value = serde_json::from_str(field).ok()?;
            obj.insert(col.clone(), cell);
        }
        rows.push(Value::Object(obj));
    }
    if rows.len() != n {
        return None;
    }
    Some(rows)
}

/// Whether `arr` qualifies as a table (>=2 non-empty objects, encodable).
pub(super) fn is_qualifying_array(arr: &[Value]) -> bool {
    encode_table_body(arr).is_some()
}

/// Encode an array as the top-level table form (`OBLETH_TABLE rows=N` + body).
fn try_encode_table(value: &Value) -> Option<String> {
    let arr = value.as_array()?;
    let (n, body) = encode_table_body(arr)?;
    let mut out = String::with_capacity(body.len() + TABLE_MARKER.len() + 8);
    out.push_str(TABLE_MARKER);
    out.push_str(&n.to_string());
    out.push('\n');
    out.push_str(&body);
    Some(out)
}

/// Walk `value`, replacing every qualifying array with a placeholder string
/// `<<OBLETH_TABLE:N>>` and pushing its `(row_count, body)` to `blocks`.
fn collect_tables(value: &mut Value, blocks: &mut Vec<(usize, String)>) {
    match value {
        Value::Array(arr) => {
            if let Some((n, body)) = encode_table_body(arr) {
                let idx = blocks.len();
                blocks.push((n, body));
                *value = Value::String(format!("{BLOCK_PREFIX}{idx}>>"));
            } else {
                for item in arr.iter_mut() {
                    collect_tables(item, blocks);
                }
            }
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                collect_tables(v, blocks);
            }
        }
        _ => {}
    }
}

/// Compact a JSON segment whose qualifying arrays are nested (not the whole
/// segment), by emitting a valid-JSON skeleton with `<<OBLETH_TABLE:N>>`
/// placeholders followed by the appended table blocks.
pub(super) fn compact_recursive(text: &str) -> Option<String> {
    let original: Value = serde_json::from_str(text.trim()).ok()?;
    let mut skeleton = original.clone();
    let mut blocks: Vec<(usize, String)> = Vec::new();
    collect_tables(&mut skeleton, &mut blocks);
    if blocks.is_empty() {
        return None;
    }
    let mut out = serde_json::to_string(&skeleton).ok()?;
    out.push('\n');
    for (idx, (n, body)) in blocks.iter().enumerate() {
        out.push('\n');
        out.push_str(BLOCK_PREFIX);
        out.push_str(&format!("{idx} rows={n}>>\n"));
        out.push_str(body);
    }
    if out.len() >= text.len() {
        return None;
    }
    if reconstruct_blocks(&out).as_ref() != Some(&original) {
        return None;
    }
    Some(out)
}

/// Reconstruct the original `Value` from a skeleton + appended-blocks document.
fn reconstruct_blocks(text: &str) -> Option<Value> {
    let first_nl = text.find('\n')?;
    let mut skeleton: Value = serde_json::from_str(text[..first_nl].trim()).ok()?;
    let rest = &text[first_nl + 1..];

    let mut tables: Vec<Vec<Value>> = Vec::new();
    let mut pending_rows: Option<usize> = None;
    let mut body = String::new();
    for line in rest.lines() {
        if let Some(after) = line.strip_prefix(BLOCK_PREFIX) {
            if let Some(n) = pending_rows.take() {
                tables.push(parse_table_body(&body, n)?);
                body.clear();
            }
            let inner = after.strip_suffix(">>")?;
            let (idx_str, rows_str) = inner.split_once(" rows=")?;
            if idx_str.trim().parse::<usize>().ok()? != tables.len() {
                return None; // blocks must appear in order 0,1,2,...
            }
            pending_rows = Some(rows_str.trim().parse().ok()?);
        } else if pending_rows.is_some() {
            body.push_str(line);
            body.push('\n');
        }
        // lines before the first block header (the blank separator) are ignored
    }
    if let Some(n) = pending_rows.take() {
        tables.push(parse_table_body(&body, n)?);
    }
    splice_placeholders(&mut skeleton, &tables)?;
    Some(skeleton)
}

/// Test-only inverse of [`compact`]: decode either structural form — the
/// top-level `OBLETH_TABLE rows=` table OR the skeleton+appended-blocks document
/// — back to the original `Value`, so verification harnesses can prove that a
/// compacted segment round-trips exactly (losslessness, demonstrated not assumed).
#[cfg(test)]
pub(super) fn decode_for_verification(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if trimmed.starts_with(TABLE_MARKER) {
        reconstruct_table(trimmed)
    } else {
        reconstruct_blocks(trimmed)
    }
}

/// Replace every `<<OBLETH_TABLE:N>>` string value with reconstructed block N.
fn splice_placeholders(value: &mut Value, tables: &[Vec<Value>]) -> Option<()> {
    match value {
        Value::String(s) => {
            if let Some(idx) = s
                .strip_prefix(BLOCK_PREFIX)
                .and_then(|x| x.strip_suffix(">>"))
                .and_then(|x| x.trim().parse::<usize>().ok())
            {
                let arr = tables.get(idx)?;
                *value = Value::Array(arr.clone());
            }
            Some(())
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                splice_placeholders(v, tables)?;
            }
            Some(())
        }
        Value::Object(map) => {
            for (_k, v) in map.iter_mut() {
                splice_placeholders(v, tables)?;
            }
            Some(())
        }
        _ => Some(()),
    }
}

/// Compact a JSON text segment. Top-level qualifying arrays keep their exact
/// current `OBLETH_TABLE` form; otherwise nested/wrapped/multiple arrays use the
/// skeleton+blocks form; otherwise lossless minify; otherwise `None`.
pub(super) fn compact(text: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(text.trim()) {
        if value.is_array() {
            if let Some(table) = try_encode_table(&value) {
                if table.len() < text.len() && reconstruct_table(&table).as_ref() == Some(&value) {
                    return Some(table);
                }
            }
        }
        if let Some(rec) = compact_recursive(text) {
            return Some(rec);
        }
    }
    super::compression::compact_json(text)
}

/// Reconstruct the JSON array from the top-level table form.
fn reconstruct_table(text: &str) -> Option<Value> {
    let nl = text.find('\n')?;
    let n: usize = text[..nl].strip_prefix(TABLE_MARKER)?.trim().parse().ok()?;
    Some(Value::Array(parse_table_body(&text[nl + 1..], n)?))
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
    fn compact_handles_compact_pasted_array() {
        // Exactly what a user pastes: a COMPACT (single-line) array of uniform
        // objects. Must table-ify and shrink.
        let rows: Vec<String> = (0..80)
            .map(|i| format!("{{\"id\":{i},\"name\":\"user{i}\",\"email\":\"u{i}@x.com\",\"role\":\"member\",\"active\":true,\"score\":{}}}", i * 3))
            .collect();
        let arr = format!("[{}]", rows.join(","));
        let out = compact(&arr).expect("compact pasted array should table-ify");
        assert!(out.starts_with("OBLETH_TABLE rows=80\n"), "got: {}", &out[..out.len().min(40)]);
        assert!(out.len() < arr.len());
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
    fn encode_supports_non_uniform_keys() {
        let value = json!([{"a": 1}, {"b": 2}]);
        let table = try_encode_table(&value).expect("now encodes via union keys");
        assert_eq!(reconstruct_table(&table), Some(value));
    }

    #[test]
    fn encode_table_body_handles_sparse_keys() {
        // Objects with differing key sets -> union columns, empty cell for absent.
        let value = json!([
            {"id": 1, "name": "alice"},
            {"id": 2, "email": "b@x"}
        ]);
        let arr = value.as_array().unwrap();
        let (n, body) = encode_table_body(arr).expect("sparse encodes");
        assert_eq!(n, 2);
        // Union columns sorted: email,id,name
        assert!(body.starts_with("email,id,name\n"), "body was: {body:?}");
        // Round-trips exactly, distinguishing absent from present.
        assert_eq!(parse_table_body(&body, n), Some(value.as_array().unwrap().clone()));
    }

    #[test]
    fn parse_table_body_distinguishes_absent_from_null() {
        // present null vs absent key must round-trip differently.
        let value = json!([
            {"a": null, "b": 1},
            {"b": 2}
        ]);
        let arr = value.as_array().unwrap();
        let (n, body) = encode_table_body(arr).expect("encodes");
        let back = parse_table_body(&body, n).expect("parses");
        assert_eq!(back, *arr);
        // First object HAS key "a" (null); second does NOT.
        assert!(back[0].as_object().unwrap().contains_key("a"));
        assert!(!back[1].as_object().unwrap().contains_key("a"));
    }

    #[test]
    fn is_qualifying_array_rejects_non_objects_and_singletons() {
        assert!(!is_qualifying_array(json!([1, 2, 3]).as_array().unwrap()));
        assert!(!is_qualifying_array(json!([{"a": 1}]).as_array().unwrap())); // <2
        assert!(is_qualifying_array(json!([{"a": 1}, {"b": 2}]).as_array().unwrap()));
    }

    #[test]
    fn encode_rejects_non_object_array_and_singletons() {
        assert_eq!(try_encode_table(&json!([1, 2, 3])), None);
        assert_eq!(try_encode_table(&json!([{"a": 1}])), None); // <2 rows
        assert_eq!(try_encode_table(&json!({"a": 1})), None); // not an array
    }

    #[test]
    fn compact_uses_table_for_object_arrays() {
        // Pretty-printed array of like objects → table form, exact round-trip, shorter.
        let value = json!([
            {"id": 1, "name": "alice", "role": "admin", "active": true},
            {"id": 2, "name": "bob", "role": "user", "active": false}
        ]);
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let out = compact(&pretty).expect("should compact");
        assert!(out.starts_with("OBLETH_TABLE rows=2\n"));
        assert!(out.len() < pretty.len());
        assert_eq!(reconstruct_table(&out), Some(value));
    }

    #[test]
    fn compact_falls_back_to_minify_for_non_tabular() {
        // A single object isn't tabular → falls back to lossless minify.
        let pretty = "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}";
        let out = compact(pretty).expect("should minify");
        assert!(!out.starts_with("OBLETH_TABLE"));
        let a: Value = serde_json::from_str(pretty).unwrap();
        let b: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(a, b);
        assert!(out.len() < pretty.len());
    }

    #[test]
    fn compact_returns_none_when_no_gain() {
        assert_eq!(compact("{\"a\":1}"), None); // already minimal, not tabular
    }

    #[test]
    fn compact_recursive_tabbifies_wrapped_array() {
        let rows: Vec<Value> = (0..50).map(|i| json!({"id": i, "name": format!("u{i}"), "ok": true})).collect();
        let value = json!({"results": rows, "meta": {"total": 50}});
        let text = serde_json::to_string(&value).unwrap();
        let out = compact_recursive(&text).expect("wrapped array compacts");
        assert!(out.len() < text.len());
        // Skeleton holds a placeholder; a block carries the rows.
        assert!(out.contains("\"<<OBLETH_TABLE:0>>\""), "out: {out}");
        assert!(out.contains("<<OBLETH_TABLE:0 rows=50>>"));
        // Lossless round-trip through compact() entry as well.
        assert_eq!(reconstruct_blocks(&out), Some(value));
    }

    #[test]
    fn compact_recursive_handles_multiple_arrays() {
        let a: Vec<Value> = (0..20).map(|i| json!({"x": i, "y": i * 2})).collect();
        let b: Vec<Value> = (0..20).map(|i| json!({"k": format!("k{i}"), "v": i})).collect();
        let value = json!({"first": a, "second": b});
        let text = serde_json::to_string(&value).unwrap();
        let out = compact_recursive(&text).expect("two arrays compact");
        assert!(out.contains("<<OBLETH_TABLE:0 rows=20>>"));
        assert!(out.contains("<<OBLETH_TABLE:1 rows=20>>"));
        assert_eq!(reconstruct_blocks(&out), Some(value));
    }

    #[test]
    fn compact_recursive_collision_is_safe() {
        // Data legitimately containing a placeholder-looking string + a real array.
        let rows: Vec<Value> = (0..40).map(|i| json!({"id": i, "v": i})).collect();
        let value = json!({"note": "<<OBLETH_TABLE:0>>", "rows": rows});
        let text = serde_json::to_string(&value).unwrap();
        // Either it returns None (fell back) or it returns a form that round-trips
        // to the EXACT original — never corruption.
        if let Some(out) = compact_recursive(&text) {
            assert_eq!(reconstruct_blocks(&out), Some(value));
        }
    }

    #[test]
    fn compact_keeps_top_level_array_output_byte_identical() {
        // The common case must not gain a skeleton wrapper.
        let value = json!([
            {"id": 1, "name": "alice", "role": "admin"},
            {"id": 2, "name": "bob", "role": "user"}
        ]);
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let out = compact(&pretty).expect("top-level array compacts");
        assert!(out.starts_with("OBLETH_TABLE rows=2\n"));
        assert!(!out.contains("<<OBLETH_TABLE:"));
    }
}
