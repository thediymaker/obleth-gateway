//! Multipart request bodies: parsing the fields, the text view boons and
//! guardrails read, and rebuilding the form for the upstream.

use super::*;

/// True when the endpoint takes a file upload, so the OpenAI spec sends it as
/// `multipart/form-data` with the model as a form field rather than JSON:
/// audio transcription/translation, the two image endpoints that take a
/// source image (edits and variations), the video create, whose optional
/// reference frame is an upload, and the mesh create, whose source image is
/// (both accept JSON too).
pub(super) fn is_multipart_endpoint(path: &str) -> bool {
    matches!(
        path,
        "/v1/audio/transcriptions"
            | "/v1/audio/translations"
            | "/v1/images/edits"
            | "/v1/images/variations"
            | crate::jobs::VIDEOS_PATH
            | crate::jobs::MESHES_PATH
    )
}

/// A single parsed `multipart/form-data` field, held in memory so it can be
/// rebuilt into an upstream form after model resolution.
pub(super) struct MultipartField {
    pub(super) name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    pub(super) data: Bytes,
}

/// Parse an in-memory multipart body into its fields. The whole body is already
/// buffered (bounded by `BODY_LIMIT`), so this just re-reads it through `multer`.
pub(super) async fn parse_multipart(
    body: &Bytes,
    boundary: &str,
) -> Result<Vec<MultipartField>, multer::Error> {
    let bytes = body.clone();
    let stream =
        futures_util::stream::once(async move { Ok::<Bytes, std::convert::Infallible>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary.to_string());
    let mut fields = Vec::new();
    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|m| m.to_string());
        let data = field.bytes().await?;
        fields.push(MultipartField {
            name,
            file_name,
            content_type,
            data,
        });
    }
    Ok(fields)
}

/// The form's text fields as a JSON object, so the stages that read the
/// request body (the token estimate, the input guardrails, the per-image
/// cost) see a multipart request the way they see a JSON one. File parts are
/// left out. Values stay strings, as the form sent them; a name that repeats
/// becomes an array, so every occurrence is scanned.
pub(super) fn multipart_text_view(fields: &[MultipartField]) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for f in fields.iter().filter(|f| f.file_name.is_none()) {
        let text = serde_json::Value::String(String::from_utf8_lossy(&f.data).into_owned());
        match obj.get_mut(&f.name) {
            None => {
                obj.insert(f.name.clone(), text);
            }
            Some(serde_json::Value::Array(items)) => items.push(text),
            Some(first) => *first = serde_json::Value::Array(vec![first.take(), text]),
        }
    }
    serde_json::Value::Object(obj)
}

/// Copy text fields rewritten in the JSON view (a redacting input policy)
/// back into the form, so what is forwarded is what was scanned. The inverse
/// of [`multipart_text_view`]: a repeated name maps onto its array by position.
pub(super) fn apply_multipart_text_view(fields: &mut [MultipartField], view: &serde_json::Value) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in fields.iter_mut().filter(|f| f.file_name.is_none()) {
        let index = seen.entry(f.name.clone()).or_insert(0);
        let value = match view.get(&f.name) {
            Some(serde_json::Value::Array(items)) => items.get(*index),
            other if *index == 0 => other,
            _ => None,
        };
        *index += 1;
        if let Some(text) = value.and_then(|v| v.as_str()) {
            if text.as_bytes() != f.data.as_ref() {
                f.data = Bytes::from(text.to_string());
            }
        }
    }
}

