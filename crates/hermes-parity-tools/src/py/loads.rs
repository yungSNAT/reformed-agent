//! `json.loads` as Python reads a document: objects keep their keys in
//! document order (a repeated key keeps its first place and its last
//! value), integers stay integers however large Python would hold them up
//! to `i128`, and `NaN`, `Infinity` and `-Infinity` are numbers.
//!
//! One departure: nesting deeper than [`MAX_DEPTH`] is an error, a bound
//! of this port's own so deep input never overflows the stack. CPython
//! goes deeper (measured 17 November 2026: 3.12.10 parsed 2,997 levels,
//! 3.15.0 parsed 13,862), so between the two a caller of `loads` refuses
//! a document Python reads. `/review` cannot meet one: the agent loop
//! stores tool-call arguments only as serde_json parsed or wrote them
//! (`hermes-agent/src/tool_call_args.rs:18-24` and `:73-77`), and
//! serde_json's parser refuses nesting past 128 levels.

use super::Py;

/// Arrays and objects nested deeper than this are refused.
pub const MAX_DEPTH: usize = 1_000;

pub fn loads(text: &str) -> Result<Py, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        depth: 0,
    };
    parser.skip_ws();
    let value = parser.value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(parser.error("Extra data"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        let before = String::from_utf8_lossy(&self.bytes[..self.pos.min(self.bytes.len())]);
        format!("{what}: char {}", before.chars().count())
    }

    fn skip_ws(&mut self) {
        while matches!(self.bytes.get(self.pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        if self.text[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Py, String> {
        match self.bytes.get(self.pos) {
            Some(b'{') => self.nested(Self::object),
            Some(b'[') => self.nested(Self::array),
            Some(b'"') => self.string().map(Py::Str),
            _ if self.eat("null") => Ok(Py::Null),
            _ if self.eat("true") => Ok(Py::Bool(true)),
            _ if self.eat("false") => Ok(Py::Bool(false)),
            _ if self.eat("NaN") => Ok(Py::Float(f64::NAN)),
            _ if self.eat("Infinity") => Ok(Py::Float(f64::INFINITY)),
            _ if self.eat("-Infinity") => Ok(Py::Float(f64::NEG_INFINITY)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("Expecting value")),
        }
    }

    /// Runs `parse` one level deeper, refusing to pass [`MAX_DEPTH`].
    fn nested(&mut self, parse: fn(&mut Self) -> Result<Py, String>) -> Result<Py, String> {
        if self.depth == MAX_DEPTH {
            return Err(self.error("Maximum nesting depth exceeded"));
        }
        self.depth += 1;
        let value = parse(self);
        self.depth -= 1;
        value
    }

    fn object(&mut self) -> Result<Py, String> {
        self.pos += 1;
        let mut pairs: Vec<(String, Py)> = Vec::new();
        self.skip_ws();
        if self.eat("}") {
            return Ok(Py::Dict(pairs));
        }
        loop {
            self.skip_ws();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.error("Expecting property name enclosed in double quotes"));
            }
            let key = self.string()?;
            self.skip_ws();
            if !self.eat(":") {
                return Err(self.error("Expecting ':' delimiter"));
            }
            self.skip_ws();
            let value = self.value()?;
            match pairs.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key, value)),
            }
            self.skip_ws();
            if self.eat("}") {
                return Ok(Py::Dict(pairs));
            }
            if !self.eat(",") {
                return Err(self.error("Expecting ',' delimiter"));
            }
        }
    }

    fn array(&mut self) -> Result<Py, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.eat("]") {
            return Ok(Py::List(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value()?);
            self.skip_ws();
            if self.eat("]") {
                return Ok(Py::List(items));
            }
            if !self.eat(",") {
                return Err(self.error("Expecting ',' delimiter"));
            }
        }
    }

    fn number(&mut self) -> Result<Py, String> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let from = p.pos;
            while p.bytes.get(p.pos).is_some_and(u8::is_ascii_digit) {
                p.pos += 1;
            }
            p.pos - from
        };
        self.eat("-");
        if self.eat("0") {
            // Python's scanner reads "01" as 0 followed by extra data.
        } else if digits(self) == 0 {
            return Err(self.error("Expecting value"));
        }
        let mut float = false;
        let frac_start = self.pos;
        if self.eat(".") {
            if digits(self) == 0 {
                self.pos = frac_start;
            } else {
                float = true;
            }
        }
        let exp_start = self.pos;
        if matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if digits(self) == 0 {
                self.pos = exp_start;
            } else {
                float = true;
            }
        }
        let literal = &self.text[start..self.pos];
        if float {
            literal
                .parse::<f64>()
                .map(Py::Float)
                .map_err(|e| format!("invalid number {literal}: {e}"))
        } else {
            // shortcut: Python's int has no upper bound; past i128 this
            // refuses the document instead of reading it.
            literal
                .parse::<i128>()
                .map(Py::Int)
                .map_err(|_| format!("integer {literal} is beyond what this port reads (i128)"))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let rest = &self.text[self.pos..];
            let Some(c) = rest.chars().next() else {
                return Err(self.error("Unterminated string starting at"));
            };
            self.pos += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => out.push(self.escape()?),
                c if (c as u32) < 0x20 => return Err(self.error("Invalid control character at")),
                c => out.push(c),
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let c = self.bytes.get(self.pos).copied();
        self.pos += 1;
        Ok(match c {
            Some(b'"') => '"',
            Some(b'\\') => '\\',
            Some(b'/') => '/',
            Some(b'b') => '\u{8}',
            Some(b'f') => '\u{c}',
            Some(b'n') => '\n',
            Some(b'r') => '\r',
            Some(b't') => '\t',
            Some(b'u') => {
                let high = self.hex4()?;
                if (0xd800..0xdc00).contains(&high) && self.eat("\\u") {
                    let low = self.hex4()?;
                    if (0xdc00..0xe000).contains(&low) {
                        let code = 0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                        return char::from_u32(code)
                            .ok_or_else(|| self.error("Invalid \\uXXXX escape"));
                    }
                    self.pos -= 6;
                }
                // shortcut: Python keeps a lone surrogate in its str; a Rust
                // String cannot hold one, so the document is refused.
                char::from_u32(high)
                    .ok_or_else(|| self.error("Lone surrogate in \\uXXXX escape"))?
            }
            _ => return Err(self.error("Invalid \\escape")),
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.text.get(self.pos..self.pos + 4).unwrap_or("");
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.error("Invalid \\uXXXX escape"));
        }
        self.pos += 4;
        u32::from_str_radix(digits, 16).map_err(|_| self.error("Invalid \\uXXXX escape"))
    }
}
