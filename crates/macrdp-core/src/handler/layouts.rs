//! Session-local interpretation of RDP Set 1 scan codes.
//!
//! Match the entire Windows layout identifier: a locale's Dvorak or other
//! variant must not silently be interpreted as its ordinary QWERTY layout.
//! Unicode input bypasses these tables and remains independent of host layout.

use super::TextTranslation;

mod tables;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyboardTranslationProfile {
    Us,
    Uk,
    Norwegian,
    Swedish,
    Danish,
    German,
    French,
}

impl KeyboardTranslationProfile {
    pub(super) fn from_layout_id(layout: u32) -> Option<Self> {
        Some(match layout {
            0x0409 => Self::Us,
            0x0809 => Self::Uk,
            0x0414 => Self::Norwegian,
            0x041D => Self::Swedish,
            0x0406 => Self::Danish,
            0x0407 => Self::German,
            0x040C => Self::French,
            _ => return None,
        })
    }

    pub(super) fn from_setting(setting: &str, negotiated: u32) -> Option<Self> {
        Some(match setting {
            "auto" => return Self::from_layout_id(negotiated),
            "us" => Self::Us,
            "uk" => Self::Uk,
            "no" => Self::Norwegian,
            "se" => Self::Swedish,
            "dk" => Self::Danish,
            "de" => Self::German,
            "fr" => Self::French,
            // Configuration validation rejects unknown names. Physical is an
            // explicit escape hatch for layouts not represented here.
            _ => return None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Us => "us",
            Self::Uk => "uk",
            Self::Norwegian => "norwegian",
            Self::Swedish => "swedish",
            Self::Danish => "danish",
            Self::German => "german",
            Self::French => "french",
        }
    }

    pub(super) fn has_altgr(self) -> bool {
        self != Self::Us
    }

    /// A text key's layout identity, before shortcut modifier policy is applied.
    pub(super) fn translate(
        self,
        code: u8,
        shifted: bool,
        altgr: bool,
        caps: bool,
    ) -> Option<TextTranslation> {
        if altgr && self.has_altgr() {
            return tables::altgr(self, code, shifted, caps);
        }

        let mut shifted = shifted;
        if caps && self.caps_affects(code) {
            shifted = !shifted;
        }
        tables::base(self, code, shifted)
    }

    fn caps_affects(self, code: u8) -> bool {
        match self {
            // These Windows layouts use Shift Lock for their alphanumeric
            // blocks, including number-row symbols. ISO <> is unaffected.
            Self::French => matches!(code, 0x02..=0x0D | 0x10..=0x1B | 0x1E..=0x28 | 0x2B..=0x35),
            Self::German => matches!(code, 0x02..=0x0C | 0x10..=0x1B | 0x1E..=0x28 | 0x2B..=0x34),
            _ => {
                matches!(tables::base(self, code, false), Some(TextTranslation::Unicode(ch)) if ch.is_alphabetic())
            }
        }
    }
}
