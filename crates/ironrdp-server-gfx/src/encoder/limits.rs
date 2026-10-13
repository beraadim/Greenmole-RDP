//! A Fast-Path packet limit and a client's reassembly limit are different.
//! MS-RDPBCGR 2.2.7.2.6 caps the *complete* update, not each wire fragment.
//! https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/01717954-716a-424d-af35-28fb2b86df89

use ironrdp_graphics::diff::Rect;

#[derive(Debug, Clone, Copy)]
pub(super) struct UpdateLimits {
    max_payload: usize,
}

impl Default for UpdateLimits {
    fn default() -> Self {
        // Without this capability, do not assume that fragmented updates can
        // be reassembled. Each independently encoded update fits one packet.
        Self {
            max_payload: super::fast_path::MAX_FASTPATH_UPDATE_SIZE,
        }
    }
}

impl UpdateLimits {
    pub(super) fn from_client(bytes: u32) -> Self {
        if bytes == 0 {
            return Self::default();
        }
        Self {
            // Also bound our work if a peer announces an enormous allocation.
            // This is the maximum advertised by our own server capabilities.
            max_payload: bytes.min(16_777_215) as usize,
        }
    }

    pub(super) fn max_payload(self) -> usize {
        self.max_payload
    }

    pub(super) fn should_partition(self, rect: &Rect) -> bool {
        // Avoid first encoding an entire desktop for a small receive buffer.
        // Keep at least one RemoteFX tile so edge padding cannot inflate the
        // amount of work; the actual encoded byte limit is checked afterward.
        rect.width.saturating_mul(rect.height) > (self.max_payload / 4).max(64 * 64)
    }
}

/// Split complete images, never arbitrary compressed bytes. A depth-first
/// traversal needs only O(log(width * height)) pending rectangles. Prefer tile
/// boundaries to avoid redundant RemoteFX edge padding on large regions.
pub(super) fn split_rect(rect: &Rect) -> Option<(Rect, Rect)> {
    let horizontal = rect.width >= rect.height && rect.width > 1;
    let length = if horizontal { rect.width } else { rect.height };
    if length <= 1 {
        return None;
    }
    let half = if length > 64 {
        (length / 2 / 64).max(1) * 64
    } else {
        length / 2
    };
    let mut first = rect.clone();
    let mut second = rect.clone();
    if horizontal {
        first.width = half;
        second.x += half;
        second.width -= half;
    } else {
        first.height = half;
        second.y += half;
        second.height -= half;
    }
    Some((first, second))
}
