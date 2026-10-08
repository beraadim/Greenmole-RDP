use core::num::{NonZeroU16, NonZeroUsize};

use anyhow::Result;
use bytes::{Bytes, BytesMut};
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_graphics::diff::Rect;
use ironrdp_pdu::pointer::PointerPositionAttribute;
use tracing::{debug, warn};

#[rustfmt::skip]
pub use ironrdp_acceptor::DesktopSize;
pub use ironrdp_graphics::image_processing::PixelFormat;

/// Display Update
///
/// Contains all types of display updates currently supported by the server implementation
/// and the RDP spec
///
#[derive(Debug, Clone)]
pub enum DisplayUpdate {
    Resize(DesktopSize),
    Bitmap(BitmapUpdate),
    /// GFX H.264 frame update (requires RDPGFX channel)
    GfxFrame(GfxFrameUpdate),
    /// GFX uncompressed dirty rect update (small changes, zero encode latency)
    GfxUncompressed(GfxUncompressedUpdate),
    PointerPosition(PointerPositionAttribute),
    ColorPointer(ColorPointer),
    RGBAPointer(RGBAPointer),
    HidePointer,
    DefaultPointer,
}

/// H.264 encoded frame for GFX pipeline
#[derive(Debug, Clone)]
pub struct GfxFrameUpdate {
    /// H.264 NAL units (Annex B format).
    /// In AVC420 mode: complete frame. In AVC444 mode: Main View (stream1).
    pub h264_data: Bytes,
    /// Visible display width
    pub width: u16,
    /// Visible display height
    pub height: u16,
    /// H.264 encoded width (aligned to 16)
    pub enc_width: u16,
    /// H.264 encoded height (aligned to 16)
    pub enc_height: u16,
    pub is_keyframe: bool,
    /// AVC444 Auxiliary View H.264 data (stream2).
    /// None = AVC420 mode, Some = AVC444 mode.
    pub h264_aux: Option<Bytes>,
}

/// Single uncompressed dirty rectangle for GFX pipeline
#[derive(Debug, Clone)]
pub struct UncompressedRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Pixel data in BGRA format (= RDP XRGB on little-endian), tightly packed per row
    pub pixel_data: Bytes,
}

/// GFX uncompressed frame update — sends raw pixels for small dirty regions
#[derive(Debug, Clone)]
pub struct GfxUncompressedUpdate {
    pub rects: Vec<UncompressedRect>,
    /// Surface width (for first-frame setup)
    pub width: u16,
    /// Surface height (for first-frame setup)
    pub height: u16,
}

#[derive(Clone)]
pub struct RGBAPointer {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub data: Vec<u8>,
}

