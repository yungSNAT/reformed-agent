// Keep the test filter `handler::prompt_parts::coverage_gen::` usable when
// prompt_parts.rs is included directly into the handler module.
mod handler {
    mod prompt_parts {
        mod coverage_gen {
            use super::super::super::super::{
                decode_text_bytes, embedded_resource_to_parts, extract_prompt_payload,
                format_resource_text, image_block_to_parts, is_text_resource_mime,
                path_from_file_uri, resource_display_name, resource_link_to_parts, BASE64_STANDARD,
                MAX_ACP_RESOURCE_BYTES, TEXT_RESOURCE_MIME_TYPES,
            };
            use serde_json::{json, Map, Value};
            use std::fs;
            use std::io::ErrorKind;
            use std::path::PathBuf;
            use std::sync::atomic::{AtomicUsize, Ordering};

            struct ResourceTestDir(PathBuf);

            impl ResourceTestDir {
                fn new() -> Self {
                    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
                    let root = std::env::temp_dir(); // nosemgrep: rust.lang.security.temp-dir.temp-dir
                    loop {
                        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                        let path = root.join(format!(
                            "hermes-acp-prompt-parts-{}-{id}",
                            std::process::id()
                        ));
                        match fs::create_dir(&path) {
                            Ok(()) => return Self(path),
                            Err(err) if err.kind() == ErrorKind::AlreadyExists => {}
                            Err(err) => panic!("failed to create {}: {err}", path.display()),
                        }
                    }
                }

                fn path(&self, name: &str) -> PathBuf {
                    self.0.join(name)
                }
            }

            impl Drop for ResourceTestDir {
                fn drop(&mut self) {
                    let _ = fs::remove_dir_all(&self.0);
                }
            }

            fn object(value: &Value) -> &Map<String, Value> {
                value.as_object().expect("expected an object fixture")
            }

            fn only_text(parts: &[Value]) -> &str {
                assert_eq!(parts.len(), 1, "expected one text part");
                assert_eq!(parts[0]["type"], "text");
                parts[0]["text"].as_str().expect("expected a text string")
            }

            #[test]
            fn text_mime_controls_nul_handling_and_invalid_utf8_is_lossy() {
                assert!(!is_text_resource_mime(None));
                assert!(!is_text_resource_mime(Some(" ; charset=utf-8")));
                assert!(!is_text_resource_mime(Some("application/octet-stream")));
                assert!(is_text_resource_mime(Some(" Text/Plain ; charset=UTF-8")));

                for mime in TEXT_RESOURCE_MIME_TYPES {
                    let parameterized = format!(" {} ; charset=UTF-8", mime.to_ascii_uppercase());
                    assert!(is_text_resource_mime(Some(&parameterized)));
                }

                assert_eq!(decode_text_bytes(b"a\0b", None), None);
                assert_eq!(
                    decode_text_bytes(b"a\0b", Some(" Text/Plain ; charset=UTF-8")).as_deref(),
                    Some("a\0b")
                );
                assert_eq!(
                    decode_text_bytes(b"a\xffb", None).as_deref(),
                    Some("a\u{fffd}b")
                );
            }

            #[test]
            fn file_uri_conversion_accepts_local_paths_and_rejects_remote_hosts() {
                for uri in [
                    " \t ",
                    "https://example.invalid/report.txt",
                    "file://server/share/report.txt",
                ] {
                    assert_eq!(path_from_file_uri(uri), None, "{uri:?}");
                }

                for (uri, path) in [
                    ("  folder/report.txt  ", "folder/report.txt"),
                    ("file://localhost/tmp/report.txt", "/tmp/report.txt"),
                    ("file:///D:/Work/report.txt", "/mnt/d/Work/report.txt"),
                    ("file:///%3Aflag.txt", ":flag.txt"),
                ] {
                    assert_eq!(path_from_file_uri(uri), Some(PathBuf::from(path)), "{uri}");
                }
            }

