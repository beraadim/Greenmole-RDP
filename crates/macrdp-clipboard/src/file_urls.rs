//! File URL conversion and multi-item pasteboard publication.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSString, NSURL};

use crate::file::MAX_CLIPBOARD_FILES;

// Allow percent-encoded Unicode paths while bounding allocations from other apps.
const MAX_FILE_URL_BYTES: usize = 32 * 1024;

pub(crate) fn read_file_urls(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    // SAFETY: AppKit's constant is a valid static NSString reference.
    let file_type = unsafe { NSPasteboardTypeFileURL };
    let mut paths = Vec::new();
    if let Some(items) = pasteboard.pasteboardItems() {
        if items.count() > MAX_CLIPBOARD_FILES {
            tracing::warn!(count = items.count(), "Clipboard file count exceeds limit");
            return paths;
        }
        for item in items.iter() {
            if let Some(value) = item.stringForType(file_type) {
                if let Some(path) = pasteboard_string_to_path(&value) {
                    paths.push(path);
                }
            }
        }
    }
    if paths.is_empty() {
        if let Some(value) = pasteboard.stringForType(file_type) {
            if let Some(path) = pasteboard_string_to_path(&value) {
                paths.push(path);
            }
        }
    }
    paths
}

fn pasteboard_string_to_path(value: &NSString) -> Option<PathBuf> {
    // NSString length is UTF-16 units; check before making a Rust allocation too.
    if value.length() > MAX_FILE_URL_BYTES {
        return None;
    }
    file_url_to_path(&value.to_string())
}

fn file_url_to_path(value: &str) -> Option<PathBuf> {
    // NSURL may preserve %00 literally instead of reporting a decoded NUL.
    // Reject it before parsing so the interpretation cannot vary by consumer.
    // A real filename containing "%00" has "%2500" in its URL and remains valid.
    if value.len() > MAX_FILE_URL_BYTES || value.contains('\0') || value.contains("%00") {
        return None;
    }
    let url = NSURL::URLWithString(&NSString::from_str(value))?;
    if !url.isFileURL()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.user().is_some()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    // A network authority must never be mistaken for a local absolute path.
    if url.host().is_some_and(|host| {
        let host = host.to_string();
        !host.is_empty() && !host.eq_ignore_ascii_case("localhost")
    }) {
        return None;
    }
    let path = url.path()?.to_string();
    if path.contains('\0') || !Path::new(&path).is_absolute() {
        return None;
    }
    Some(PathBuf::from(path))
}

fn path_to_file_url(path: &Path) -> Option<Retained<NSURL>> {
    let value = path.to_str()?;
    if !path.is_absolute() || value.len() > MAX_FILE_URL_BYTES || value.contains('\0') {
        return None;
    }
    // NSURL escapes %, #, spaces and UTF-8 correctly; string concatenation cannot.
    let url = NSURL::fileURLWithPath(&NSString::from_str(value));
    if url.absoluteString()?.length() > MAX_FILE_URL_BYTES {
        return None;
    }
    Some(url)
}