impl core::fmt::Debug for RGBAPointer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RGBAPointer")
            .field("with", &self.width)
            .field("height", &self.height)
            .field("hot_x", &self.hot_x)
            .field("hot_y", &self.hot_y)
            .field("data_len", &self.data.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ColorPointer {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    pub and_mask: Vec<u8>,
    pub xor_mask: Vec<u8>,
}

pub struct Framebuffer {
    pub width: NonZeroU16,
    pub height: NonZeroU16,
    pub format: PixelFormat,
    pub data: BytesMut,
    pub stride: usize,
}

impl TryInto<Framebuffer> for BitmapUpdate {
    type Error = &'static str;

    fn try_into(self) -> Result<Framebuffer, Self::Error> {
        assert_eq!(self.x, 0);
        assert_eq!(self.y, 0);
        Ok(Framebuffer {
            width: self.width,
            height: self.height,
            format: self.format,
            data: self.data.into(),
            stride: self.stride.get(),
        })
    }
}

impl Framebuffer {
    pub fn new(width: NonZeroU16, height: NonZeroU16, format: PixelFormat) -> Self {
        let mut data = BytesMut::new();
        let w = NonZeroUsize::from(width).get();
        let h = NonZeroUsize::from(height).get();
        let bpp = usize::from(format.bytes_per_pixel());
        data.resize(bpp * w * h, 0);

        Self {
            width,
            height,
            format,
            data,
            stride: bpp * w,
        }
    }

    pub fn update(&mut self, bitmap: &BitmapUpdate) {
        if self.format != bitmap.format {
            warn!("Bitmap format mismatch, unsupported");
            return;
        }
        let bpp = usize::from(self.format.bytes_per_pixel());
        let x = usize::from(bitmap.x);
        let y = usize::from(bitmap.y);
        let width = NonZeroUsize::from(bitmap.width).get();
        let height = NonZeroUsize::from(bitmap.height).get();

        let data = &mut self.data;
        let start = y * self.stride + x * bpp;
        let end = start + (height - 1) * self.stride + width * bpp;
        let dst = &mut data[start..end];

        for y in 0..height {
            let start = y * bitmap.stride.get();
            let end = start + width * bpp;

            let src = bitmap.data.slice(start..end);

            let start = y * self.stride;
            let end = start + width * bpp;
            let dst = &mut dst[start..end];

            dst.copy_from_slice(&src);
        }
    }

    pub(crate) fn update_diffs(&mut self, bitmap: &BitmapUpdate, diffs: &[Rect]) {
        diffs
            .iter()
            .filter_map(|diff| {
                let x = u16::try_from(diff.x).ok()?;
                let y = u16::try_from(diff.y).ok()?;
                let width = u16::try_from(diff.width).ok().and_then(NonZeroU16::new)?;
                let height = u16::try_from(diff.height).ok().and_then(NonZeroU16::new)?;

                bitmap.sub(x, y, width, height)
            })
            .for_each(|sub| self.update(&sub));
    }
}

/// Bitmap Display Update
///
/// Bitmap updates are encoded using RDP 6.0 compression, fragmented and sent using
/// Fastpath Server Updates
///
#[derive(Clone)]
pub struct BitmapUpdate {
    pub x: u16,
    pub y: u16,
    pub width: NonZeroU16,
    pub height: NonZeroU16,
    pub format: PixelFormat,
    pub data: Bytes,
    pub stride: NonZeroUsize,
    /// Optional damage hints in absolute framebuffer coordinates.
    /// Empty means the encoder should derive diffs on its own.
    pub dirty_rects: Vec<Rect>,
}

impl BitmapUpdate {
    /// Extracts a sub-region of the bitmap update.
    ///
    /// # Parameters
    ///
    /// - `x`: The x-coordinate of the top-left corner of the sub-region.
    /// - `y`: The y-coordinate of the top-left corner of the sub-region.
    /// - `width`: The width of the sub-region.
    /// - `height`: The height of the sub-region.
    ///
    /// # Returns
    ///
    /// An `Option` containing a new `BitmapUpdate` representing the sub-region if the specified
    /// dimensions are within the bounds of the original bitmap update, otherwise `None`.
    ///
    /// # Example
    ///
    /// ```
    /// # use core::num::NonZeroU16;
    /// # use std::num::NonZeroUsize;
    /// # use bytes::Bytes;
    /// # use ironrdp_graphics::image_processing::PixelFormat;
    /// # use ironrdp_server::BitmapUpdate;
    /// let original = BitmapUpdate {
    ///     x: 0,
    ///     y: 0,
    ///     width: NonZeroU16::new(100).unwrap(),
    ///     height: NonZeroU16::new(100).unwrap(),
    ///     format: PixelFormat::ARgb32,
    ///     data: Bytes::from(vec![0; 40000]),
    ///     stride: NonZeroUsize::new(400).unwrap(),
    /// };
    ///
    /// let sub_region = original.sub(10, 10, NonZeroU16::new(50).unwrap(), NonZeroU16::new(50).unwrap());
    /// assert!(sub_region.is_some());
    /// ```
    #[must_use]
    pub fn sub(&self, x: u16, y: u16, width: NonZeroU16, height: NonZeroU16) -> Option<Self> {
        if x + width.get() > self.width.get() || y + height.get() > self.height.get() {
            None
        } else {
            let bpp = usize::from(self.format.bytes_per_pixel());
            let start = usize::from(y) * self.stride.get() + usize::from(x) * bpp;
            let end = start
                + usize::from(height.get() - 1) * self.stride.get()
                + usize::from(width.get()) * bpp;
            Some(Self {
                x: self.x + x,
                y: self.y + y,
                width,
                height,
                format: self.format,
                data: self.data.slice(start..end),
                stride: self.stride,
                dirty_rects: Vec::new(),
            })
        }
    }
}

impl core::fmt::Debug for BitmapUpdate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BitmapUpdate")
            .field("x", &self.x)
            .field("y", &self.y)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("stride", &self.stride)
            .field("dirty_rects", &self.dirty_rects.len())
            .finish()
    }
}

