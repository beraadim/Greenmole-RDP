//! Drain helper stderr independently of command replies, with bounded buffers.
use std::io::{BufRead, Write};

use super::{decode, emit, MAX_RECORD_BYTES};

pub(crate) fn forward_stderr(mut input: impl BufRead, child_pid: u32) {
    let mut output = std::io::stderr();
    while let Ok(Some((line, truncated))) = next_line(&mut input) {
        if !truncated {
            if let Some(record) = decode(&line, child_pid) {
                emit(record);
                continue;
            }
        }
        // Preserve existing owner stderr without treating arbitrary text as a
        // structured event. Even a malformed/oversized line is always drained.
        let _ = output.write_all(&line);
        let _ = output.write_all(if truncated {
            b" [stderr line truncated]\n"
        } else {
            b"\n"
        });
    }
}

fn next_line(input: &mut impl BufRead) -> std::io::Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::new();
    let mut truncated = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty() || truncated).then_some((line, truncated)));
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.unwrap_or(available.len());
        let keep = count.min(MAX_RECORD_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&available[..keep]);
        truncated |= keep != count;
        input.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(Some((line, truncated)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};
    #[test]
    fn oversized_fragmented_line_is_drained_before_the_next_record() {
        let mut bytes = vec![b'x'; MAX_RECORD_BYTES * 3];
        bytes.extend_from_slice(b"\nnext record\n");
        let mut input = BufReader::with_capacity(13, Cursor::new(bytes));
        let (line, truncated) = next_line(&mut input).unwrap().unwrap();
        assert_eq!(line.len(), MAX_RECORD_BYTES);
        assert!(truncated);
        assert_eq!(
            next_line(&mut input).unwrap(),
            Some((b"next record".to_vec(), false))
        );
        assert!(next_line(&mut input).unwrap().is_none());
    }
}
