//! Windows CF_HTML envelopes. Offsets describe UTF-8 bytes, not characters;
//! parse only the bounded description header, never header-like HTML content.

const HTML_PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
const HTML_SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";
const HEADER_TEMPLATE: &str = "Version:0.9\r\n\
    StartHTML:0000000000\r\n\
    EndHTML:0000000000\r\n\
    StartFragment:0000000000\r\n\
    EndFragment:0000000000\r\n";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HTML_PAYLOAD_BYTES: usize = crate::text::MAX_TEXT_PAYLOAD_BYTES;
pub(crate) const MAX_HTML_FRAGMENT_BYTES: usize =
    MAX_HTML_PAYLOAD_BYTES - HEADER_TEMPLATE.len() - HTML_PREFIX.len() - HTML_SUFFIX.len();

/// Wrap a bounded fragment with 10-digit byte offsets. Reject before allocating
/// the output; a valid response leaves room in the shared event queue budget.
pub fn wrap_html_format(html: &str) -> Option<Vec<u8>> {
    if html.len() > MAX_HTML_FRAGMENT_BYTES {
        return None;
    }
    let start_html = HEADER_TEMPLATE.len();
    let start_fragment = start_html + HTML_PREFIX.len();
    let end_fragment = start_fragment + html.len();
    let end_html = end_fragment + HTML_SUFFIX.len();

    let header = format!(
        "Version:0.9\r\n\
         StartHTML:{start_html:010}\r\n\
         EndHTML:{end_html:010}\r\n\
         StartFragment:{start_fragment:010}\r\n\
         EndFragment:{end_fragment:010}\r\n"
    );

    let mut result = Vec::with_capacity(end_html);
    result.extend_from_slice(header.as_bytes());
    result.extend_from_slice(HTML_PREFIX.as_bytes());
    result.extend_from_slice(html.as_bytes());
    result.extend_from_slice(HTML_SUFFIX.as_bytes());
    Some(result)
}

/// Extract a fragment only after checking the header boundary, context bounds
/// and UTF-8 character boundaries. CF_HTML permits CR, LF and CRLF separators,
/// and uses paired -1 context offsets when only a fragment is provided.
pub fn unwrap_html_format(data: &[u8]) -> Option<String> {
    if data.len() > MAX_HTML_PAYLOAD_BYTES {
        return None;
    }
    let mut cursor = 0;
    let mut start_html: Option<i64> = None;
    let mut end_html = None;
    let mut start_fragment = None;
    let mut end_fragment = None;
    let mut version_seen = false;

    loop {
        let boundary = start_html
            .filter(|offset| *offset >= 0)
            .or(start_fragment)
            .and_then(|offset| usize::try_from(offset).ok());
        if boundary.is_some_and(|boundary| cursor >= boundary) {
            break;
        }
        let limit = data
            .len()
            .min(MAX_HEADER_BYTES)
            .min(boundary.unwrap_or(usize::MAX));
        let remaining = data.get(cursor..limit)?;
        if !remaining.first().is_some_and(u8::is_ascii_alphabetic) {
            break;
        }
        let end = remaining
            .iter()
            .position(|byte| matches!(*byte, b'\r' | b'\n'))?;
        let line = std::str::from_utf8(&remaining[..end]).ok()?;
        let Some((key, value)) = line.split_once(':') else {
            // With no context, a fragment marker can precede StartFragment.
            // It is content, and must never be scanned for more offset fields.
            break;
        };
        if !key.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            break;
        }
        let value = value.trim_matches([' ', '\t']);
        if !version_seen {
            if key != "Version" || !matches!(value, "0.9" | "1.0") {
                return None;
            }
            version_seen = true;
        } else {
            let destination = match key {
                "StartHTML" => Some(&mut start_html),
                "EndHTML" => Some(&mut end_html),
                "StartFragment" => Some(&mut start_fragment),
                "EndFragment" => Some(&mut end_fragment),
                "Version" => return None,
                _ => None, // SourceURL and optional selection fields are metadata.
            };
            if let Some(destination) = destination {
                if destination.is_some() {
                    return None;
                }
                *destination = Some(parse_offset(value)?);
            }
        }
        cursor += end + 1;
        if remaining[end] == b'\r' && data.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }
    }

    let start = usize::try_from(start_fragment?).ok()?;
    let end = usize::try_from(end_fragment?).ok()?;
    let (context_start, context_end) = match (start_html?, end_html?) {
        (-1, -1) => (cursor, data.len()),
        (start, end) if start >= 0 && end >= 0 => {
            (usize::try_from(start).ok()?, usize::try_from(end).ok()?)
        }
        _ => return None,
    };
    if !version_seen
        || context_start < cursor
        || context_start > MAX_HEADER_BYTES
        || start < context_start
        || start > end
        || end > context_end
        || context_end > data.len()
        || end - start > MAX_HTML_FRAGMENT_BYTES
    {
        return None;
    }
    // Checking the complete context also rejects offsets into a UTF-8 sequence.
    let context = std::str::from_utf8(data.get(context_start..context_end)?).ok()?;
    Some(
        context
            .get(start - context_start..end - context_start)?
            .to_owned(),
    )
}

