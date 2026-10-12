//! Safe Rust wrapper around macOS NSPasteboard (general pasteboard).
//!
//! objc2 0.6 API notes used here:
//! - `NSPasteboard::generalPasteboard()` returns `Retained<NSPasteboard>` (safe)
//! - `NSPasteboard::types()` returns `Option<Retained<NSArray<NSPasteboardType>>>`
//! - `NSPasteboard::clearContents()` returns `NSInteger` (new change_count)
//! - `NSPasteboard::stringForType()` returns `Option<Retained<NSString>>`
//! - `NSPasteboard::dataForType()` returns `Option<Retained<NSData>>`
//! - `NSPasteboard::setString_forType()` / `setData_forType()` return `bool`
//! - `NSData::with_bytes()` creates NSData from a &[u8]
//! - `NSData::to_vec()` copies bytes into a Vec<u8>

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString, NSUTF8StringEncoding};
use tracing::{debug, warn};

/// Stateless wrapper around the general pasteboard. Objective-C handles are
/// obtained within each call and are never retained across threads.
#[derive(Clone)]
pub struct PasteboardBridge {
    name: Option<String>,
}

impl PasteboardBridge {
    /// Create a new `PasteboardBridge`.
    ///
    /// This does not allocate any ObjC objects; `self.board()`
    /// is called lazily on each method invocation, always returning the same
    /// system-wide singleton.
    pub fn new() -> Self {
        Self { name: None }
    }

    fn board(&self) -> Retained<NSPasteboard> {
        match &self.name {
            Some(name) => NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
            None => NSPasteboard::generalPasteboard(),
        }
    }

    /// Tests use private pasteboards so they cannot replace the user's clipboard.
    #[cfg(test)]
    pub(crate) fn isolated() -> Self {
        let board = NSPasteboard::pasteboardWithUniqueName();
        Self {
            name: Some(board.name().to_string()),
        }
    }

    /// Return the current pasteboard change count.
    ///
    /// Callers can poll this to detect clipboard changes without reading content.
    pub fn change_count(&self) -> i64 {
        let pb = self.board();
        pb.changeCount() as i64
    }

    /// Return all declared type UTI strings on the pasteboard.
    pub fn available_types(&self) -> Vec<String> {
        let pb = self.board();
        match pb.types() {
            None => Vec::new(),
            Some(arr) => {
                let count = arr.count();
                let mut result = Vec::with_capacity(count);
                for i in 0..count {
                    let ns_str = arr.objectAtIndex(i);
                    result.push(ns_str.to_string());
                }
                result
            }
        }
    }

    /// Read plain-text string from the pasteboard, if available.
    pub fn read_string(&self) -> Option<String> {
        let pb = self.board();
        // SAFETY: NSPasteboardTypeString is a valid static reference.
        let type_str: &NSString = unsafe { NSPasteboardTypeString };
        match pb.stringForType(type_str) {
            None => {
                debug!("pasteboard: no string data");
                None
            }
            Some(ns_str) => {
                // Check native lengths before allocating the Rust UTF-8 copy or
                // the later UTF-16 response (which includes a NUL terminator).
                let units = ns_str.length();
                if !crate::text::valid_unicode_lengths(units, 0)
                    || !crate::text::valid_unicode_lengths(
                        units,
                        ns_str.lengthOfBytesUsingEncoding(NSUTF8StringEncoding),
                    )
                {
                    warn!("pasteboard: text exceeds clipboard limit");
                    return None;
                }
                Some(ns_str.to_string())
            }
        }
    }

    /// Read encoded PNG/TIFF/JPEG bytes. The converter validates dimensions and
    /// decoding budgets; check NSData length here before creating a Rust copy.
    pub fn read_image(&self) -> Option<Vec<u8>> {
        let pb = self.board();
        for uti in ["public.png", "public.tiff", "public.jpeg"] {
            let image_type = NSString::from_str(uti);
            if let Some(data) = pb.dataForType(&image_type) {
                if data.length() > crate::formats::MAX_IMAGE_PAYLOAD_BYTES {
                    warn!("pasteboard: image payload exceeds clipboard limit");
                    continue;
                }
                return Some(data.to_vec());
            }
        }
        None
    }