/// Display Updates receiver for an RDP server
///
/// The RDP server will repeatedly call the `next_update` method to receive
/// display updates which will then be encoded and sent to the client
///
/// See [`RdpServerDisplay`] example.
#[async_trait::async_trait]
pub trait RdpServerDisplayUpdates {
    /// # Cancel safety
    ///
    /// This method MUST be cancellation safe because it is used in a
    /// `tokio::select!` statement. If some other branch completes first, it
    /// MUST be guaranteed that no data is lost.
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>>;
}

/// Display for an RDP server
///
/// # Example
///
/// ```
///# use anyhow::Result;
/// use ironrdp_server::{DesktopSize, DisplayUpdate, RdpServerDisplay, RdpServerDisplayUpdates};
///
/// pub struct DisplayUpdates {
///     receiver: tokio::sync::mpsc::Receiver<DisplayUpdate>,
/// }
///
/// #[async_trait::async_trait]
/// impl RdpServerDisplayUpdates for DisplayUpdates {
///     async fn next_update(&mut self) -> anyhow::Result<Option<DisplayUpdate>> {
///         Ok(self.receiver.recv().await)
///     }
/// }
///
/// pub struct DisplayHandler {
///     width: u16,
///     height: u16,
/// }
///
/// #[async_trait::async_trait]
/// impl RdpServerDisplay for DisplayHandler {
///     async fn size(&mut self) -> DesktopSize {
///         DesktopSize { width: self.width, height: self.height }
///     }
///
///     async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
///         Ok(Box::new(DisplayUpdates { receiver: todo!() }))
///     }
/// }
/// ```
#[async_trait::async_trait]
pub trait RdpServerDisplay: Send {
    /// Return the requested desktop size used for the initial Demand Active.
    /// Later layout changes are announced with DisplayUpdate::Resize; capture
    /// and input must use the size committed through activate_size.
    async fn size(&mut self) -> DesktopSize;

    /// Return a display updates receiver
    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>>;

    /// Commit the desktop size negotiated for this activation. A newer layout
    /// request may already be pending, so implementations must keep that request
    /// separate from the size used to create the active capture stream.
    fn activate_size(&mut self, _size: DesktopSize) {}

    /// Request a new size for the display
    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        debug!(?layout, "Requesting layout")
    }

    /// Request a specific resolution (called during client capability negotiation).
    /// May be ignored if the display has a fixed resolution.
    fn request_resize(&mut self, _width: u16, _height: u16) {
        // Default: ignore. Override in display implementations that support dynamic resolution.
    }

    /// Force-set display size (server-initiated, bypasses fixed_resolution check).
    /// Used for hot-config resolution changes that apply on next connection.
    fn set_size(&mut self, _width: u16, _height: u16) {
        // Default: delegate to request_resize
    }
}

/// Wait independently of capture so resize also works on an unchanged or
/// unavailable desktop. Watching before reading prevents a lost wakeup.
pub async fn wait_for_display_resize(
    mut resize_rx: tokio::sync::watch::Receiver<DesktopSize>,
    width: u32,
    height: u32,
) -> Option<DesktopSize> {
    loop {
        let size = *resize_rx.borrow_and_update();
        if u32::from(size.width) != width || u32::from(size.height) != height {
            return Some(size);
        }
        resize_rx.changed().await.ok()?;
    }
}

