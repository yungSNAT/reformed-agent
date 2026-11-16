//! Splits a server-sent-events byte stream into event blocks.

/// Collects SSE bytes and hands out each complete event block. Bytes are
/// decoded only once a whole block has arrived, so a character split across
/// network chunks stays intact, and consumed bytes are dropped once per push
/// rather than copied once per event.
#[derive(Debug, Default)]
pub(crate) struct SseBuffer {
    bytes: Vec<u8>,
    /// Start of the bytes not yet handed out.
    start: usize,
    /// How many of those bytes are known to hold no event end.
    searched: usize,
}

impl SseBuffer {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.bytes.drain(..self.start);
        self.start = 0;
        self.bytes.extend_from_slice(chunk);
    }

    /// The next complete event block, without the blank line that ends it.
    pub(crate) fn next_event(&mut self) -> Option<String> {
        let pending = &self.bytes[self.start..];
        // Back up one byte: the first newline of a pair may end the searched part.
        let from = self.searched.saturating_sub(1);
        let Some(end) = pending[from..]
            .windows(2)
            .position(|pair| pair == b"\n\n")
            .map(|offset| from + offset)
        else {
            self.searched = pending.len();
            return None;
        };
        let event = String::from_utf8_lossy(&pending[..end]).into_owned();
        self.start += end + 2;
        self.searched = 0;
        Some(event)
    }

    /// What followed the last complete event, for a stream that ended
    /// without a blank line.
    pub(crate) fn rest(&self) -> String {
        String::from_utf8_lossy(&self.bytes[self.start..]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_split_across_chunks_stays_intact() {
        let event = "data: {\"content\":\"é😀\"}\n\n".as_bytes();
        // Split inside the 2-byte é, then inside the 4-byte emoji.
        let e_at = event.iter().position(|b| *b == 0xC3).unwrap();
        let emoji_at = event.iter().position(|b| *b == 0xF0).unwrap();
        let mut buffer = SseBuffer::default();
        buffer.push(&event[..e_at + 1]);
        assert_eq!(buffer.next_event(), None);
        buffer.push(&event[e_at + 1..emoji_at + 2]);
        assert_eq!(buffer.next_event(), None);
        buffer.push(&event[emoji_at + 2..]);
        assert_eq!(
            buffer.next_event().as_deref(),
            Some("data: {\"content\":\"é😀\"}")
        );
        assert_eq!(buffer.next_event(), None);
    }

    #[test]
    fn events_split_at_blank_lines_and_the_rest_is_kept() {
        let mut buffer = SseBuffer::default();
        buffer.push(b"data: 1\n\nevent: x\ndata: 2\n");
        assert_eq!(buffer.next_event().as_deref(), Some("data: 1"));
        assert_eq!(buffer.next_event(), None);
        // The blank line's second newline arrives in the next chunk.
        buffer.push(b"\ndata: 3");
        assert_eq!(buffer.next_event().as_deref(), Some("event: x\ndata: 2"));
        assert_eq!(buffer.next_event(), None);
        assert_eq!(buffer.rest(), "data: 3");
    }
}