            #[test]
            fn attachment_labels_prefer_metadata_then_a_basename_then_the_uri() {
                let file_uri = "file:///tmp/report.txt";
                let remote_uri = "https://example.invalid/report.txt";
                assert_eq!(
                    resource_display_name(
                        file_uri,
                        Some(" report.txt "),
                        Some(" Quarterly report ")
                    ),
                    "Quarterly report (report.txt)"
                );
                assert_eq!(
                    resource_display_name(file_uri, Some(" Report "), Some("Report ")),
                    "Report"
                );
                assert_eq!(
                    resource_display_name(file_uri, Some(" "), Some(" Report ")),
                    "Report"
                );
                assert_eq!(
                    resource_display_name(file_uri, Some(" Named report "), None),
                    "Named report"
                );
                assert_eq!(
                    resource_display_name(file_uri, None, Some(" ")),
                    "report.txt"
                );
                assert_eq!(resource_display_name(remote_uri, None, None), remote_uri);
                assert_eq!(
                    format_resource_text(" \t ", "body", Some("notes"), None, Some("preview")),
                    "[Attached file: notes] (preview)\n\nbody"
                );
            }

            #[test]
            fn image_blocks_preserve_data_urls_and_accept_nested_urls() {
                let inline = image_block_to_parts(object(&json!({
                    "mimeType": "image/jpeg",
                    "data": "  data:image/gif;base64,AP8=  ",
                    "url": "https://example.invalid/ignored"
                })));
                assert_eq!(
                    inline,
                    vec![json!({
                        "type": "image_url",
                        "image_url": {"url": "data:image/gif;base64,AP8="}
                    })]
                );

                let nested = image_block_to_parts(object(&json!({
                    "image_url": {"url": "  https://example.invalid/image.png  "}
                })));
                assert_eq!(
                    nested,
                    vec![json!({
                        "type": "image_url",
                        "image_url": {"url": "https://example.invalid/image.png"}
                    })]
                );

                let empty = image_block_to_parts(object(&json!({
                    "data": " ",
                    "image_url": {"url": "\t"}
                })));
                assert!(empty.is_empty());
            }

            #[test]
            fn blank_resource_links_are_ignored_and_nonfile_links_are_described() {
                assert!(resource_link_to_parts(object(&json!({"uri": " \t "}))).is_empty());

                let parts = resource_link_to_parts(object(&json!({
                    "uri": "  https://example.invalid/report.txt  ",
                    "name": "report.txt",
                    "title": "Report"
                })));
                assert_eq!(
                    only_text(&parts),
                    concat!(
                        "[Attached file: Report (report.txt)]\n",
                        "URI: https://example.invalid/report.txt\n\n",
                        "[Resource link only; Hermes cannot read non-file ACP resource URIs directly.]"
                    )
                );
            }

            #[test]
            fn linked_images_use_canonical_explicit_mime_or_infer_it_from_the_extension() {
                let dir = ResourceTestDir::new();
                for (filename, mime, expected_url) in [
                    (
                        "scan.bin",
                        Some(" IMAGE/JPEG ; ignored=yes"),
                        "data:image/jpeg;base64,AAH/",
                    ),
                    ("scan.PNG", None, "data:image/png;base64,AAH/"),
                ] {
                    let path = dir.path(filename);
                    fs::write(&path, b"\x00\x01\xff").unwrap();
                    let uri = path.to_str().unwrap();
                    let parts =
                        resource_link_to_parts(object(&json!({"uri": uri, "mime_type": mime})));
                    assert_eq!(
                        parts,
                        vec![
                            json!({
                                "type": "text",
                                "text": format!("[Attached image: {filename}]\nURI: {uri}")
                            }),
                            json!({"type": "image_url", "image_url": {"url": expected_url}}),
                        ]
                    );
                }
            }

            #[test]
            fn oversized_linked_images_are_reported_without_an_image_part() {
                let dir = ResourceTestDir::new();
                let path = dir.path("large.png");
                let cap = MAX_ACP_RESOURCE_BYTES;
                let size = cap + 1;
                fs::File::create(&path)
                    .unwrap()
                    .set_len(size as u64)
                    .unwrap();
                let uri = path.to_str().unwrap();

                let parts =
                    resource_link_to_parts(object(&json!({"uri": uri, "title": "Large scan"})));
                assert_eq!(
                    only_text(&parts),
                    format!(
                        "[Attached file: Large scan]\nURI: {uri}\n\n\
                         [Image too large to inline: {size} bytes, cap={cap}]"
                    )
                );
            }

