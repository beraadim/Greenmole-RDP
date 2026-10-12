//! Read immutable cursor snapshots directly, caching unchanged native images.

use crate::CursorShape;
use anyhow::{Context, Result};
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use image::{imageops::FilterType, RgbaImage};
use objc2_app_kit::NSCursor;
use objc2_foundation::NSPoint;
use std::ffi::c_void;

const MAX_RDP_CURSOR_EDGE: u16 = 32;
const MAX_NATIVE_CURSOR_EDGE: usize = 1024;

pub enum CursorUpdate {
    Unchanged,
    Hidden,
    Shape(CursorShape),
}

/// CGImage snapshots are immutable. Retaining the image also prevents its
/// address from being reused and falsely matching a different cursor.
struct RetainedImage(*const c_void);
// SAFETY: immutable CoreGraphics images may be retained/read on any thread.
unsafe impl Send for RetainedImage {}
impl Drop for RetainedImage {
    fn drop(&mut self) {
        unsafe { CGImageRelease(self.0) };
    }
}

#[derive(Default)]
pub struct CursorCapture {
    image: Option<RetainedImage>,
    hotspot: Option<(u64, u64)>,
}

impl CursorCapture {
    pub fn reset(&mut self) {
        self.image = None;
        self.hotspot = None;
    }

    pub fn capture_if_changed(&mut self) -> Result<CursorUpdate> {
        let cursor = NSCursor::currentSystemCursor().unwrap_or_else(NSCursor::currentCursor);
        let image = cursor.image();
        let Some(cg_image) = (unsafe {
            image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None)
        }) else {
            self.reset();
            return Ok(CursorUpdate::Hidden);
        };
        let native = &*cg_image as *const objc2_core_graphics::CGImage as *const c_void;
        let hotspot = cursor.hotSpot();
        let key = (hotspot.x.to_bits(), hotspot.y.to_bits());
        if self.image.as_ref().is_some_and(|old| old.0 == native) && self.hotspot == Some(key) {
            return Ok(CursorUpdate::Unchanged);
        }
        let shape = decode_cursor_cgimage(native, hotspot)?;
        // Only cache successful conversions, so a temporary allocation failure
        // can be retried on the next poll.
        self.image = Some(RetainedImage(unsafe { CGImageRetain(native) }));
        self.hotspot = Some(key);
        Ok(shape.map_or(CursorUpdate::Hidden, CursorUpdate::Shape))
    }
}

fn decode_cursor_cgimage(native: *const c_void, hot_spot: NSPoint) -> Result<Option<CursorShape>> {
    let (width, height) = unsafe { (CGImageGetWidth(native), CGImageGetHeight(native)) };
    if width == 0 || height == 0 {
        return Ok(None);
    }
    anyhow::ensure!(
        width <= MAX_NATIVE_CURSOR_EDGE && height <= MAX_NATIVE_CURSOR_EDGE,
        "Native cursor image exceeds bounded conversion size"
    );
    let mut rgba = vec![0u8; width * height * 4];
    let space = unsafe { CGColorSpaceCreateDeviceRGB() };
    anyhow::ensure!(!space.is_null(), "Unable to allocate cursor color space");
    // kCGBitmapByteOrder32Big | kCGImageAlphaPremultipliedLast gives RGBA bytes.
    let context = unsafe {
        CGBitmapContextCreate(
            rgba.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space,
            0x4001,
        )
    };
    unsafe { CGColorSpaceRelease(space) };
    anyhow::ensure!(
        !context.is_null(),
        "Unable to allocate cursor drawing context"
    );
    unsafe {
        CGContextDrawImage(
            context,
            CGRect::new(
                &CGPoint::new(0.0, 0.0),
                &CGSize::new(width as f64, height as f64),
            ),
            native,
        );
        CGContextRelease(context);
    }
    // Preserve Quartz's premultiplied RGB, as the previous TIFF path did.
    // The classic 24-bit RDP cursor uses these RGB bytes plus a thresholded
    // transparency mask; unpremultiplying here would brighten its edge pixels.
    let width = u16::try_from(width).context("Invalid cursor width")?;
    let height = u16::try_from(height).context("Invalid cursor height")?;
    let hot_x = hot_spot.x.round().clamp(0.0, f64::from(width - 1)) as u16;
    let hot_y = hot_spot.y.round().clamp(0.0, f64::from(height - 1)) as u16;
    Ok(Some(clamp_cursor_shape(CursorShape {
        width,
        height,
        hot_x,
        hot_y,
        rgba,
    })))
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageRetain(image: *const c_void) -> *const c_void;
    fn CGImageRelease(image: *const c_void);
    fn CGImageGetWidth(image: *const c_void) -> usize;
    fn CGImageGetHeight(image: *const c_void) -> usize;
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *const c_void);
    fn CGContextRelease(context: *mut c_void);
    #[cfg(test)]
    fn CGBitmapContextCreateImage(context: *mut c_void) -> *const c_void;
}

