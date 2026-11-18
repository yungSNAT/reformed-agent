//! UTC instants in the text forms the workspace wrote while it used chrono,
//! so stored records, file names and output keep their exact shape.

use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp};

/// The fractional digits chrono's `SecondsFormat::AutoSi` chose: none, 3, 6
/// or 9, whichever shows every nonzero digit.
fn auto_si_digits(ts: Timestamp) -> usize {
    match ts.subsec_nanosecond() {
        0 => 0,
        n if n % 1_000_000 == 0 => 3,
        n if n % 1_000 == 0 => 6,
        _ => 9,
    }
}

/// `2026-11-17T19:51:55.123456789+00:00`: chrono's `to_rfc3339`.
pub fn rfc3339(ts: Timestamp) -> String {
    format!(
        "{:.*}",
        auto_si_digits(ts),
        ts.display_with_offset(Offset::UTC)
    )
}

/// `2026-11-17T19:51:55.123Z`: the form chrono's serde wrote, and
/// [`serde_utc`] writes, for a JSON value built by hand.
pub fn rfc3339_z(ts: Timestamp) -> String {
    format!("{:.*}", auto_si_digits(ts), ts)
}

/// The current time as [`rfc3339`] writes it.
pub fn now_rfc3339() -> String {
    rfc3339(Timestamp::now())
}

/// `ts` moved by `secs` seconds, or `None` outside the range of instants
/// (years -9999 to 9999). Token responses go through this, so an issuer's
/// out-of-range `expires_in` is refused instead of reading as a credential
/// that never expires.
pub fn checked_plus_secs(ts: Timestamp, secs: i64) -> Option<Timestamp> {
    ts.checked_add(SignedDuration::from_secs(secs)).ok()
}

/// Now moved by `secs` seconds, clamped to the range of instants.
pub fn now_plus_secs(secs: i64) -> Timestamp {
    let edge = if secs < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    };
    checked_plus_secs(Timestamp::now(), secs).unwrap_or(edge)
}

/// Reads RFC 3339 as chrono's `DateTime::parse_from_rfc3339` did:
/// `YYYY-MM-DD`, then `T`, `t` or a space, `HH:MM:SS`, an optional fraction
/// (digits after the ninth are ignored), and `Z`, `z` or a `+HH:MM` offset
/// below 24 hours (`-` or U+2212 for west). Anything else is `None`. A leap
/// second (`:60`) reads as second 59.
pub fn parse_rfc3339(text: &str) -> Option<Timestamp> {
    let (civil, offset) = parse_rfc3339_parts(text)?;
    offset.to_timestamp(civil).ok()
}

/// The civil time and offset [`parse_rfc3339`] reads, before it joins them
/// into an instant. Joining fails only for a time past [`Timestamp::MAX`],
/// so a caller can tell a time that is too late from text that is no time.
pub fn parse_rfc3339_parts(text: &str) -> Option<(DateTime, Offset)> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i32> {
        bytes[from..to].iter().try_fold(0i32, |value, byte| {
            byte.is_ascii_digit()
                .then(|| value * 10 + i32::from(byte - b'0'))
        })
    };
    let separators_hold = bytes[4] == b'-'
        && bytes[7] == b'-'
        && matches!(bytes[10], b'T' | b't' | b' ')
        && bytes[13] == b':'
        && bytes[16] == b':';
    if !separators_hold {
        return None;
    }
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);

    let mut rest = &text[19..];
    let mut nanosecond = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let kept = &fraction[..digits.min(9)];
        nanosecond = kept.parse::<i32>().ok()? * 10i32.pow(9 - kept.len() as u32);
        rest = &fraction[digits..];
    }

    let offset_seconds = match rest {
        "Z" | "z" => 0,
        _ => {
            let (west, unsigned) = if let Some(after) = rest.strip_prefix('+') {
                (false, after)
            } else if let Some(after) = rest.strip_prefix('-') {
                (true, after)
            } else {
                (true, rest.strip_prefix('\u{2212}')?)
            };
            let offset = unsigned.as_bytes();
            if offset.len() != 5 || offset[2] != b':' {
                return None;
            }
            let digit = |at: usize| {
                offset[at]
                    .is_ascii_digit()
                    .then(|| i32::from(offset[at] - b'0'))
            };
            let hours = digit(0)? * 10 + digit(1)?;
            let minutes = digit(3)? * 10 + digit(4)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            if west {
                -seconds
            } else {
                seconds
            }
        }
    };

    let civil = DateTime::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
        i8::try_from(hour).ok()?,
        i8::try_from(minute).ok()?,
        i8::try_from(if second == 60 { 59 } else { second }).ok()?,
        nanosecond,
    )
    .ok()?;
    Some((civil, Offset::from_seconds(offset_seconds).ok()?))
}

/// Serde for a [`Timestamp`] in chrono's form: `Z`, with 0, 3, 6 or 9
/// fractional digits. Reads any RFC 3339 or ISO 8601 instant with an offset.
pub mod serde_utc {
    use jiff::Timestamp;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(ts: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::rfc3339_z(*ts))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Timestamp, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }

    /// The same form for an `Option<Timestamp>`; `None` is `null`.
    pub mod option {
        use jiff::Timestamp;
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(
            ts: &Option<Timestamp>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match ts {
                Some(ts) => super::serialize(ts, serializer),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<Timestamp>, D::Error> {
            #[derive(Deserialize)]
            struct Wrapped(#[serde(with = "super")] Timestamp);
            Ok(Option::<Wrapped>::deserialize(deserializer)?.map(|Wrapped(ts)| ts))
        }
    }
}

/// The calendar date and clock time of `ts` in UTC.
pub fn utc_civil(ts: Timestamp) -> DateTime {
    TimeZone::UTC.to_datetime(ts)
}

#[cfg(test)]
mod tests;