            #[test]
            fn missing_linked_resources_report_image_and_file_read_errors() {
                let dir = ResourceTestDir::new();
                for (filename, kind) in [("missing.png", "image"), ("missing.txt", "file")] {
                    let path = dir.path(filename);
                    let uri = path.to_str().unwrap();
                    let expected_error = fs::File::open(&path).unwrap_err();
                    assert_eq!(expected_error.kind(), ErrorKind::NotFound);

                    let parts = resource_link_to_parts(object(&json!({"uri": uri})));
                    assert_eq!(
                        only_text(&parts),
                        format!(
                            "[Attached file: {filename}]\nURI: {uri}\n\n\
                             [Could not read attached {kind}: {expected_error}]"
                        )
                    );
                }
            }

            #[test]
            fn oversized_linked_text_includes_only_the_prefix_and_a_size_note() {
                let dir = ResourceTestDir::new();
                let path = dir.path("large.txt");
                let cap = MAX_ACP_RESOURCE_BYTES;
                let mut data = vec![b'x'; cap];
                data.extend_from_slice(b"tail");
                let size = data.len();
                fs::write(&path, data).unwrap();
                let uri = path.to_str().unwrap();

                let parts =
                    resource_link_to_parts(object(&json!({"uri": uri, "mimeType": "text/plain"})));
                let (header, body) = only_text(&parts).split_once("\n\n").unwrap();
                assert_eq!(
                    header,
                    format!("[Attached file: large.txt] (truncated to {cap} of {size} bytes)\nURI: {uri}")
                );
                assert_eq!(body.len(), cap);
                assert!(body.bytes().all(|byte| byte == b'x'));
            }

            #[test]
            fn linked_binary_data_is_omitted_with_a_canonical_or_unknown_mime() {
                let dir = ResourceTestDir::new();
                let path = dir.path("payload.bin");
                fs::write(&path, b"\0\x01\x02").unwrap();
                let uri = path.to_str().unwrap();

                for (mime, label) in [
                    (
                        Some(" Application/Octet-Stream ; version=1 "),
                        "application/octet-stream",
                    ),
                    (None, "unknown"),
                ] {
                    let parts =
                        resource_link_to_parts(object(&json!({"uri": uri, "mimeType": mime})));
                    assert_eq!(
                        only_text(&parts),
                        format!(
                            "[Attached file: payload.bin]\nURI: {uri}\n\n\
                             [Binary file omitted: 3 bytes, mime={label}]"
                        )
                    );
                }
            }

            #[test]
            fn embedded_resources_without_text_or_blob_produce_no_parts() {
                for block in [
                    json!({}),
                    json!({"resource": "not an object"}),
                    json!({"resource": {}}),
                    json!({"resource": {"uri": "unused.txt", "text": 42}}),
                ] {
                    assert!(embedded_resource_to_parts(object(&block)).is_empty());
                }
            }

            #[test]
            fn embedded_images_include_the_uri_only_when_present() {
                for (uri, caption) in [
                    (" \t ", "[Attached image: ]"),
                    (
                        " folder/icon.png ",
                        "[Attached image: icon.png]\nURI: folder/icon.png",
                    ),
                ] {
                    let parts = embedded_resource_to_parts(object(&json!({
                        "resource": {
                            "uri": uri,
                            "mime_type": " Image/PNG ; ignored=yes ",
                            "blob": "AP8="
                        }
                    })));
                    assert_eq!(
                        parts,
                        vec![
                            json!({"type": "text", "text": caption}),
                            json!({
                                "type": "image_url",
                                "image_url": {"url": "data:image/png;base64,AP8="}
                            }),
                        ]
                    );
                }
            }

            #[test]
            fn oversized_embedded_image_reports_the_decoded_size() {
                let cap = MAX_ACP_RESOURCE_BYTES;
                let size = cap + 1;
                let blob = base64::Engine::encode(&BASE64_STANDARD, vec![0_u8; size]);
                let parts = embedded_resource_to_parts(object(&json!({
                    "resource": {"uri": "folder/large.png", "mimeType": "image/png", "blob": blob}
                })));

                assert_eq!(
                    only_text(&parts),
                    format!(
                        "[Attached file: large.png]\nURI: folder/large.png\n\n\
                         [Embedded image too large to inline: {size} bytes, cap={cap}]"
                    )
                );
            }