fn clamp_cursor_shape(shape: CursorShape) -> CursorShape {
    if shape.width <= MAX_RDP_CURSOR_EDGE && shape.height <= MAX_RDP_CURSOR_EDGE {
        return shape;
    }

    let old_width = u32::from(shape.width);
    let old_height = u32::from(shape.height);
    let scale = f32::min(
        f32::from(MAX_RDP_CURSOR_EDGE) / old_width as f32,
        f32::from(MAX_RDP_CURSOR_EDGE) / old_height as f32,
    );
    let new_width =
        ((old_width as f32 * scale).round() as u32).clamp(1, u32::from(MAX_RDP_CURSOR_EDGE));
    let new_height =
        ((old_height as f32 * scale).round() as u32).clamp(1, u32::from(MAX_RDP_CURSOR_EDGE));

    let resized = RgbaImage::from_raw(old_width, old_height, shape.rgba)
        .expect("cursor RGBA length should match dimensions");
    let resized = image::imageops::resize(&resized, new_width, new_height, FilterType::Triangle);

    let hot_x = ((f32::from(shape.hot_x) * (new_width as f32 / old_width as f32)).round() as u16)
        .min(new_width.saturating_sub(1) as u16);
    let hot_y = ((f32::from(shape.hot_y) * (new_height as f32 / old_height as f32)).round() as u16)
        .min(new_height.saturating_sub(1) as u16);

    CursorShape {
        width: new_width as u16,
        height: new_height as u16,
        hot_x,
        hot_y,
        rgba: resized.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::AnyThread;
    use objc2_app_kit::NSBitmapImageRep;

    fn fixture_image() -> RetainedImage {
        let mut pixels = [
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 128, 64, 32, 128,
        ];
        unsafe {
            let space = CGColorSpaceCreateDeviceRGB();
            let context =
                CGBitmapContextCreate(pixels.as_mut_ptr().cast(), 2, 2, 8, 8, space, 0x4001);
            CGColorSpaceRelease(space);
            assert!(!context.is_null());
            let image = CGBitmapContextCreateImage(context);
            CGContextRelease(context);
            assert!(!image.is_null());
            RetainedImage(image)
        }
    }

    fn legacy_tiff_pixels(image: &RetainedImage) -> Vec<u8> {
        let cg_image = unsafe { &*image.0.cast::<objc2_core_graphics::CGImage>() };
        let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), cg_image);
        let tiff = bitmap.TIFFRepresentation().unwrap();
        image::load_from_memory_with_format(&tiff.to_vec(), image::ImageFormat::Tiff)
            .unwrap()
            .to_rgba8()
            .into_raw()
    }

    #[test]
    fn direct_cursor_conversion_preserves_tiff_orientation_and_alpha() {
        let image = fixture_image();
        let actual = decode_cursor_cgimage(image.0, NSPoint::new(1.0, 0.0))
            .unwrap()
            .unwrap();
        let expected = legacy_tiff_pixels(&image);
        assert_eq!(
            (actual.width, actual.height, actual.hot_x, actual.hot_y),
            (2, 2, 1, 0)
        );
        assert_eq!(actual.rgba, expected);
        assert_eq!(&actual.rgba[12..], &[128, 64, 32, 128]);
    }

    #[test]
    #[ignore = "opt-in native cursor conversion timing"]
    fn cursor_conversion_microbenchmark() {
        let image = fixture_image();
        let now = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(legacy_tiff_pixels(&image));
        }
        let tiff = now.elapsed();
        let now = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(decode_cursor_cgimage(image.0, NSPoint::new(1.0, 0.0)).unwrap());
        }
        eprintln!(
            "cursor_conversion iterations=1000 legacy_tiff_us={} direct_rgba_us={}",
            tiff.as_micros(),
            now.elapsed().as_micros()
        );
    }

    #[test]
    fn clamp_cursor_shape_resizes_oversized_cursor_and_hotspot() {
        let shape = CursorShape {
            width: 56,
            height: 80,
            hot_x: 5,
            hot_y: 5,
            rgba: vec![255; 56 * 80 * 4],
        };

        let resized = clamp_cursor_shape(shape);

        assert!(resized.width <= MAX_RDP_CURSOR_EDGE);
        assert!(resized.height <= MAX_RDP_CURSOR_EDGE);
        assert_eq!(resized.width, 22);
        assert_eq!(resized.height, 32);
        assert_eq!(resized.hot_x, 2);
        assert_eq!(resized.hot_y, 2);
        assert_eq!(
            resized.rgba.len(),
            usize::from(resized.width) * usize::from(resized.height) * 4
        );
    }
}