/// Prepare the whole batch before replacing the clipboard, then publish one NSURL
/// per item so Finder can paste every file. Invalid batches leave existing data intact.
pub(crate) fn write_file_urls(pasteboard: &NSPasteboard, paths: &[PathBuf]) -> i64 {
    if paths.is_empty() || paths.len() > MAX_CLIPBOARD_FILES {
        tracing::warn!(count = paths.len(), "Rejected clipboard file batch size");
        return -1;
    }
    let Some(urls): Option<Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>>> = paths
        .iter()
        .map(|path| path_to_file_url(path).map(ProtocolObject::from_retained))
        .collect()
    else {
        tracing::warn!("Rejected invalid clipboard file path");
        return -1;
    };
    let objects = NSArray::from_retained_slice(&urls);
    pasteboard.clearContents();
    if !pasteboard.writeObjects(&objects) {
        tracing::warn!("Failed to publish clipboard file URLs");
        return -1;
    }
    pasteboard.changeCount() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_urls_round_trip_unicode_spaces_percent_hash_and_question_mark() {
        let directory = tempfile::tempdir().unwrap();
        for name in [
            "plain.txt",
            "Blåbær 東京 😀.txt",
            "a #1? 100%+done.txt",
            "literal%20name.txt",
            "literal%00name.txt",
        ] {
            let path = directory.path().join(name);
            std::fs::write(&path, name.as_bytes()).unwrap();
            let url = path_to_file_url(&path)
                .unwrap()
                .absoluteString()
                .unwrap()
                .to_string();
            // Native file URLs normalize Unicode on macOS. Verify they still
            // resolve to the same actual file rather than comparing UTF-8 bytes.
            let decoded = file_url_to_path(&url).unwrap();
            assert_eq!(std::fs::read(decoded).unwrap(), name.as_bytes(), "{url}");
            assert!(!url.contains('#') && !url.contains('?') && !url.contains(' '));
        }
        let base = path_to_file_url(directory.path())
            .unwrap()
            .absoluteString()
            .unwrap()
            .to_string();
        std::fs::write(directory.path().join("Blåbær.txt"), b"encoded Unicode").unwrap();
        let encoded = format!("{}/Bl%C3%A5b%C3%A6r.txt", base.trim_end_matches('/'));
        assert_eq!(
            std::fs::read(file_url_to_path(&encoded).unwrap()).unwrap(),
            b"encoded Unicode"
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a%20b.txt"),
            Some(PathBuf::from("/tmp/a b.txt"))
        );
    }

    #[test]
    fn rejects_nonlocal_relative_ambiguous_or_oversized_urls() {
        for value in [
            "https://example.test/a",
            "file://remote.test/tmp/a",
            "file:relative.txt",
            "file:///tmp/a?query",
            "file:///tmp/a#fragment",
            "file:///tmp/a%00b",
            "file://user:pass@localhost/tmp/a",
            "file://localhost:123/tmp/a",
        ] {
            assert_eq!(file_url_to_path(value), None, "{value}");
        }
        assert!(path_to_file_url(Path::new("relative.txt")).is_none());
        assert!(path_to_file_url(Path::new("/tmp/a\0b")).is_none());
        assert!(file_url_to_path(&format!("file:///{}", "a".repeat(MAX_FILE_URL_BYTES))).is_none());
    }

    #[test]
    fn publishes_every_file_as_an_item_without_touching_the_general_pasteboard() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let directory = tempfile::tempdir().unwrap();
        let paths = vec![
            directory.path().join("Blåbær 東京.txt"),
            directory.path().join("a #1? 100%.txt"),
        ];
        for (index, path) in paths.iter().enumerate() {
            std::fs::write(path, [index as u8]).unwrap();
        }
        let count = write_file_urls(&board, &paths);
        assert!(count >= 0);
        assert_eq!(count, board.changeCount() as i64);
        assert_eq!(board.pasteboardItems().unwrap().count(), 2);
        let decoded = read_file_urls(&board);
        assert_eq!(decoded.len(), paths.len());
        for (index, path) in decoded.iter().enumerate() {
            assert_eq!(std::fs::read(path).unwrap(), [index as u8]);
        }
        board.clearContents();
    }

    #[test]
    fn invalid_batches_do_not_clear_existing_pasteboard_items() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let paths = vec![PathBuf::from("/tmp/existing.txt")];
        let count = write_file_urls(&board, &paths);
        assert!(count >= 0);
        for batch in [
            vec![],
            vec![PathBuf::from("relative.txt")],
            vec![paths[0].clone(); MAX_CLIPBOARD_FILES + 1],
        ] {
            assert_eq!(write_file_urls(&board, &batch), -1);
            assert_eq!(board.changeCount() as i64, count);
            assert_eq!(read_file_urls(&board), paths);
        }
        board.clearContents();
    }
}