fn parse_offset(value: &str) -> Option<i64> {
    if value == "-1" {
        return Some(-1);
    }
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(fragment: &str, separator: &str, context: bool, extra_header: &str) -> Vec<u8> {
        let header = format!(
            "Version:1.0{separator}StartHTML:0000000000{separator}EndHTML:0000000000{separator}StartFragment:0000000000{separator}EndFragment:0000000000{separator}{extra_header}"
        );
        let start = header.len();
        let end = start + fragment.len();
        let start_html = if context {
            format!("{start:010}")
        } else {
            "-1".to_owned()
        };
        let end_html = if context {
            format!("{end:010}")
        } else {
            "-1".to_owned()
        };
        // Keep the fields fixed width even for -1 so replacement preserves offsets.
        let header = header
            .replace(
                "StartHTML:0000000000",
                &format!("StartHTML:{start_html:>10}"),
            )
            .replace("EndHTML:0000000000", &format!("EndHTML:{end_html:>10}"))
            .replace(
                "StartFragment:0000000000",
                &format!("StartFragment:{start:010}"),
            )
            .replace("EndFragment:0000000000", &format!("EndFragment:{end:010}"));
        [header.as_bytes(), fragment.as_bytes()].concat()
    }

    #[test]
    fn header_like_body_content_cannot_override_offsets() {
        for html in [
            "<pre>\nStartFragment:0\nEndFragment:7\n</pre>",
            "<pre>\nStartFragment:not a number\n</pre>",
        ] {
            let wrapped = wrap_html_format(html).unwrap();
            assert_eq!(unwrap_html_format(&wrapped).as_deref(), Some(html));
        }
    }

    #[test]
    fn accepts_all_header_line_endings_with_and_without_context() {
        for separator in ["\r", "\n", "\r\n"] {
            for context in [false, true] {
                let html = "<b>Blåbær 東京 😀</b>";
                let extra = format!("SourceURL:https://example.test/{separator}");
                assert_eq!(
                    unwrap_html_format(&envelope(html, separator, context, &extra)).as_deref(),
                    Some(html)
                );
            }
        }
    }

    #[test]
    fn accepts_fragment_markers_without_context_and_without_a_body_newline() {
        let wrapped = wrap_html_format("<b>hello</b>").unwrap();
        let wrapped = String::from_utf8(wrapped).unwrap();
        let mut output = Vec::new();
        for line in wrapped.split_inclusive('\n') {
            if line.starts_with("StartHTML:") || line.starts_with("EndHTML:") {
                let (key, _) = line.split_once(':').unwrap();
                output.extend_from_slice(format!("{key}:{:>10}\r\n", -1).as_bytes());
            } else {
                output.extend_from_slice(line.as_bytes());
            }
        }
        assert_eq!(unwrap_html_format(&output).as_deref(), Some("<b>hello</b>"));
    }

    #[test]
    fn rejects_duplicate_fields_and_offsets_outside_context_or_character_boundaries() {
        assert!(
            unwrap_html_format(&envelope("<b>test</b>", "\n", true, "StartFragment:0\n")).is_none()
        );
        let valid = String::from_utf8(envelope("é", "\n", true, "")).unwrap();
        let start = valid.len() - "é".len();
        for invalid in [
            valid.replace(
                &format!("StartFragment:{start:010}"),
                &format!("StartFragment:{:010}", start + 1),
            ),
            valid.replace(
                &format!("StartFragment:{start:010}"),
                "StartFragment:0000000000",
            ),
            valid.replace(
                &format!("EndFragment:{:010}", valid.len()),
                "EndFragment:9999999999",
            ),
            valid.replace("StartHTML:", "StartHTML:-"),
        ] {
            assert!(unwrap_html_format(invalid.as_bytes()).is_none());
        }
    }

    #[test]
    fn bounds_header_and_payload_before_copying() {
        let large_header = format!("SourceURL:{}\n", "x".repeat(MAX_HEADER_BYTES));
        assert!(unwrap_html_format(&envelope("x", "\n", true, &large_header)).is_none());
        assert!(unwrap_html_format(&vec![b'x'; MAX_HTML_PAYLOAD_BYTES + 1]).is_none());
        assert!(wrap_html_format(&"x".repeat(MAX_HTML_FRAGMENT_BYTES + 1)).is_none());
        let largest = "x".repeat(MAX_HTML_FRAGMENT_BYTES);
        let wrapped = wrap_html_format(&largest).unwrap();
        assert_eq!(wrapped.len(), MAX_HTML_PAYLOAD_BYTES);
        assert_eq!(
            unwrap_html_format(&wrapped).as_deref(),
            Some(largest.as_str())
        );
    }

    #[test]
    fn malformed_missing_or_truncated_headers_are_rejected() {
        for value in [
            b"not HTML".as_slice(),
            b"Version:1.0\r",
            b"Version:1.0\nStartHTML:999999999999999999999\n",
            b"Version:1.0\nStartFragment:0\nEndFragment:0\n",
        ] {
            assert!(unwrap_html_format(value).is_none());
        }
    }

    #[test]
    fn roundtrip_simple() {
        let html = "<b>Hello</b>";
        let wrapped = wrap_html_format(html).unwrap();
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, html);
    }

    #[test]
    fn roundtrip_unicode() {
        let html = "<p>你好世界 🎉</p>";
        let wrapped = wrap_html_format(html).unwrap();
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, html);
    }

    #[test]
    fn roundtrip_empty() {
        let wrapped = wrap_html_format("").unwrap();
        let unwrapped = unwrap_html_format(&wrapped).unwrap();
        assert_eq!(unwrapped, "");
    }

    #[test]
    fn header_has_version() {
        let wrapped = wrap_html_format("<b>test</b>").unwrap();
        let header = std::str::from_utf8(&wrapped).unwrap();
        assert!(header.starts_with("Version:0.9\r\n"));
    }

    #[test]
    fn offsets_are_10_digits() {
        let wrapped = wrap_html_format("<b>test</b>").unwrap();
        let text = std::str::from_utf8(&wrapped).unwrap();
        for line in text.lines() {
            if let Some(val) = line.strip_prefix("StartHTML:") {
                assert_eq!(val.len(), 10, "offset should be 10 digits, got: {val}");
            }
        }
    }

    #[test]
    fn unwrap_invalid_returns_none() {
        assert_eq!(unwrap_html_format(b"not html format"), None);
    }
}
