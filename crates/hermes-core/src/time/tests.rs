use super::*;

fn at(second: i64, nanosecond: i32) -> Timestamp {
    Timestamp::new(second, nanosecond).unwrap()
}

// 2026-03-08T09:30:15Z
const SECOND: i64 = 1_772_962_215;

#[test]
fn rfc3339_writes_chrono_digits_and_a_numeric_utc_offset() {
    let cases = [
        (at(SECOND, 0), "2026-03-08T09:30:15+00:00"),
        (at(SECOND, 250_000_000), "2026-03-08T09:30:15.250+00:00"),
        (at(SECOND, 123_400_000), "2026-03-08T09:30:15.123400+00:00"),
        (at(SECOND, 123_456_000), "2026-03-08T09:30:15.123456+00:00"),
        (
            at(SECOND, 123_456_700),
            "2026-03-08T09:30:15.123456700+00:00",
        ),
        (at(SECOND, 1), "2026-03-08T09:30:15.000000001+00:00"),
        (at(-1, 500_000_000), "1969-12-31T23:59:59.500+00:00"),
        (at(0, 0), "1970-01-01T00:00:00+00:00"),
    ];
    for (ts, text) in cases {
        assert_eq!(rfc3339(ts), text);
    }
}

#[test]
fn now_rfc3339_reads_back_as_an_instant_near_now() {
    let before = Timestamp::now();
    let text = now_rfc3339();
    let after = Timestamp::now();
    assert!(text.ends_with("+00:00"), "{text}");
    let read = parse_rfc3339(&text).unwrap();
    assert!(before <= read && read <= after, "{text}");
}

#[test]
fn parse_rfc3339_accepts_what_chrono_accepted() {
    let cases = [
        ("2026-03-08T09:30:15Z", at(SECOND, 0)),
        ("2026-03-08t09:30:15z", at(SECOND, 0)),
        ("2026-03-08 09:30:15Z", at(SECOND, 0)),
        ("2026-03-08T09:30:15+00:00", at(SECOND, 0)),
        ("2026-03-08T09:30:15-00:00", at(SECOND, 0)),
        ("2026-03-08T19:30:15+10:00", at(SECOND, 0)),
        ("2026-03-08T04:00:15-05:30", at(SECOND, 0)),
        ("2026-03-08T04:00:15\u{2212}05:30", at(SECOND, 0)),
        ("2026-03-08T09:30:15.25Z", at(SECOND, 250_000_000)),
        ("2026-03-08T09:30:15.123456789Z", at(SECOND, 123_456_789)),
        (
            "2026-03-08T09:30:15.1234567891234Z",
            at(SECOND, 123_456_789),
        ),
        ("2026-03-09T09:29:15+23:59", at(SECOND, 0)),
        ("2016-12-31T23:59:60Z", at(1_483_228_799, 0)),
        ("2016-12-31T23:59:60.5Z", at(1_483_228_799, 500_000_000)),
    ];
    for (text, expected) in cases {
        assert_eq!(parse_rfc3339(text), Some(expected), "{text}");
    }
}

