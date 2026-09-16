//! Buffers arbitrary byte chunks from a streaming HTTP response into
//! complete lines, since a single SSE/NDJSON line can be split across
//! multiple TCP-level reads. Pure and synchronous -- the actual byte
//! stream comes from `reqwest` in each provider's glue code
//! (`providers::http_stream`).

#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: String,
}

impl LineBuffer {
    pub fn new() -> Self { Self::default() }

    /// Feeds in a new chunk of text and returns every complete line it
    /// now contains, in order. Any trailing partial line is kept for
    /// the next call.
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.pending.push_str(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.pending.find('\n') {
            let line = self.pending[..pos].trim_end_matches('\r').to_string();
            lines.push(line);
            self.pending.drain(..=pos);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chunk_with_multiple_complete_lines() {
        let mut buf = LineBuffer::new();
        assert_eq!(buf.push("a\nb\nc\n"), vec!["a", "b", "c"]);
    }

    #[test]
    fn line_split_across_two_chunks_is_reassembled() {
        let mut buf = LineBuffer::new();
        assert!(buf.push("data: {\"partial").is_empty());
        assert_eq!(buf.push("\"}\n"), vec!["data: {\"partial\"}"]);
    }

    #[test]
    fn trailing_partial_line_is_held_until_completed() {
        let mut buf = LineBuffer::new();
        assert_eq!(buf.push("complete\nincomplete"), vec!["complete"]);
        assert_eq!(buf.push(" now done\n"), vec!["incomplete now done"]);
    }

    #[test]
    fn strips_trailing_carriage_return() {
        let mut buf = LineBuffer::new();
        assert_eq!(buf.push("line\r\n"), vec!["line"]);
    }
}
