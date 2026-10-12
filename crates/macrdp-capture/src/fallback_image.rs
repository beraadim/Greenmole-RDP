//! Normalize CoreGraphics recovery images before declaring their pixels BGRA.
//! CGImage providers may contain RGBA, wide-gamut or high-component-depth data;
//! the provider's row stride alone cannot establish a compatible pixel layout.

use anyhow::{Context, Result};
use core_graphics::{
    geometry::{CGPoint, CGRect, CGSize},
    image::{CGImage, CGImageRef},
};
use std::ffi::c_void;

// Accommodate 8K displays and wide layouts while bounding recovery allocations.
const MAX_EDGE: usize = 16_384;
const MAX_BYTES: usize = 256 * 1024 * 1024;
const BGRA8_PREMULTIPLIED: u32 = (2 << 12) | 2;

pub(crate) struct NormalizedImage {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub pixels: Vec<u8>,
}

fn checked_layout(width: usize, height: usize) -> Result<(usize, usize)> {
    anyhow::ensure!(
        width > 0 && height > 0 && width <= MAX_EDGE && height <= MAX_EDGE,
        "Unsupported fallback image dimensions"
    );
    let stride = width.checked_mul(4).context("Fallback row size overflow")?;
    let bytes = stride
        .checked_mul(height)
        .context("Fallback image size overflow")?;
    anyhow::ensure!(bytes <= MAX_BYTES, "Fallback image exceeds memory budget");
    Ok((stride, bytes))
}

/// Quartz performs the source-layout and source-color-space conversion. Keep
/// the image borrowed and the destination Vec stable until the drawing context
/// is released; only then transfer that single buffer to the captured frame.
pub(crate) fn normalize(image: &CGImage) -> Result<NormalizedImage> {
    let (width, height) = (image.width(), image.height());
    let (stride, bytes) = checked_layout(width, height)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(bytes)
        .context("Allocate fallback image")?;
    pixels.resize(bytes, 0);
    // SAFETY: kCGColorSpaceSRGB is a CoreGraphics-owned, process-lifetime CFString.
    let space = ColorSpace(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) });
    anyhow::ensure!(!space.0.is_null(), "Create fallback sRGB color space");
    // Little-endian premultiplied-first 32-bit pixels have B,G,R,A byte order.
    // SAFETY: checked_layout established every destination row lies in pixels.
    // This allocation is not resized/moved until the context has been released.
    let context = BitmapContext(unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            stride,
            space.0,
            BGRA8_PREMULTIPLIED,
        )
    });
    anyhow::ensure!(!context.0.is_null(), "Create fallback BGRA drawing context");
    // CGImageRef is the borrowed foreign image object underlying CGImage. The
    // source image remains retained by its caller throughout this synchronous draw.
    let native = (&**image as *const CGImageRef).cast::<c_void>();
    unsafe {
        // No resizing or filtering: preserve one source pixel per output pixel.
        CGContextSetInterpolationQuality(context.0, 1); // kCGInterpolationNone
        CGContextSetShouldAntialias(context.0, false);
        CGContextSetBlendMode(context.0, 17); // kCGBlendModeCopy
        CGContextDrawImage(
            context.0,
            CGRect::new(
                &CGPoint::new(0.0, 0.0),
                &CGSize::new(width as f64, height as f64),
            ),
            native,
        );
        CGContextFlush(context.0);
    }
    drop(context);
    Ok(NormalizedImage {
        width: width as u32,
        height: height as u32,
        stride,
        pixels,
    })
}

struct ColorSpace(*mut c_void);
impl Drop for ColorSpace {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CGColorSpaceRelease(self.0) };
        }
    }
}

struct BitmapContext(*mut c_void);
impl Drop for BitmapContext {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CGContextRelease(self.0) };
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
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
    fn CGContextSetInterpolationQuality(context: *mut c_void, quality: i32);
    fn CGContextSetShouldAntialias(context: *mut c_void, value: bool);
    fn CGContextSetBlendMode(context: *mut c_void, mode: i32);
    fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *const c_void);
    fn CGContextFlush(context: *mut c_void);
    fn CGContextRelease(context: *mut c_void);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::{
        color_space::{kCGColorSpaceSRGB, CGColorSpace},
        data_provider::CGDataProvider,
    };
    use std::sync::Arc;

    fn source(
        width: usize,
        height: usize,
        bits: usize,
        stride: usize,
        info: u32,
        data: Vec<u8>,
    ) -> CGImage {
        let space = CGColorSpace::create_with_name(unsafe { kCGColorSpaceSRGB }).unwrap();
        let provider = CGDataProvider::from_buffer(Arc::new(data));
        CGImage::new(
            width,
            height,
            bits,
            bits * 4,
            stride,
            &space,
            info,
            &provider,
            false,
            0,
        )
    }

    #[test]
    fn rgba_source_preserves_channels_orientation_and_excludes_row_padding() {
        // Deliberately use RGBA rather than BGRA and distinct top/bottom rows.
        let source = source(
            3,
            2,
            8,
            16,
            (4 << 12) | 1,
            vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 91, 92, 93, 94, 17, 33, 65, 255,
                255, 255, 255, 255, 0, 0, 0, 255, 95, 96, 97, 98,
            ],
        );
        let actual = normalize(&source).unwrap();
        assert_eq!((actual.width, actual.height, actual.stride), (3, 2, 12));
        assert_eq!(
            actual.pixels,
            vec![
                0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 65, 33, 17, 255, 255, 255, 255,
                255, 0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn sixteen_bit_components_are_normalized_instead_of_reinterpreted_as_bgra8() {
        let pixels: [u16; 16] = [
            65535, 0, 0, 65535, 0, 65535, 0, 65535, 0, 0, 65535, 65535, 65535, 65535, 65535, 65535,
        ];
        let data = pixels.into_iter().flat_map(u16::to_be_bytes).collect();
        let source = source(2, 2, 16, 16, (3 << 12) | 1, data);
        let actual = normalize(&source).unwrap();
        assert_eq!((actual.width, actual.height, actual.stride), (2, 2, 8));
        assert_eq!(
            actual.pixels,
            vec![0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,]
        );
    }

    #[test]
    fn srgb_dark_gradient_and_flat_regions_survive_normalization() {
        let data: Vec<u8> = (0..=255u8)
            .flat_map(|value| [value, value, value, 255])
            .collect();
        let source = source(256, 1, 8, 1024, (4 << 12) | 1, data.clone());
        assert_eq!(normalize(&source).unwrap().pixels, data);
    }

    #[test]
    fn zero_oversized_and_overflow_dimensions_fail_before_allocation() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (usize::MAX, 2),
            (2, usize::MAX),
            (MAX_EDGE + 1, 1),
            (MAX_EDGE, MAX_EDGE),
        ] {
            assert!(checked_layout(width, height).is_err());
        }
        assert_eq!(checked_layout(3, 2).unwrap(), (12, 24));
        assert_eq!(checked_layout(7680, 4320).unwrap(), (30720, 132710400));
    }
}
