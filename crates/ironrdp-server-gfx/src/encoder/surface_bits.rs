//! Keep codec-specific Surface Bits carriers separate from their shared layout.

use anyhow::Result;
use ironrdp_pdu::encode_vec;
use ironrdp_pdu::fast_path::UpdateCode;
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_pdu::surface_commands::{ExtendedBitmapDataPdu, SurfaceBitsPdu, SurfaceCommand};

use super::{BitmapUpdate, UpdateFragmenter};

pub(super) fn set_surface(
    bitmap: &BitmapUpdate,
    codec_id: u8,
    data: &[u8],
) -> Result<UpdateFragmenter> {
    Ok(UpdateFragmenter::new(
        UpdateCode::SurfaceCommands,
        encode_vec(&surface_command(bitmap, codec_id, data))?,
    ))
}

pub(super) fn stream_surface(
    bitmap: &BitmapUpdate,
    codec_id: u8,
    data: &[u8],
) -> Result<UpdateFragmenter> {
    Ok(UpdateFragmenter::new(
        UpdateCode::SurfaceCommands,
        encode_vec(&stream_surface_command(bitmap, codec_id, data))?,
    ))
}

fn surface_command<'a>(bitmap: &BitmapUpdate, codec_id: u8, data: &'a [u8]) -> SurfaceCommand<'a> {
    SurfaceCommand::SetSurfaceBits(surface_bits(bitmap, codec_id, data))
}

pub(super) fn stream_surface_command<'a>(
    bitmap: &BitmapUpdate,
    codec_id: u8,
    data: &'a [u8],
) -> SurfaceCommand<'a> {
    SurfaceCommand::StreamSurfaceBits(surface_bits(bitmap, codec_id, data))
}

fn surface_bits<'a>(bitmap: &BitmapUpdate, codec_id: u8, data: &'a [u8]) -> SurfaceBitsPdu<'a> {
    let destination = ExclusiveRectangle {
        left: bitmap.x,
        top: bitmap.y,
        right: bitmap.x + bitmap.width.get(),
        bottom: bitmap.y + bitmap.height.get(),
    };
    let extended_bitmap_data = ExtendedBitmapDataPdu {
        bpp: bitmap.format.bytes_per_pixel() * 8,
        width: bitmap.width.get(),
        height: bitmap.height.get(),
        codec_id,
        header: None,
        data,
    };
    SurfaceBitsPdu {
        destination,
        extended_bitmap_data,
    }
}
