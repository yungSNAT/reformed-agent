//! The queued push event, built from a webhook payload as
//! `scripts/upstream_webhook_sync.py` built it.

use hermes_py::Py;

#[derive(Debug, Clone, PartialEq)]
pub struct SyncEvent {
    pub delivery_id: String,
    pub event_name: String,
    pub repository: String,
    pub git_ref: String,
    pub before_sha: String,
    pub after_sha: String,
    pub compare_url: String,
    pub pushed_at: String,
    pub payload: Py,
}

impl SyncEvent {
    /// `make_event_from_payload`: `repository.full_name` stripped, and
    /// `str()` of `ref`, `before`, `after` and `compare`, stripped. The script
    /// dropped the connection when the payload or `repository` was not an
    /// object or `full_name` was not a string; this returns `None` instead.
    pub fn from_payload(delivery_id: &str, event_name: &str, payload: Py) -> Option<SyncEvent> {
        if !payload.is_dict() {
            return None;
        }
        let repository = match payload.get("repository") {
            None => String::new(),
            Some(repo) if repo.is_dict() => match repo.get("full_name") {
                None => String::new(),
                Some(Py::Str(name)) => hermes_py::strip(name).to_string(),
                Some(_) => return None,
            },
            Some(_) => return None,
        };
        let field = |key: &str| {
            payload
                .get(key)
                .map(|v| hermes_py::strip(&hermes_py::str_of(v)).to_string())
                .unwrap_or_default()
        };
        Some(SyncEvent {
            delivery_id: delivery_id.to_string(),
            event_name: event_name.to_string(),
            repository,
            git_ref: field("ref"),
            before_sha: field("before"),
            after_sha: field("after"),
            compare_url: field("compare"),
            pushed_at: hermes_py::utc_isoformat(),
            payload,
        })
    }
}

/// A delivery id safe to put in a file name: GitHub sends a GUID. The script
/// used the header as given, so a signed delivery with `../` in its id wrote
/// report files outside the report folder.
pub fn valid_delivery_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(text: &str) -> Py {
        hermes_py::loads(text).unwrap()
    }

    #[test]
    fn fields_are_stringified_and_stripped_as_python_did() {
        let event = SyncEvent::from_payload(
            "d1",
            "push",
            payload(r#"{"repository": {"full_name": " o/r "}, "ref": " refs/heads/main\n", "before": null, "after": 7, "compare": true}"#),
        )
        .unwrap();
        assert_eq!(event.repository, "o/r");
        assert_eq!(event.git_ref, "refs/heads/main");
        assert_eq!(event.before_sha, "None");
        assert_eq!(event.after_sha, "7");
        assert_eq!(event.compare_url, "True");
        assert!(event.pushed_at.ends_with("+00:00"));
    }

    #[test]
    fn missing_fields_are_empty() {
        let event = SyncEvent::from_payload("d1", "push", payload("{}")).unwrap();
        assert_eq!(
            (event.repository.as_str(), event.git_ref.as_str()),
            ("", "")
        );
    }

    #[test]
    fn shapes_the_script_crashed_on_are_refused() {
        for text in [
            "[]",
            "1",
            r#"{"repository": []}"#,
            r#"{"repository": {"full_name": 1}}"#,
        ] {
            assert!(
                SyncEvent::from_payload("d1", "push", payload(text)).is_none(),
                "{text}"
            );
        }
    }

    #[test]
    fn delivery_ids_must_be_file_name_safe() {
        assert!(valid_delivery_id("72d3162e-cc78-11e3-81ab-4c9367dc0958"));
        for bad in [
            "",
            "../x",
            ".hidden",
            "a/b",
            "a\\b",
            "a b",
            &"x".repeat(129),
        ] {
            assert!(!valid_delivery_id(bad), "{bad}");
        }
    }
}
