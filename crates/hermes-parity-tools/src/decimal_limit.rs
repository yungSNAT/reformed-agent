//! Parse decimal limits with the Python integer grammar used by the reporting CLIs.

#[derive(Clone, Debug)]
pub(crate) struct DecimalLimit {
    pub(crate) count: usize,
    pub(crate) label: String,
}

// Unicode 17 decimal digits are defined in blocks of ten, one per number system.
// Each value below is a character for which CPython 3.15 unicodedata.decimal returns 0.
const DECIMAL_ZEROES: [u32; 77] = [
    0x0030, 0x0660, 0x06f0, 0x07c0, 0x0966, 0x09e6, 0x0a66, 0x0ae6, 0x0b66, 0x0be6, 0x0c66, 0x0ce6,
    0x0d66, 0x0de6, 0x0e50, 0x0ed0, 0x0f20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80,
    0x1a90, 0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
    0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
    0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
    0x11d50, 0x11da0, 0x11de0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0,
    0x1d7ce, 0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950,
    0x1fbf0,
];

fn decimal_digit(character: char) -> Option<char> {
    let point = u32::from(character);
    let index = DECIMAL_ZEROES.partition_point(|zero| *zero <= point);
    let zero = DECIMAL_ZEROES[index.checked_sub(1)?];
    char::from_digit(point - zero, 10)
}

pub(crate) fn parse(input: &str) -> Result<DecimalLimit, String> {
    let input = input.trim();
    let negative = input.starts_with('-');
    let unsigned = input.strip_prefix(['+', '-']).unwrap_or(input);
    let mut digits = String::new();
    for group in unsigned.split('_') {
        if group.is_empty() {
            return Err("expected an integer".to_owned());
        }
        for character in group.chars() {
            digits.push(decimal_digit(character).ok_or_else(|| "expected an integer".to_owned())?);
        }
    }
    if digits.len() > 4300 {
        return Err("too many decimal digits".to_owned());
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(DecimalLimit {
            count: 0,
            label: "0".to_owned(),
        });
    }
    if negative {
        return Ok(DecimalLimit {
            count: 0,
            label: format!("-{digits}"),
        });
    }
    Ok(DecimalLimit {
        count: digits.parse().unwrap_or(usize::MAX),
        label: digits.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn follows_python_decimal_digits_and_integer_whitespace() {
        let limit = parse("\u{a0}+١_２\u{2003}").unwrap();
        assert_eq!(limit.count, 12);
        assert_eq!(limit.label, "12");
        assert_eq!(parse("-𝟎٢").unwrap().label, "-2");
        assert_eq!(parse("-０").unwrap().label, "0");
        for invalid in ["²", "١__２", "\u{1c}12", "１２\u{1f}"] {
            assert!(parse(invalid).is_err(), "{invalid:?}");
        }
    }
}