/// Coalesce a burst after the resize branch has won the capture select. Keeping
/// this outside the cancellable frame wait prevents busy desktops from resetting
/// the debounce on every video frame. Bound the pause during continuous dragging.
pub async fn settle_display_resize(
    mut resize_rx: tokio::sync::watch::Receiver<DesktopSize>,
) -> DesktopSize {
    use tokio::time::{sleep_until, Duration, Instant};
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        let size = *resize_rx.borrow_and_update();
        let now = Instant::now();
        if now >= deadline {
            return size;
        }
        let quiet_until = (now + Duration::from_millis(120)).min(deadline);
        tokio::select! {
            _ = sleep_until(quiet_until) => return *resize_rx.borrow_and_update(),
            changed = resize_rx.changed() => {
                if changed.is_err() { return *resize_rx.borrow_and_update(); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroUsize};

    use ironrdp_graphics::diff::Rect;
    use ironrdp_graphics::image_processing::PixelFormat;

    use super::{
        settle_display_resize, wait_for_display_resize, BitmapUpdate, DesktopSize, Framebuffer,
    };

    #[tokio::test]
    async fn resize_burst_commits_latest_dimensions() {
        let (tx, rx) = tokio::sync::watch::channel(DesktopSize {
            width: 1920,
            height: 1080,
        });
        let changes = async {
            for width in [1800, 1700, 1600] {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                tx.send_replace(DesktopSize { width, height: 900 });
            }
        };
        let (size, ()) = tokio::join!(settle_display_resize(rx), changes);
        assert_eq!((size.width, size.height), (1600, 900));
    }

    #[tokio::test]
    async fn resize_wait_wakes_without_a_capture_frame() {
        let (tx, rx) = tokio::sync::watch::channel(DesktopSize {
            width: 1920,
            height: 1200,
        });
        let wait = wait_for_display_resize(rx, 1920, 1200);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        tx.send_replace(DesktopSize {
            width: 2560,
            height: 1305,
        });
        let resized = tokio::time::timeout(std::time::Duration::from_secs(1), wait)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((resized.width, resized.height), (2560, 1305));
    }

    #[tokio::test]
    async fn resize_wait_preserves_newest_request_during_activation() {
        let (tx, rx) = tokio::sync::watch::channel(DesktopSize {
            width: 1920,
            height: 1200,
        });
        tx.send_replace(DesktopSize {
            width: 1600,
            height: 900,
        });
        let activating = wait_for_display_resize(rx.clone(), 1920, 1200)
            .await
            .unwrap();
        assert_eq!((activating.width, activating.height), (1600, 900));

        // A second layout arrives while the first Demand Active is in flight.
        // The new capture uses the first negotiated size; the pending request
        // must still trigger a second reactivation rather than being lost.
        tx.send_replace(DesktopSize {
            width: 1280,
            height: 720,
        });
        tx.send_replace(DesktopSize {
            width: 1024,
            height: 768,
        });
        let newest = wait_for_display_resize(
            rx,
            u32::from(activating.width),
            u32::from(activating.height),
        )
        .await
        .unwrap();
        assert_eq!((newest.width, newest.height), (1024, 768));
    }

    #[tokio::test]
    async fn resize_wait_does_not_reactivate_for_the_current_size() {
        let (tx, rx) = tokio::sync::watch::channel(DesktopSize {
            width: 1920,
            height: 1200,
        });
        tx.send_replace(DesktopSize {
            width: 1280,
            height: 720,
        });
        tx.send_replace(DesktopSize {
            width: 1920,
            height: 1200,
        });
        let wait = wait_for_display_resize(rx, 1920, 1200);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        drop(tx);
        assert!(wait.await.is_none());
    }

    #[test]
    fn framebuffer_update() {
        let width = NonZeroU16::new(800).unwrap();
        let height = NonZeroU16::new(600).unwrap();
        let fmt = PixelFormat::ABgr32;
        let bpp = usize::from(fmt.bytes_per_pixel());
        let mut fb = Framebuffer::new(width, height, fmt);

        let width = 15;
        let stride = NonZeroUsize::new(width * bpp).unwrap();
        let height = 20;
        let data = vec![1u8; height * stride.get()];
        let update = BitmapUpdate {
            x: 1,
            y: 2,
            width: NonZeroU16::new(15).unwrap(),
            height: NonZeroU16::new(20).unwrap(),
            format: fmt,
            data: data.into(),
            stride,
            dirty_rects: Vec::new(),
        };
        let diffs = vec![Rect::new(2, 3, 4, 5)];
        fb.update_diffs(&update, &diffs);
        let data = fb.data;
        for y in 5..10 {
            for x in 3..7 {
                for b in 0..bpp {
                    assert_eq!(data[y * fb.stride + x * bpp + b], 1);
                }
            }
        }
    }
}
