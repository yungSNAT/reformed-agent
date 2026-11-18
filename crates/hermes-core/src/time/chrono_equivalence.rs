//! Evidence for the move from chrono: each helper against chrono itself.
//! Removed with chrono; the literal cases in `tests.rs` stay.

use super::*;
use chrono::{DateTime as ChronoDateTime, NaiveDate, NaiveDateTime, Utc};

/// xorshift64*, so the sweep needs no extra crate and repeats exactly.
struct Sweep(u64);
impl Sweep {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Instants from 1900 to 2200 with every fraction shape AutoSi tells apart.
fn instants() -> Vec<(Timestamp, ChronoDateTime<Utc>)> {
    let mut sweep = Sweep(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::new();
    for i in 0..20_000u64 {
        let second = (sweep.next() % 9_467_280_000) as i64 - 2_208_988_800;
        let raw = (sweep.next() % 1_000_000_000) as i32;
        let nanosecond = match i % 5 {
            0 => 0,
            1 => raw - raw % 1_000_000,
            2 => raw - raw % 1_000,
            3 => raw - raw % 100,
            _ => raw,
        };
        let ts = Timestamp::new(second, nanosecond).unwrap();
        let dt = ChronoDateTime::from_timestamp(second, nanosecond as u32).unwrap();
        out.push((ts, dt));
    }
    out
}

#[test]
fn rfc3339_and_serde_match_chrono() {
    #[derive(serde::Serialize)]
    struct Jiff(#[serde(with = "serde_utc")] Timestamp);
    for (ts, dt) in instants() {
        assert_eq!(rfc3339(ts), dt.to_rfc3339(), "{dt:?}");
        assert_eq!(
            serde_json::to_string(&Jiff(ts)).unwrap(),
            serde_json::to_string(&dt).unwrap(),
            "{dt:?}"
        );
        assert_eq!(
            format!("{ts:.0}"),
            dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        );
        assert_eq!(
            format!("{ts:.3}"),
            dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        assert_eq!(
            Some(ts.as_nanosecond()),
            dt.timestamp_nanos_opt().map(i128::from)
        );
        // Before 1970 with a fraction, jiff truncates toward zero where chrono
        // floors; the workspace converts only current instants to seconds.
        if ts.as_second() >= 0 {
            assert_eq!(ts.as_second(), dt.timestamp());
            assert_eq!(ts.as_millisecond(), dt.timestamp_millis());
        }
    }
}

const PATTERNS: &[&str] = &[
    "%Y%m%dT%H%M%SZ",
    "%Y%m%d",
    "%Y-%m-%d_%H%M%S",
    "%Y-%m-%d %H:%M:%S UTC",
    "%Y%m%d_%H%M%S",
    "%Y%m%dT%H%M%S%.3fZ",
    "%Y%m%d-%H%M%S",
    "%Y%m%d%H%M%S",
    "%Y-%m-%d %H:%M:%S",
    "%H%M%S",
    "%H:%M:%S",
    "%Y-%m-%d",
    "%b %d",
    "%b %d, %Y",
    "%Y-%m-%dT%H:%M:%S+00:00",
    "%Y-%m-%dT%H:%M:%S%.6f+00:00",
    "%A, %B %d, %Y %I:%M %p",
    "%B %d, %Y at %I:%M %p",
];

#[test]
fn strftime_patterns_match_chrono() {
    for (ts, dt) in instants() {
        for pattern in PATTERNS {
            assert_eq!(
                ts.strftime(pattern).to_string(),
                dt.format(pattern).to_string(),
                "{pattern} {dt:?}"
            );
        }
    }
}

fn rfc3339_inputs() -> Vec<String> {
    let mut inputs: Vec<String> = [
        "2026-03-08T09:30:15Z",
        "2026-03-08t09:30:15z",
        "2026-03-08 09:30:15Z",
        "2026-03-08T09:30:15+00:00",
        "2026-03-08T09:30:15-00:00",
        "2026-03-08T19:30:15+10:00",
        "2026-03-08T04:00:15-05:30",
        "2026-03-08T04:00:15\u{2212}05:30",
        "2026-03-08T09:30:15.25Z",
        "2026-03-08T09:30:15.123456789Z",
        "2026-03-08T09:30:15.1234567891234Z",
        "2026-03-09T09:29:15+23:59",
        "2016-12-31T23:59:60Z",
        "2016-12-31T23:59:60.5Z",
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
        "2028-02-29T09:30:15Z",
        "2026-13-08T09:30:15Z",
        "2026-03-08T24:00:00Z",
        "2026-03-08T09:60:15Z",
        "2026-03-08T09:30:61Z",
        "2026-03-08T09:30:15UTC",
        "+2026-03-08T09:30:15Z",
        "0000-01-01T00:00:00Z",
        "9999-12-31T23:59:59Z",
        "2026-03-08T09:30:15+٠٠:00",
        "2026-03-08T09:30:15 +00:00",
        "2026-03-08T09:30:15+0000",
        "2026-03-08T09:30:15 UTC",
        "2026-03-08 09:30:15 UTC",
        "2026-03-08T09:30:15.250000+00:00",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    // Each one-byte change of a valid instant, to cover the edges of the grammar.
    let base = "2026-03-08T19:30:15.250+10:00";
    let alphabet = b"0123456789+-:.TtZz ,_x";
    for at in 0..base.len() {
        for &byte in alphabet {
            let mut bytes = base.as_bytes().to_vec();
            bytes[at] = byte;
            inputs.push(String::from_utf8(bytes).unwrap());
        }
        inputs.push(format!("{}{}", &base[..at], &base[at + 1..]));
    }
    inputs
}

/// The instant chrono parsed, as nanoseconds; a leap second (chrono keeps it
/// as nanoseconds past 1e9 in second 59) reads as second 59.
fn chrono_instant(dt: ChronoDateTime<chrono::FixedOffset>) -> i128 {
    let nanos = dt.timestamp_subsec_nanos() % 1_000_000_000;
    i128::from(dt.timestamp()) * 1_000_000_000 + i128::from(nanos)
}

#[test]
fn parse_rfc3339_matches_chrono() {
    let mut beyond_jiff = Vec::new();
    for text in rfc3339_inputs() {
        let chrono = ChronoDateTime::parse_from_rfc3339(&text)
            .ok()
            .map(chrono_instant);
        let jiff = parse_rfc3339(&text).map(Timestamp::as_nanosecond);
        match chrono {
            // jiff's instants end at 9999-12-30T22:00Z, a day short of chrono's.
            Some(nanos) if nanos > Timestamp::MAX.as_nanosecond() => {
                assert_eq!(jiff, None, "{text:?}");
                beyond_jiff.push(text);
            }
            _ => assert_eq!(jiff, chrono, "{text:?}"),
        }
    }
    println!("BEYOND-JIFF {beyond_jiff:?}");
}

/// Where serde's reading differs from chrono's relaxed reading: printed as
/// evidence, and every instant chrono itself writes must read the same.
#[test]
fn serde_reading_against_chrono() {
    #[derive(serde::Deserialize)]
    struct Jiff(#[serde(with = "serde_utc")] Timestamp);
    let mut differences = Vec::new();
    for text in rfc3339_inputs() {
        let json = serde_json::to_string(&text).unwrap();
        let chrono = serde_json::from_str::<ChronoDateTime<Utc>>(&json)
            .ok()
            .map(|dt| chrono_instant(dt.fixed_offset()));
        let jiff = serde_json::from_str::<Jiff>(&json)
            .ok()
            .map(|Jiff(ts)| ts.as_nanosecond());
        if chrono != jiff {
            differences.push(format!("{text:?}: chrono {chrono:?}, jiff {jiff:?}"));
        }
    }
    println!(
        "SERDE-DIFFERENCES {}\n{}",
        differences.len(),
        differences.join("\n")
    );
    for (ts, dt) in instants() {
        let json = serde_json::to_string(&dt).unwrap();
        let Jiff(read) = serde_json::from_str(&json).unwrap();
        assert_eq!(read, ts, "{json}");
        let json = serde_json::to_string(&dt.to_rfc3339()).unwrap();
        let Jiff(read) = serde_json::from_str(&json).unwrap();
        assert_eq!(read, ts, "{json}");
    }
}

/// The naive patterns the workspace parses: differences printed as evidence.
#[test]
fn strptime_against_chrono() {
    let inputs = [
        "2026-03-08T09:30:15",
        "2026-03-08 09:30:15",
        "2026-3-8T9:30:15",
        "2026-3-8 9:30:15",
        " 2026-03-08 09:30:15",
        "2026-03-08 09:30:15 ",
        "2026-03-08  09:30:15",
        "2026-03-0809:30:15",
        "2026-03-08T09:30:15Z",
        "2026-03-08T09:30:15.5",
        "2026-03-08 09:30:15.5",
        "2026-03-08T09:30:60",
        "2026-02-29T09:30:15",
        "2026-03-08",
        "2026-03-08x",
        "26-03-08",
        "+2026-03-08",
        "12026-03-08",
        "2026-03-08T24:00:00",
        "",
    ];
    let mut differences = Vec::new();
    for pattern in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        for text in inputs {
            let chrono = NaiveDateTime::parse_from_str(text, pattern)
                .ok()
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.f").to_string());
            let jiff = jiff::civil::DateTime::strptime(pattern, text)
                .ok()
                .map(|dt| dt.strftime("%Y-%m-%dT%H:%M:%S%.f").to_string());
            if chrono != jiff {
                differences.push(format!(
                    "{pattern} {text:?}: chrono {chrono:?}, jiff {jiff:?}"
                ));
            }
        }
    }
    for text in inputs {
        let chrono = NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .ok()
            .map(|d| d.to_string());
        let jiff = jiff::civil::Date::strptime("%Y-%m-%d", text)
            .ok()
            .map(|d| d.to_string());
        if chrono != jiff {
            differences.push(format!(
                "%Y-%m-%d {text:?}: chrono {chrono:?}, jiff {jiff:?}"
            ));
        }
    }
    println!(
        "STRPTIME-DIFFERENCES {}\n{}",
        differences.len(),
        differences.join("\n")
    );
}
