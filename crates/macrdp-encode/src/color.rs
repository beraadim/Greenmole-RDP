//! RDP graphics use full-range BT.709, including both AVC420 and AVC444.
//! See MS-RDPEGFX §3.3.8.3.1. Video-range BT.601 is not interchangeable:
//! FreeRDP applies this inverse matrix regardless of an encoder's VUI defaults.

#[inline]
pub(crate) fn luma(r: i32, g: i32, b: i32) -> u8 {
    ((13_933 * r + 46_871 * g + 4_732 * b + 32_768) >> 16).clamp(0, 255) as u8
}

#[inline]
pub(crate) fn chroma(r: i32, g: i32, b: i32) -> (u8, u8) {
    let u = ((-7_509 * r - 25_259 * g + 32_768 * b + 32_768) >> 16) + 128;
    let v = ((32_768 * r - 29_763 * g - 3_005 * b + 32_768) >> 16) + 128;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

#[cfg(test)]
#[path = "color/tests.rs"]
pub(crate) mod tests;
