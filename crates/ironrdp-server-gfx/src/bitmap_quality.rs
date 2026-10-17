use ironrdp_pdu::codecs::rfx::Quant;
use ironrdp_pdu::rdp::capability_sets::{BitmapCodecs, CodecProperty};

/// Image quality policy for the negotiated bitmap transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BitmapQuality {
    /// Spend fewer bytes on RemoteFX detail to favor responsiveness.
    Performance,
    /// Preserve the RemoteFX codec's standard quantization.
    #[default]
    Balanced,
    /// Preserve more fine detail using the lowest legal RemoteFX quantization.
    HighQuality,
    /// Use lossless RGB-plane RLE instead of RemoteFX or the GFX video channel.
    Lossless,
}

impl BitmapQuality {
    /// Parse the shared public configuration spelling without silently changing policy.
    pub fn from_config(value: &str) -> Option<Self> {
        match value {
            "low_latency" => Some(Self::Performance),
            "balanced" => Some(Self::Balanced),
            "high_quality" => Some(Self::HighQuality),
            "lossless" => Some(Self::Lossless),
            _ => None,
        }
    }

    pub(crate) fn config_name(self) -> &'static str {
        match self {
            Self::Performance => "low_latency",
            Self::Balanced => "balanced",
            Self::HighQuality => "high_quality",
            Self::Lossless => "lossless",
        }
    }

    pub(crate) fn restrict_codecs(self, codecs: &mut BitmapCodecs) {
        if self == Self::Lossless {
            codecs.0.retain(|codec| {
                !matches!(
                    codec.property,
                    CodecProperty::RemoteFx(_) | CodecProperty::ImageRemoteFx(_)
                )
            });
        }
    }

    pub(crate) fn remotefx_quant(self) -> Quant {
        // MS-RDPRFX 2.2.2.1.5: each subband factor is in [6, 15].
        // RemoteFX still transforms/quantizes colors at factor 6; only the
        // separate RGB-plane transport provides a lossless desktop.
        let mut quant = Quant::default();
        for value in [
            &mut quant.ll3,
            &mut quant.lh3,
            &mut quant.hl3,
            &mut quant.hh3,
            &mut quant.lh2,
            &mut quant.hl2,
            &mut quant.hh2,
            &mut quant.lh1,
            &mut quant.hl1,
            &mut quant.hh1,
        ] {
            *value = match self {
                Self::Performance => (*value + 1).min(15),
                Self::Balanced => *value,
                Self::HighQuality | Self::Lossless => 6,
            };
        }
        quant
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_pdu::rdp::capability_sets::server_codecs_capabilities;

    #[test]
    fn lossless_does_not_advertise_either_remotefx_codec() {
        let mut codecs = server_codecs_capabilities(&[]).unwrap();
        assert!(codecs
            .0
            .iter()
            .any(|codec| matches!(codec.property, CodecProperty::RemoteFx(_))));
        BitmapQuality::Lossless.restrict_codecs(&mut codecs);
        assert!(!codecs.0.iter().any(|codec| matches!(
            codec.property,
            CodecProperty::RemoteFx(_) | CodecProperty::ImageRemoteFx(_)
        )));
    }

    #[test]
    fn preset_quantization_is_legal_and_balanced_keeps_legacy_values() {
        for quality in [
            BitmapQuality::Performance,
            BitmapQuality::Balanced,
            BitmapQuality::HighQuality,
        ] {
            let quant = quality.remotefx_quant();
            assert!([
                quant.ll3, quant.lh3, quant.hl3, quant.hh3, quant.lh2, quant.hl2, quant.hh2,
                quant.lh1, quant.hl1, quant.hh1
            ]
            .into_iter()
            .all(|value| (6..=15).contains(&value)));
        }
        assert_eq!(BitmapQuality::Balanced.remotefx_quant(), Quant::default());
    }
}
