//! Splits a server-sent-events byte stream into event blocks.

use hermes_core::AgentError;

/// The event blocks of a streaming response, read chunk by chunk.
pub(crate) struct SseEvents {
    response: reqwest::Response,
    buffer: SseBuffer,
}

impl SseEvents {
    /// Starts reading the response's events, or returns its error status
    /// with the body text.
    pub(crate) async fn open(response: reqwest::Response) -> Result<Self, AgentError> {
        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(AgentError::LlmApi(format!(
                "API error {status}: {body_text}"
            )));
        }
        Ok(Self {
            response,
            buffer: SseBuffer::default(),
        })
    }

    /// The next complete event block; `None` once the body has ended.
    pub(crate) async fn next(&mut self) -> Option<Result<String, AgentError>> {
        loop {
            if let Some(event) = self.buffer.next_event() {
                return Some(Ok(event));
            }
            match self.response.chunk().await {
                Ok(Some(chunk)) => self.buffer.push(&chunk),
                Ok(None) => return None,
                Err(e) => return Some(Err(AgentError::LlmApi(format!("Stream read error: {e}")))),
            }
        }
    }

    /// What followed the last complete event, for a stream that ended
    /// without a blank line.
    pub(crate) fn rest(&self) -> String {
        self.buffer.rest()
    }
}

/// Collects SSE bytes and hands out each complete event block. Bytes are
/// decoded only once a whole block has arrived, so a character split across
/// network chunks stays intact, and consumed bytes are dropped once per push
/// rather than copied once per event.
#[derive(Debug, Default)]
struct SseBuffer {
    bytes: Vec<u8>,
    /// Start of the bytes not yet handed out.
    start: usize,
    /// How many of those bytes are known to hold no event end.
    searched: usize,
}

impl SseBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.drain(..self.start);
        self.start = 0;
        self.bytes.extend_from_slice(chunk);
    }

    /// The next complete event block, without the blank line that ends it.
    fn next_event(&mut self) -> Option<String> {
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
    fn rest(&self) -> String {
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

    /// Answers one request with `raw` and closes the connection.
    async fn serve(raw: &'static [u8]) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 65536];
            let _ = socket.read(&mut request).await;
            socket.write_all(raw).await.unwrap();
        });
        reqwest::get(format!("http://{addr}/")).await.unwrap()
    }

    #[tokio::test]
    async fn events_come_in_order_and_the_rest_is_kept() {
        let response =
            serve(b"HTTP/1.1 200 OK\r\ncontent-length: 25\r\n\r\ndata: 1\n\ndata: 2\n\ndata: 3")
                .await;
        let mut events = SseEvents::open(response).await.unwrap();
        assert_eq!(events.next().await.unwrap().unwrap(), "data: 1");
        assert_eq!(events.next().await.unwrap().unwrap(), "data: 2");
        assert!(events.next().await.is_none());
        assert_eq!(events.rest(), "data: 3");
    }

    #[tokio::test]
    async fn an_error_status_is_reported_with_its_body() {
        let response =
            serve(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\n\r\nbusy").await;
        let Err(error) = SseEvents::open(response).await else {
            panic!("a 503 response opened as an event stream");
        };
        assert_eq!(
            error.to_string(),
            AgentError::LlmApi("API error 503 Service Unavailable: busy".into()).to_string()
        );
    }

    #[tokio::test]
    async fn a_body_cut_off_mid_chunk_is_a_stream_read_error() {
        let response = serve(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n9\r\ndata: 1\n\n\r\n20\r\ndata",
        )
        .await;
        let mut events = SseEvents::open(response).await.unwrap();
        assert_eq!(events.next().await.unwrap().unwrap(), "data: 1");
        let error = events.next().await.unwrap().unwrap_err();
        assert!(error.to_string().contains("Stream read error"), "{error}");
    }
}