#[test]
fn parse_rfc3339_refuses_what_chrono_refused() {
    let refused = [
        "",
        "2026-03-08",
        "2026-03-08T09:30:15",
        "2026-03-08T09:30Z",
        "2026-03-08T09:30:15+10",
        "2026-03-08T09:30:15+1000",
        "2026-03-08T09:30:15+24:00",
        "2026-03-08T09:30:15+10:60",
        "2026-03-08T09:30:15.Z",
        "2026-03-08T09:30:15,5Z",
        "2026-03-08T09:30:15Z[UTC]",
        "2026-03-08T09:30:15 Z",
        "2026-03-08T09:30:15Z ",
        " 2026-03-08T09:30:15Z",
        "2026-03-08_09:30:15Z",
        "20260308T093015Z",
        "2026-02-29T09:30:15Z",
        "2026-13-08T09:30:15Z",
        "2026-03-08T24:00:00Z",
        "2026-03-08T09:60:15Z",
        "2026-03-08T09:30:61Z",
        "2026-03-08T09:30:15UTC",
        "+2026-03-08T09:30:15Z",
        "2026-03-08T09:30:15+٠٠:00",
    ];
    for text in refused {
        assert_eq!(parse_rfc3339(text), None, "{text}");
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct Record {
    #[serde(with = "serde_utc")]
    at: Timestamp,
    #[serde(with = "serde_utc::option", default)]
    maybe: Option<Timestamp>,
}

#[test]
fn serde_utc_writes_chrono_form() {
    let cases = [
        (at(SECOND, 0), "2026-03-08T09:30:15Z"),
        (at(SECOND, 250_000_000), "2026-03-08T09:30:15.250Z"),
        (at(SECOND, 123_400_000), "2026-03-08T09:30:15.123400Z"),
        (at(SECOND, 123_456_700), "2026-03-08T09:30:15.123456700Z"),
    ];
    for (ts, text) in cases {
        let record = Record {
            at: ts,
            maybe: Some(ts),
        };
        assert_eq!(
            serde_json::to_string(&record).unwrap(),
            format!(r#"{{"at":"{text}","maybe":"{text}"}}"#)
        );
    }
    let none = Record {
        at: at(SECOND, 0),
        maybe: None,
    };
    assert_eq!(
        serde_json::to_string(&none).unwrap(),
        r#"{"at":"2026-03-08T09:30:15Z","maybe":null}"#
    );
}

#[test]
fn serde_utc_reads_chrono_form_offsets_and_a_missing_option() {
    let cases = [
        r#"{"at":"2026-03-08T09:30:15.250Z","maybe":"2026-03-08T09:30:15.250Z"}"#,
        r#"{"at":"2026-03-08T09:30:15.250+00:00","maybe":"2026-03-08T19:30:15.25+10:00"}"#,
        r#"{"at":"2026-03-08 09:30:15.250000Z","maybe":"2026-03-08t09:30:15.250000000z"}"#,
    ];
    for text in cases {
        let record: Record = serde_json::from_str(text).unwrap();
        assert_eq!(
            record,
            Record {
                at: at(SECOND, 250_000_000),
                maybe: Some(at(SECOND, 250_000_000)),
            },
            "{text}"
        );
    }
    let record: Record = serde_json::from_str(r#"{"at":"2026-03-08T09:30:15Z"}"#).unwrap();
    assert_eq!(record.maybe, None);
    let record: Record =
        serde_json::from_str(r#"{"at":"2026-03-08T09:30:15Z","maybe":null}"#).unwrap();
    assert_eq!(record.maybe, None);
    for bad in [
        r#"{"at":"2026-03-08T09:30:15"}"#,
        r#"{"at":"yesterday"}"#,
        r#"{"at":1772962215}"#,
        r#"{"at":"2026-03-08T09:30:15Z","maybe":"soon"}"#,
    ] {
        assert!(serde_json::from_str::<Record>(bad).is_err(), "{bad}");
    }
}

/// Every strftime pattern the workspace formats a UTC instant with, at an
/// instant that exercises padding, the afternoon and a fraction.
#[test]
fn workspace_strftime_patterns_keep_their_output() {
    let ts = at(1_767_315_845, 7_000_000); // 2026-01-02T01:04:05.007Z
    let pm = at(1_772_983_815, 0); // 2026-03-08T15:30:15Z
    let cases = [
        (ts, "%Y%m%dT%H%M%SZ", "20260102T010405Z"),
        (ts, "%Y%m%d", "20260102"),
        (ts, "%Y-%m-%d_%H%M%S", "2026-01-02_010405"),
        (ts, "%Y-%m-%d %H:%M:%S UTC", "2026-01-02 01:04:05 UTC"),
        (ts, "%Y%m%d_%H%M%S", "20260102_010405"),
        (ts, "%Y%m%dT%H%M%S%.3fZ", "20260102T010405.007Z"),
        (ts, "%Y%m%d-%H%M%S", "20260102-010405"),
        (ts, "%Y%m%d%H%M%S", "20260102010405"),
        (ts, "%Y-%m-%d %H:%M:%S", "2026-01-02 01:04:05"),
        (ts, "%H%M%S", "010405"),
        (ts, "%H:%M:%S", "01:04:05"),
        (ts, "%Y-%m-%d", "2026-01-02"),
        (ts, "%b %d", "Jan 02"),
        (ts, "%b %d, %Y", "Jan 02, 2026"),
        (ts, "%Y-%m-%dT%H:%M:%S+00:00", "2026-01-02T01:04:05+00:00"),
        (
            ts,
            "%Y-%m-%dT%H:%M:%S%.6f+00:00",
            "2026-01-02T01:04:05.007000+00:00",
        ),
        (
            ts,
            "%A, %B %d, %Y %I:%M %p",
            "Friday, January 02, 2026 01:04 AM",
        ),
        (
            pm,
            "%A, %B %d, %Y %I:%M %p",
            "Sunday, March 08, 2026 03:30 PM",
        ),
        (ts, "%B %d, %Y at %I:%M %p", "January 02, 2026 at 01:04 AM"),
        (pm, "%B %d, %Y at %I:%M %p", "March 08, 2026 at 03:30 PM"),
        (at(1_772_928_000, 0), "%I:%M %p", "12:00 AM"),
        (at(1_772_971_200, 0), "%I:%M %p", "12:00 PM"),
    ];
    for (ts, pattern, text) in cases {
        assert_eq!(ts.strftime(pattern).to_string(), text, "{pattern}");
    }
}

#[test]
fn utc_civil_reads_the_utc_calendar() {
    let civil = utc_civil(at(-1, 0));
    assert_eq!(civil.to_string(), "1969-12-31T23:59:59");
}
