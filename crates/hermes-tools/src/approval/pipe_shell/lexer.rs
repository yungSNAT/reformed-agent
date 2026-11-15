//! A bash lexer for the pipe-to-shell check: one left-to-right pass that
//! turns a command into words, operators and nested frames (`$(...)`,
//! backticks, `<(...)`, `>(...)`, subshells and here-document bodies), so
//! every rule reads a word, a quote or a command's end the same way.
//!
//! A frame's inner tokens come before the word that holds it. Constructs
//! whose reading differs between bash versions, or that the lexer does not
//! model, emit [`Token::Opaque`]; nesting past [`MAX_DEPTH`] stops the scan.

/// Frames nested deeper than this stop the scan with [`Token::Opaque`].
pub(super) const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Frame {
    /// `$(...)`; with `arith`, the outer frame of `$((...))`.
    Command,
    Backtick,
    /// `<(...)`.
    ProcessIn,
    /// `>(...)`.
    ProcessOut,
    /// `(...)`; with `arith`, a frame of `((...))` or `$((...))`.
    Subshell,
    /// A here-document body, in the order of the `<<` operators. A literal
    /// body (quoted delimiter) is an empty frame.
    HereDoc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Op {
    /// `|` or `|&`.
    Pipe,
    /// `;`, `&`, `&&`, `||`, a newline, `;;`, `;&`, `;;&` or a `)` that
    /// closes nothing.
    Terminator,
    /// A redirection other than a here-document; the next word is its target.
    Redirect,
    /// `<<` or `<<-`; the next word is the delimiter.
    HereDoc,
}

#[derive(Debug)]
pub(super) struct Word {
    pub start: usize,
    pub end: usize,
    /// The word with one level of quoting removed. Text of expansions is
    /// left out, so the value is exact only when `computed` is false.
    pub value: String,
    /// Holds an expansion: `$name`, `${...}`, `$(...)`, backticks, a process
    /// substitution, or `$'...'` with a backslash.
    pub computed: bool,
    /// Holds a quote or a backslash, so it cannot be a reserved word.
    pub quoted: bool,
}

#[derive(Debug)]
pub(super) enum Token {
    Word(Word),
    Op {
        op: Op,
        at: usize,
    },
    Open {
        frame: Frame,
        arith: bool,
        at: usize,
    },
    Close,
    Opaque,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Input,
    Paren,
    Backtick,
}

struct HereDoc {
    delimiter: String,
    literal: bool,
    strip_tabs: bool,
}

pub(super) fn tokens(src: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        end: src.len(),
        depth: 0,
        backticks: 0,
        tokens: Vec::new(),
        pending: Vec::new(),
        delimiter: None,
        stop: false,
        after_pipe: false,
        arith_head: false,
    };
    lexer.list(End::Input, false);
    lexer.tokens
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Where the current text ends: the input, or a here-document body.
    end: usize,
    depth: usize,
    /// Backtick frames open around the current position.
    backticks: usize,
    tokens: Vec<Token>,
    pending: Vec<HereDoc>,
    /// After `<<` (`Some(true)` for `<<-`): the next word is a delimiter.
    delimiter: Option<bool>,
    stop: bool,
    /// The last operator was a pipe: a newline after it continues the
    /// pipeline (`curl x |` newline `bash`).
    after_pipe: bool,
    /// The next `(` is the inner one of `((` or `$((`.
    arith_head: bool,
}

/// A word being built.
#[derive(Default)]
struct Builder {
    value: Vec<u8>,
    computed: bool,
    quoted: bool,
    /// Inside `${...}`, whose text is not part of the value.
    muted: usize,
}