            #[test]
            fn embedded_text_blobs_fall_back_to_raw_text_and_truncate_at_the_cap() {
                let short = embedded_resource_to_parts(object(&json!({
                    "resource": {"mimeType": "text/plain", "blob": "raw text!"}
                })));
                assert_eq!(only_text(&short), "[Attached file: ]\n\nraw text!");

                let cap = MAX_ACP_RESOURCE_BYTES;
                let mut blob = "x".repeat(cap);
                blob.push('!');
                let size = blob.len();
                let parts = embedded_resource_to_parts(object(&json!({
                    "resource": {
                        "uri": "folder/large.txt",
                        "mimeType": "text/plain",
                        "blob": blob
                    }
                })));
                let (header, body) = only_text(&parts).split_once("\n\n").unwrap();
                assert_eq!(header, "[Attached file: large.txt]\nURI: folder/large.txt");
                let (prefix, note) = body.rsplit_once("\n\n").unwrap();
                assert_eq!(prefix.len(), cap);
                assert!(prefix.bytes().all(|byte| byte == b'x'));
                assert_eq!(note, format!("[Truncated to {cap} of {size} bytes]"));
            }

            #[test]
            fn embedded_binary_blobs_report_the_decoded_size_and_mime() {
                for (mime, label) in [
                    (
                        Some(" Application/Octet-Stream ; version=1 "),
                        "application/octet-stream",
                    ),
                    (None, "unknown"),
                ] {
                    let parts = embedded_resource_to_parts(object(&json!({
                        "resource": {"uri": "payload.bin", "mime_type": mime, "blob": "AAEC"}
                    })));
                    assert_eq!(
                        only_text(&parts),
                        format!(
                            "[Attached file: payload.bin]\nURI: payload.bin\n\n\
                             [Binary embedded file omitted: 3 bytes, mime={label}]"
                        )
                    );
                }
            }

            #[test]
            fn mixed_prompt_preserves_order_and_prefers_embedded_text_over_a_blob() {
                let result = extract_prompt_payload(object(&json!({
                    "prompt": [
                        "first",
                        17,
                        {
                            "type": "resource",
                            "resource": {
                                "uri": " folder/notes.md ",
                                "text": "verbatim",
                                "mimeType": "image/png",
                                "blob": "AP8="
                            }
                        },
                        {"type": "image", "data": "data:image/png;base64,AP8="},
                        {"type": "future_type", "text": "last"}
                    ]
                })));

                let attachment = "[Attached file: notes.md]\nURI: folder/notes.md\n\nverbatim";
                assert_eq!(result.user_text, format!("first\n{attachment}\nlast"));
                assert_eq!(
                    result.user_content,
                    json!([
                        {"type": "text", "text": "first"},
                        {"type": "text", "text": attachment},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64,AP8="}},
                        {"type": "text", "text": "last"}
                    ])
                );
                assert!(result.has_content);
                assert!(!result.text_only_prompt);
            }

            #[test]
            fn embedded_text_attachment_keeps_the_prompt_from_being_text_only() {
                let result = extract_prompt_payload(object(&json!({
                    "prompt": [{"type": "resource", "resource": {"text": "notes"}}]
                })));

                let attachment = "[Attached file: ]\n\nnotes";
                assert_eq!(result.user_text, attachment);
                assert_eq!(
                    result.user_content,
                    json!([{"type": "text", "text": attachment}])
                );
                assert!(result.has_content);
                assert!(!result.text_only_prompt);
            }

            #[test]
            fn unsupported_prompt_blocks_without_text_produce_an_empty_payload() {
                let result = extract_prompt_payload(object(&json!({
                    "content": [null, false, {"type": "future_type", "text": 42}]
                })));

                assert!(result.user_text.is_empty());
                assert_eq!(result.user_content, json!(""));
                assert!(!result.has_content);
                assert!(!result.text_only_prompt);
            }

            #[test]
            fn empty_prompt_array_is_text_only_and_has_no_content() {
                let result = extract_prompt_payload(object(&json!({"prompt": []})));

                assert!(result.user_text.is_empty());
                assert_eq!(result.user_content, json!(""));
                assert!(!result.has_content);
                assert!(result.text_only_prompt);
            }

            #[test]
            fn non_string_non_array_prompt_falls_back_to_top_level_text() {
                let result = extract_prompt_payload(object(&json!({
                    "prompt": {"text": "not an array"},
                    "text": " fallback "
                })));

                assert_eq!(result.user_text, " fallback ");
                assert_eq!(result.user_content, json!(" fallback "));
                assert!(result.has_content);
                assert!(result.text_only_prompt);
            }
        }
    }
}