    /// Read file URLs from the pasteboard.
    ///
    /// Iterates all pasteboard items to support multi-file copies.
    /// Falls back to a single `stringForType` read if no items are found.
    pub fn read_file_urls(&self) -> Vec<PathBuf> {
        crate::file_urls::read_file_urls(&self.board())
    }

    /// Read HTML content from the pasteboard (public.html UTI).
    pub fn read_html(&self) -> Option<String> {
        let pb = self.board();
        let html_type = NSString::from_str("public.html");
        match pb.stringForType(&html_type) {
            None => {
                debug!("pasteboard: no HTML data");
                None
            }
            Some(ns_str) => {
                let limit = crate::html::MAX_HTML_FRAGMENT_BYTES;
                // UTF-8 cannot be shorter than the UTF-16 unit count. The cheap
                // first check also bounds work needed to measure UTF-8 length.
                if ns_str.length() > limit
                    || ns_str.lengthOfBytesUsingEncoding(NSUTF8StringEncoding) > limit
                {
                    warn!("pasteboard: HTML exceeds clipboard limit");
                    return None;
                }
                Some(ns_str.to_string())
            }
        }
    }

    /// Write HTML content to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_html(&self, html: &str) -> i64 {
        self.write_rich_text(html, None)
    }

    /// Prepare both flavors in one item before replacing the clipboard. The
    /// native write publishes them together, so clients never observe HTML with
    /// a plain-text flavor belonging to a different copy.
    pub(crate) fn write_rich_text(&self, html: &str, text: Option<&str>) -> i64 {
        let item = NSPasteboardItem::new();
        if !item.setString_forType(
            &NSString::from_str(html),
            &NSString::from_str("public.html"),
        ) {
            warn!("pasteboard: preparing HTML flavor failed");
            return -1;
        }
        if let Some(text) = text {
            if !item.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
            {
                warn!("pasteboard: preparing plain-text flavor failed");
                return -1;
            }
        }
        let objects = NSArray::from_retained_slice(&[
            ProtocolObject::<dyn NSPasteboardWriting>::from_retained(item),
        ]);
        let pb = self.board();
        pb.clearContents();
        if !pb.writeObjects(&objects) {
            warn!("pasteboard: publishing rich text failed");
            return -1;
        }
        pb.changeCount() as i64
    }

    /// Write file URLs to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_file_urls(&self, paths: &[PathBuf]) -> i64 {
        crate::file_urls::write_file_urls(&self.board(), paths)
    }

    /// Write a plain-text string to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_string(&self, text: &str) -> i64 {
        let pb = self.board();
        let change_count = pb.clearContents();

        let ns_str = NSString::from_str(text);
        let type_str: &NSString = unsafe { NSPasteboardTypeString };

        if !pb.setString_forType(&ns_str, type_str) {
            warn!("pasteboard: setString:forType: returned false");
            return -1;
        }

        change_count as i64
    }

    /// Write PNG image data to the pasteboard.
    ///
    /// Returns the new change count, or -1 on failure.
    pub fn write_image(&self, png_data: &[u8]) -> i64 {
        let pb = self.board();
        let change_count = pb.clearContents();

        let ns_data = NSData::with_bytes(png_data);
        let png_type: &NSString = unsafe { NSPasteboardTypePNG };

        if !pb.setData_forType(Some(&ns_data), png_type) {
            warn!("pasteboard: setData:forType: returned false");
            return -1;
        }

        change_count as i64
    }

    /// Clear the pasteboard contents.
    ///
    /// Returns the new change count.
    pub fn clear(&self) -> i64 {
        let pb = self.board();
        pb.clearContents() as i64
    }
}

impl Default for PasteboardBridge {
    fn default() -> Self {
        Self::new()
    }
}