/// Rebuild a reqwest multipart form from parsed fields, replacing the client
/// `model` value with the upstream model name. File parts preserve their
/// filename and content-type.
pub(super) fn build_multipart_form(
    fields: Vec<MultipartField>,
    upstream_model: &str,
) -> reqwest::multipart::Form {
    let mut form = reqwest::multipart::Form::new();
    let mut had_model = false;
    for f in fields {
        if f.name == "model" {
            had_model = true;
            form = form.text("model", upstream_model.to_string());
            continue;
        }
        if f.file_name.is_some() {
            let file_name = f.file_name.unwrap_or_default();
            let data = f.data.to_vec();
            let part = match f.content_type {
                // content_type came from a parsed Mime, so mime_str won't fail.
                Some(ct) => reqwest::multipart::Part::bytes(data)
                    .file_name(file_name)
                    .mime_str(&ct)
                    .unwrap_or_else(|_| reqwest::multipart::Part::text("")),
                None => reqwest::multipart::Part::bytes(data).file_name(file_name),
            };
            form = form.part(f.name, part);
        } else {
            form = form.text(f.name, String::from_utf8_lossy(&f.data).into_owned());
        }
    }
    if !had_model {
        form = form.text("model", upstream_model.to_string());
    }
    form
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::tests::minimal_model;

    /// Endpoints whose OpenAI spec sends `multipart/form-data` (they take a
    /// file upload). The video create's upload is its optional
    /// `input_reference` frame; it also accepts JSON.
    const SPEC_MULTIPART_PATHS: &[&str] = &[
        "/v1/audio/transcriptions",
        "/v1/audio/translations",
        "/v1/images/edits",
        "/v1/images/variations",
        "/v1/videos",
        "/v1/meshes",
    ];

    fn image_edit_form() -> (String, Bytes) {
        let boundary = "XBOUNDARYX".to_string();
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nimage-model\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nadd a hat\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n3\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"cat.png\"\r\n\
             Content-Type: image/png\r\n\r\nPNGDATA\r\n\
             --{b}--\r\n",
            b = boundary
        );
        (boundary, Bytes::from(body))
    }

    #[test]
    fn every_registered_multipart_endpoint_parses_its_form() {
        // The two lists drifted once: the image upload endpoints required a
        // model but their form was never parsed, so the model field was never
        // seen and every request failed "model is required".
        for path in REGISTERED_MODEL_PATHS {
            assert_eq!(
                is_multipart_endpoint(path),
                SPEC_MULTIPART_PATHS.contains(path),
                "{path}"
            );
        }
        for path in SPEC_MULTIPART_PATHS {
            assert!(requires_registered_model(path), "{path}");
        }
    }

    #[tokio::test]
    async fn an_image_edit_form_reads_like_a_json_body() {
        let (boundary, body) = image_edit_form();
        let fields = parse_multipart(&body, &boundary)
            .await
            .expect("form parses");
        let view = multipart_text_view(&fields);
        // The file is not text the scanners or the estimate should read.
        assert_eq!(
            view,
            serde_json::json!({"model": "image-model", "prompt": "add a hat", "n": "3"})
        );

        // Priced per image from the form's `n`, like a JSON generation.
        let mut route = minimal_model("image-model");
        route.model_type = "image".into();
        route.cost_per_image = 0.04;
        assert!((compute_modality_cost(Some(&route), &view) - 0.12).abs() < 1e-9);
        let json_body = serde_json::json!({"prompt": "x", "n": 2});
        assert!((compute_modality_cost(Some(&route), &json_body) - 0.08).abs() < 1e-9);
        let unset = serde_json::json!({"prompt": "x"});
        assert!((compute_modality_cost(Some(&route), &unset) - 0.04).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_redacted_prompt_is_what_the_form_forwards() {
        let (boundary, body) = image_edit_form();
        let mut fields = parse_multipart(&body, &boundary)
            .await
            .expect("form parses");
        let mut view = multipart_text_view(&fields);
        view["prompt"] = serde_json::json!("add a [REDACTED]");
        apply_multipart_text_view(&mut fields, &view);

        let prompt = fields.iter().find(|f| f.name == "prompt").unwrap();
        assert_eq!(prompt.data.as_ref(), b"add a [REDACTED]");
        // The file part and the untouched fields keep their bytes.
        let image = fields.iter().find(|f| f.name == "image").unwrap();
        assert_eq!(image.data.as_ref(), b"PNGDATA");
        assert_eq!(image.file_name.as_deref(), Some("cat.png"));
        let n = fields.iter().find(|f| f.name == "n").unwrap();
        assert_eq!(n.data.as_ref(), b"3");
    }

    #[test]
    fn a_repeated_text_field_is_scanned_and_redacted_in_every_occurrence() {
        let text = |name: &str, data: &str| MultipartField {
            name: name.into(),
            file_name: None,
            content_type: None,
            data: Bytes::from(data.to_string()),
        };
        let mut fields = vec![text("prompt", "first"), text("prompt", "second")];
        let mut view = multipart_text_view(&fields);
        assert_eq!(view["prompt"], serde_json::json!(["first", "second"]));
        view["prompt"][1] = serde_json::json!("[REDACTED]");
        apply_multipart_text_view(&mut fields, &view);
        assert_eq!(fields[0].data.as_ref(), b"first");
        assert_eq!(fields[1].data.as_ref(), b"[REDACTED]");
    }
}
