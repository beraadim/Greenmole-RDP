//! Clipboard format identifiers and text conversion.

#[path = "formats/image.rs"]
mod bounded_image;
pub use self::bounded_image::{
    dib_to_png, png_to_dib, BITMAPINFOHEADER_SIZE, MAX_IMAGE_PAYLOAD_BYTES,
};

/// Registered format ID for Windows "HTML Format"
pub const FORMAT_ID_HTML: u32 = 0xD010;

/// Registered format ID for FileGroupDescriptorW
pub const FORMAT_ID_FILE_LIST: u32 = 0xD011;

/// Convert UTF-8 string to UTF-16LE bytes with null terminator.
pub fn utf8_to_utf16le(s: &str) -> Vec<u8> {
    let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    utf16.iter().flat_map(|&w| w.to_le_bytes()).collect()
}

/// Convert UTF-16LE bytes to UTF-8 string, stripping null terminator.
pub fn utf16le_to_utf8(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return Some(String::new());
    }
    let words: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&words)
        .ok()
        .map(|s| s.trim_end_matches('\0').to_string())
}

/// Map macOS UTI to RDP ClipboardFormat.
/// Returns a tuple of (format_id, optional_name) for now.
/// The exact ClipboardFormat construction depends on ironrdp-cliprdr API.
pub fn uti_to_rdp_format_id(uti: &str) -> Option<u32> {
    match uti {
        "public.utf8-plain-text" | "public.plain-text" => Some(13), // CF_UNICODETEXT
        "public.png" | "public.tiff" | "public.jpeg" => Some(8),    // CF_DIB
        "public.html" => Some(FORMAT_ID_HTML),
        "public.file-url" => Some(FORMAT_ID_FILE_LIST),
        _ => None,
    }
}

/// Map RDP format ID to macOS UTI.
pub fn rdp_format_id_to_uti(format_id: u32) -> Option<&'static str> {
    match format_id {
        13 => Some("public.utf8-plain-text"), // CF_UNICODETEXT
        8 => Some("public.png"),              // CF_DIB
        FORMAT_ID_HTML => Some("public.html"),
        FORMAT_ID_FILE_LIST => Some("public.file-url"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_to_utf16le_ascii() {
        let result = utf8_to_utf16le("hello");
        assert_eq!(result.len(), 12); // 5 chars + null = 6 code units = 12 bytes
        assert_eq!(result[0], 0x68); // 'h'
        assert_eq!(result[1], 0x00);
        assert_eq!(result[10], 0x00); // null terminator
        assert_eq!(result[11], 0x00);
    }

    #[test]
    fn utf8_to_utf16le_chinese() {
        let result = utf8_to_utf16le("你好");
        assert_eq!(result.len(), 6); // 2 chars + null = 3 code units = 6 bytes
        assert_eq!(result[0], 0x60); // '你' = U+4F60 → LE [0x60, 0x4F]
        assert_eq!(result[1], 0x4F);
    }

    #[test]
    fn utf16le_to_utf8_roundtrip() {
        let original = "Hello 世界! 🎉";
        let encoded = utf8_to_utf16le(original);
        let decoded = utf16le_to_utf8(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn utf16le_to_utf8_empty() {
        let result = utf16le_to_utf8(&[]);
        assert_eq!(result, Some(String::new()));
    }

    #[test]
    fn uti_to_rdp_text() {
        assert_eq!(uti_to_rdp_format_id("public.utf8-plain-text"), Some(13));
    }

    #[test]
    fn uti_to_rdp_image() {
        assert_eq!(uti_to_rdp_format_id("public.png"), Some(8));
    }

    #[test]
    fn uti_to_rdp_unknown() {
        assert_eq!(uti_to_rdp_format_id("com.apple.custom"), None);
    }

    #[test]
    fn png_to_dib_and_back() {
        let mut img = image::RgbaImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));

        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let dib = png_to_dib(&png_bytes).unwrap();
        assert!(dib.len() > 40);

        let width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
        let height = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
        assert_eq!(width, 2);
        assert!(height > 0); // Positive = bottom-up

        let png_back = dib_to_png(&dib).unwrap();
        assert!(!png_back.is_empty());

        // Verify roundtrip preserves pixels
        let img_back = image::load_from_memory(&png_back).unwrap().to_rgba8();
        assert_eq!(img_back.get_pixel(0, 0), &image::Rgba([255, 0, 0, 255]));
        assert_eq!(img_back.get_pixel(1, 1), &image::Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn dib_too_small() {
        let result = dib_to_png(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn uti_to_rdp_html() {
        assert_eq!(uti_to_rdp_format_id("public.html"), Some(FORMAT_ID_HTML));
    }

    #[test]
    fn uti_to_rdp_file_url() {
        assert_eq!(
            uti_to_rdp_format_id("public.file-url"),
            Some(FORMAT_ID_FILE_LIST)
        );
    }

    #[test]
    fn rdp_to_uti_html() {
        assert_eq!(rdp_format_id_to_uti(FORMAT_ID_HTML), Some("public.html"));
    }
}