impl Builder {
    fn push(&mut self, bytes: &[u8]) {
        if self.muted == 0 {
            self.value.extend_from_slice(bytes);
        }
    }
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.peek_at(0)
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes[..self.end].get(self.pos + offset).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        self.bytes[self.pos..self.end].starts_with(text.as_bytes())
    }

    /// The byte length of the character at `pos`.
    fn char_len(&self) -> usize {
        self.src[self.pos..]
            .chars()
            .next()
            .map_or(1, char::len_utf8)
    }

    fn opaque(&mut self) {
        self.tokens.push(Token::Opaque);
    }

    fn op(&mut self, op: Op, at: usize, len: usize) {
        self.pos = at + len;
        self.after_pipe = op == Op::Pipe;
        self.tokens.push(Token::Op { op, at });
    }

    /// A command list up to `end`: the input, a `)` or a backtick.
    fn list(&mut self, end: End, arith: bool) {
        while !self.stop {
            while matches!(self.peek(), Some(b' ' | b'\t')) || self.starts_with("\\\n") {
                self.pos += if self.peek() == Some(b'\\') { 2 } else { 1 };
            }
            let at = self.pos;
            let Some(b) = self.peek() else { return };
            match b {
                b'\n' => {
                    if self.after_pipe {
                        self.pos += 1;
                    } else {
                        self.op(Op::Terminator, at, 1);
                    }
                    self.here_documents();
                }
                b'#' if !arith => {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.pos += 1;
                    }
                }
                b')' if end == End::Paren => {
                    self.pos += 1;
                    return;
                }
                b')' => self.op(Op::Terminator, at, 1),
                b'`' if end == End::Backtick => {
                    self.pos += 1;
                    return;
                }
                b';' => {
                    let len = [";;&", ";;", ";&"]
                        .iter()
                        .find(|s| self.starts_with(s))
                        .map_or(1, |s| s.len());
                    self.op(Op::Terminator, at, len);
                }
                b'&' if self.starts_with("&&") => self.op(Op::Terminator, at, 2),
                b'&' if self.peek_at(1) == Some(b'>') && !arith => {
                    let len = if self.starts_with("&>>") { 3 } else { 2 };
                    self.op(Op::Redirect, at, len);
                }
                b'&' => self.op(Op::Terminator, at, 1),
                b'|' if self.starts_with("||") => self.op(Op::Terminator, at, 2),
                b'|' => {
                    let len = if self.starts_with("|&") { 2 } else { 1 };
                    self.op(Op::Pipe, at, len);
                }
                b'(' => {
                    self.pos += 1;
                    let head = std::mem::take(&mut self.arith_head);
                    let double = self.peek() == Some(b'(');
                    self.arith_head = double;
                    self.frame(Frame::Subshell, arith || double, at, End::Paren);
                    if head && self.peek() != Some(b')') {
                        // Not `))`: bash reads the text as a subshell after
                        // all, where `$SHELL` and `>` mean what they say.
                        self.opaque();
                    }
                }
                b'<' | b'>' if !arith && self.peek_at(1) != Some(b'(') => self.redirect(at),
                _ if !arith && self.descriptor_len() > 0 => {
                    self.pos += self.descriptor_len();
                    self.redirect(at);
                }
                _ => self.word(end, arith),
            }
        }
    }

    /// The length of a redirection's descriptor at `pos` (`2` in `2>`,
    /// `{fd}` in `{fd}>`), or 0.
    fn descriptor_len(&self) -> usize {
        let rest = &self.bytes[self.pos..self.end];
        let len = if rest.first() == Some(&b'{') {
            let name = rest[1..]
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                .count();
            if name > 0 && rest.get(1 + name) == Some(&b'}') {
                name + 2
            } else {
                0
            }
        } else {
            rest.iter().take_while(|b| b.is_ascii_digit()).count()
        };
        let op = rest.get(len);
        if len > 0 && matches!(op, Some(b'<' | b'>')) && rest.get(len + 1) != Some(&b'(') {
            len
        } else {
            0
        }
    }

    /// A redirection operator at `pos` (after any descriptor).
    fn redirect(&mut self, at: usize) {
        const OPS: [(&str, Op); 8] = [
            ("<<<", Op::Redirect),
            ("<<-", Op::HereDoc),
            ("<<", Op::HereDoc),
            ("<>", Op::Redirect),
            ("<&", Op::Redirect),
            (">>", Op::Redirect),
            (">|", Op::Redirect),
            (">&", Op::Redirect),
        ];
        let (len, op) = OPS
            .iter()
            .find(|(text, _)| self.starts_with(text))
            .map_or((1, Op::Redirect), |(text, op)| (text.len(), *op));
        if op == Op::HereDoc {
            self.delimiter = Some(len == 3);
        }
        self.pos += len;
        self.after_pipe = false;
        self.tokens.push(Token::Op { op, at });
    }

    fn frame(&mut self, frame: Frame, arith: bool, at: usize, end: End) {
        if self.depth >= MAX_DEPTH {
            self.opaque();
            self.stop = true;
            return;
        }
        self.depth += 1;
        self.after_pipe = false;
        self.tokens.push(Token::Open { frame, arith, at });
        if end == End::Backtick {
            self.backticks += 1;
            self.list(end, arith);
            self.backticks -= 1;
        } else {
            self.list(end, arith);
        }
        self.tokens.push(Token::Close);
        self.depth -= 1;
    }

    /// One word, which may hold quotes and nested frames.
    fn word(&mut self, end: End, arith: bool) {
        let start = self.pos;
        let mut word = Builder::default();
        while !self.stop {
            let Some(b) = self.peek() else { break };
            match b {
                b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')' => break,
                b'`' if end == End::Backtick => break,
                b'<' | b'>' if self.peek_at(1) == Some(b'(') => {
                    let frame = if b == b'<' {
                        Frame::ProcessIn
                    } else {
                        Frame::ProcessOut
                    };
                    let at = self.pos;
                    self.pos += 2;
                    word.computed = true;
                    self.frame(frame, false, at, End::Paren);
                }
                b'<' | b'>' if !arith => break,
                b'\\' => self.escape(&mut word, false),
                b'\'' => self.single_quote(&mut word),
                b'"' => self.double_quote(&mut word),
                b'$' => self.dollar(&mut word, false),
                b'`' => self.backtick(&mut word),
                _ => {
                    let len = self.char_len();
                    word.push(&self.bytes[self.pos..self.pos + len]);
                    self.pos += len;
                }
            }
        }
        if self.pos == start {
            // Nothing a word can hold: skip the byte so the scan advances.
            self.pos += self.char_len();
            return;
        }
        let value = String::from_utf8_lossy(&word.value).into_owned();
        if let Some(strip_tabs) = self.delimiter.take() {
            self.pending.push(HereDoc {
                delimiter: value.clone(),
                literal: word.quoted,
                strip_tabs,
            });
        }
        self.after_pipe = false;
        self.tokens.push(Token::Word(Word {
            start,
            end: self.pos,
            value,
            computed: word.computed,
            quoted: word.quoted,
        }));
    }

    /// `\` outside quotes (or, with `in_double`, inside double quotes).
    fn escape(&mut self, word: &mut Builder, in_double: bool) {
        word.quoted = true;
        match self.peek_at(1) {
            Some(b'\n') => self.pos += 2,
            Some(b'`' | b'$' | b'\\') if self.backticks > 0 => {
                // bash removes these backslashes before it parses a backtick
                // substitution, so `\$(` and `` \` `` there run commands.
                self.opaque();
                self.pos += 2;
            }
            Some(next) if !in_double || matches!(next, b'$' | b'`' | b'"' | b'\\') => {
                self.pos += 1;
                let len = self.char_len();
                word.push(&self.bytes[self.pos..self.pos + len]);
                self.pos += len;
            }
            Some(_) => {
                word.push(b"\\");
                self.pos += 1;
            }
            None => self.pos += 1,
        }
    }

    fn single_quote(&mut self, word: &mut Builder) {
        word.quoted = true;
        self.pos += 1;
        let start = self.pos;
        while self.peek().is_some_and(|b| b != b'\'') {
            if self.peek() == Some(b'`') && self.backticks > 0 {
                self.opaque();
            }
            self.pos += 1;
        }
        word.push(&self.bytes[start..self.pos]);
        if self.peek().is_some() {
            self.pos += 1;
        }
    }

    fn double_quote(&mut self, word: &mut Builder) {
        word.quoted = true;
        self.pos += 1;
        while !self.stop {
            match self.peek() {
                None => return,
                Some(b'"') => {
                    self.pos += 1;
                    return;
                }
                Some(b'\\') => self.escape(word, true),
                Some(b'$') => self.dollar(word, true),
                Some(b'`') => self.backtick(word),
                Some(_) => {
                    let len = self.char_len();
                    word.push(&self.bytes[self.pos..self.pos + len]);
                    self.pos += len;
                }
            }
        }
    }

    /// An opening backtick. Inside another backtick substitution, bash ends
    /// that one here instead, whatever the quoting.
    fn backtick(&mut self, word: &mut Builder) {
        if self.backticks > 0 {
            self.opaque();
        }
        word.computed = true;
        let at = self.pos;
        self.pos += 1;
        self.frame(Frame::Backtick, false, at, End::Backtick);
    }

    /// `$` and what follows it.
    fn dollar(&mut self, word: &mut Builder, in_double: bool) {
        let at = self.pos;
        match self.peek_at(1) {
            Some(b'(') => {
                word.computed = true;
                self.pos += 2;
                let arith = self.peek() == Some(b'(');
                self.arith_head = arith;
                self.frame(Frame::Command, arith, at, End::Paren);
            }
            Some(b'{') => {
                word.computed = true;
                self.pos += 2;
                self.parameter(word, in_double);
            }
            Some(b'\'') if !in_double => self.ansi_c(word),
            Some(b'"') if !in_double => {
                self.pos += 1;
                self.double_quote(word);
            }
            Some(b) if b.is_ascii_alphanumeric() || b"_@*#?$!-[".contains(&b) => {
                word.computed = true;
                self.pos += 2;
                if b.is_ascii_alphabetic() || b == b'_' {
                    while self
                        .peek()
                        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        self.pos += 1;
                    }
                }
            }
            _ => {
                word.push(b"$");
                self.pos += 1;
            }
        }
    }

    /// `$'...'`: backslash escapes, including `\'`.
    fn ansi_c(&mut self, word: &mut Builder) {
        word.quoted = true;
        self.pos += 2;
        let start = self.pos;
        while let Some(b) = self.peek() {
            match b {
                b'\'' => break,
                b'\\' => {
                    word.computed = true;
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += self.char_len();
                    }
                }
                b'`' if self.backticks > 0 => {
                    self.opaque();
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        word.push(&self.bytes[start..self.pos]);
        if self.peek().is_some() {
            self.pos += 1;
        }
    }

    /// The rest of `${...}` after `${`: up to the first `}` outside quotes.
    fn parameter(&mut self, word: &mut Builder, in_double: bool) {
        // `"${a:-"${b:-...}"}"` nests without frames; bound it the same way.
        if self.depth >= MAX_DEPTH {
            self.opaque();
            self.stop = true;
            return;
        }
        self.depth += 1;
        word.muted += 1;
        while !self.stop {
            match self.peek() {
                None => break,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => self.escape(word, false),
                Some(b'\'') => {
                    if in_double {
                        // bash 5.3 reads a quote here; older bash a literal.
                        self.opaque();
                    }
                    self.single_quote(word);
                }
                Some(b'"') => self.double_quote(word),
                Some(b'$') => self.dollar(word, in_double),
                Some(b'`') => self.backtick(word),
                Some(_) => self.pos += self.char_len(),
            }
        }
        word.muted -= 1;
        self.depth -= 1;
    }

    /// Bodies of the here-documents queued on the line just ended.
    fn here_documents(&mut self) {
        let src = self.src;
        for doc in std::mem::take(&mut self.pending) {
            let body = self.pos;
            let mut body_end = self.end;
            let mut after = self.end;
            let mut line = self.pos;
            while line < self.end {
                let line_end = self.bytes[line..self.end]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(self.end, |n| line + n);
                let mut text = &src[line..line_end];
                if !doc.literal && text.ends_with('\\') {
                    // bash joins such a line before it compares it with the
                    // delimiter, with rules for `\\` this lexer does not model.
                    self.opaque();
                }
                if doc.strip_tabs {
                    text = text.trim_start_matches('\t');
                }
                if text == doc.delimiter {
                    body_end = line;
                    after = (line_end + 1).min(self.end);
                    break;
                }
                line = line_end + 1;
            }
            self.tokens.push(Token::Open {
                frame: Frame::HereDoc,
                arith: false,
                at: body,
            });
            if !doc.literal {
                let outer_end = std::mem::replace(&mut self.end, body_end);
                self.pos = body;
                self.here_document_body();
                self.end = outer_end;
            }
            self.tokens.push(Token::Close);
            self.pos = after;
        }
    }

    /// An unquoted here-document body: expansions are active, quotes are not.
    fn here_document_body(&mut self) {
        let mut word = Builder::default();
        while !self.stop {
            match self.peek() {
                None => return,
                Some(b'\\') => self.escape(&mut word, true),
                Some(b'$') => self.dollar(&mut word, true),
                Some(b'`') => self.backtick(&mut word),
                Some(_) => self.pos += 1,
            }
        }
    }
}
