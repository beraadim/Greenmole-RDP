//! Peer format IDs are connection-local: registered formats are identified by name.

use ironrdp_cliprdr::pdu::{ClipboardFormat, ClipboardFormatId, ClipboardFormatName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatKind {
    Files,
    Html,
    Dib,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RemoteFormat {
    pub id: ClipboardFormatId,
    pub kind: FormatKind,
}

/// Preserve the peer's ID for requests and the semantic kind for decoding responses.
/// A peer may use our locally assigned HTML ID for an entirely different format.
pub(crate) fn preferred_format(
    formats: &[ClipboardFormat],
    file_transfer: bool,
) -> Option<RemoteFormat> {
    for kind in [
        FormatKind::Files,
        FormatKind::Html,
        FormatKind::Dib,
        FormatKind::Text,
    ] {
        if kind == FormatKind::Files && !file_transfer {
            continue;
        }
        let format = formats.iter().find(|format| match kind {
            FormatKind::Files => registered_name_matches(format, ClipboardFormatName::FILE_LIST),
            FormatKind::Html => registered_name_matches(format, ClipboardFormatName::HTML),
            FormatKind::Dib => format.id() == ClipboardFormatId::CF_DIB,
            FormatKind::Text => format.id() == ClipboardFormatId::CF_UNICODETEXT,
        });
        if let Some(format) = format {
            return Some(RemoteFormat {
                id: format.id(),
                kind,
            });
        }
    }
    None
}

/// Rich copies retain the peer's plain text too; no HTML-to-text conversion can
/// reproduce all source application whitespace, table or selection semantics.
pub(crate) fn text_fallback(
    formats: &[ClipboardFormat],
    preferred: Option<RemoteFormat>,
) -> Option<RemoteFormat> {
    if preferred?.kind != FormatKind::Html {
        return None;
    }
    formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        .then_some(RemoteFormat {
            id: ClipboardFormatId::CF_UNICODETEXT,
            kind: FormatKind::Text,
        })
}

fn registered_name_matches(format: &ClipboardFormat, expected: ClipboardFormatName) -> bool {
    // Standard IDs have fixed meanings and cannot be redefined by a format name.
    (0xc000..=0xffff).contains(&format.id().value())
        && format
            .name()
            .is_some_and(|name| name.value().eq_ignore_ascii_case(expected.value()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{FORMAT_ID_FILE_LIST, FORMAT_ID_HTML};

    fn named(id: u32, name: ClipboardFormatName) -> ClipboardFormat {
        ClipboardFormat::new(ClipboardFormatId::new(id)).with_name(name)
    }

    #[test]
    fn uses_peer_ids_and_case_insensitive_names_for_registered_formats() {
        let formats = [
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
            ClipboardFormat::new(ClipboardFormatId::CF_DIB),
            named(0xc137, ClipboardFormatName::new_static("html format")),
            named(
                0xc245,
                ClipboardFormatName::new_static("FILEGROUPDESCRIPTORW"),
            ),
        ];
        assert_eq!(
            preferred_format(&formats, true),
            Some(RemoteFormat {
                id: ClipboardFormatId::new(0xc245),
                kind: FormatKind::Files,
            })
        );
        assert_eq!(
            preferred_format(&formats, false),
            Some(RemoteFormat {
                id: ClipboardFormatId::new(0xc137),
                kind: FormatKind::Html,
            })
        );
    }

    #[test]
    fn local_id_collisions_and_unnamed_registered_formats_are_not_trusted() {
        let formats = [
            named(
                FORMAT_ID_HTML,
                ClipboardFormatName::new_static("Unrelated Format"),
            ),
            ClipboardFormat::new(ClipboardFormatId::new(FORMAT_ID_FILE_LIST)),
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ];
        assert_eq!(
            preferred_format(&formats, true).unwrap().kind,
            FormatKind::Text
        );
        assert_eq!(preferred_format(&formats[..2], true), None);
        // Even our two registered IDs can be reversed by another endpoint.
        let swapped = [named(FORMAT_ID_HTML, ClipboardFormatName::FILE_LIST)];
        assert_eq!(
            preferred_format(&swapped, true).unwrap().kind,
            FormatKind::Files
        );
    }

    #[test]
    fn standard_formats_keep_their_defined_meanings_and_preference() {
        let formats = [
            named(13, ClipboardFormatName::FILE_LIST),
            ClipboardFormat::new(ClipboardFormatId::CF_DIB),
        ];
        assert_eq!(
            preferred_format(&formats, true).unwrap().kind,
            FormatKind::Dib
        );
        assert_eq!(
            preferred_format(&formats[..1], true).unwrap().kind,
            FormatKind::Text
        );
        assert_eq!(preferred_format(&[], true), None);
    }
}
