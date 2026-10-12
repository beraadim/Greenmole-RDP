//! Bound both native-to-Rust string copies and their encoded wire responses.
//! Leave space in the 32 MiB event budget for graphics, audio and other work.

pub(crate) const MAX_TEXT_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn valid_unicode_lengths(utf16_units: usize, utf8_bytes: usize) -> bool {
    utf8_bytes <= MAX_TEXT_PAYLOAD_BYTES
        && utf16_units
            .checked_add(1)
            .and_then(|units| units.checked_mul(2))
            .is_some_and(|bytes| bytes <= MAX_TEXT_PAYLOAD_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_budget_includes_terminator_and_both_encodings_without_overflow() {
        let largest = MAX_TEXT_PAYLOAD_BYTES / 2 - 1;
        assert!(valid_unicode_lengths(largest, MAX_TEXT_PAYLOAD_BYTES));
        assert!(!valid_unicode_lengths(largest + 1, largest + 1));
        assert!(!valid_unicode_lengths(1, MAX_TEXT_PAYLOAD_BYTES + 1));
        assert!(!valid_unicode_lengths(usize::MAX, 0));
        assert!(valid_unicode_lengths(0, 0));
    }
}
