use super::{KeyboardTranslationProfile as Profile, TextTranslation};
use TextTranslation::{Dead, Unicode};

#[test]
fn norwegian_legacy_characters_are_preserved_for_every_recorded_chord() {
    // Frozen before the refactor; this protects the user's accepted mapping,
    // including compatibility AltGr chords that differ from ordinary layouts.
    for line in include_str!("fixtures/norwegian-0.1.31.tsv").lines() {
        if line.starts_with('#') {
            continue;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        let code = u8::from_str_radix(parts[0], 16).unwrap();
        let shifted = parts[1] == "1";
        let altgr = parts[2] == "1";
        let ch = char::from_u32(u32::from_str_radix(parts[4], 16).unwrap()).unwrap();
        let expected = if parts[3] == "Dead" {
            Dead(ch)
        } else {
            Unicode(ch)
        };
        assert_eq!(
            Profile::Norwegian.translate(code, shifted, altgr, false),
            Some(expected),
            "{line}"
        );
        if altgr {
            assert_eq!(
                Profile::Norwegian.translate(code, shifted, true, true),
                Some(expected),
                "Caps must not change {line}"
            );
        }
    }
}

#[test]
fn exact_layout_ids_and_overrides_never_guess_from_locale_or_client_name() {
    for (id, name, profile) in [
        (0x0409, "us", Profile::Us),
        (0x0809, "uk", Profile::Uk),
        (0x0414, "no", Profile::Norwegian),
        (0x041D, "se", Profile::Swedish),
        (0x0406, "dk", Profile::Danish),
        (0x0407, "de", Profile::German),
        (0x040C, "fr", Profile::French),
    ] {
        assert_eq!(Profile::from_layout_id(id), Some(profile));
        assert_eq!(Profile::from_setting("auto", id), Some(profile));
        assert_eq!(Profile::from_setting(name, 0xDEADBEEF), Some(profile));
    }
    for id in [0, 0x10409, 0x20409, 0x10407, 0x0411, 0xFFFFFFFF] {
        assert_eq!(Profile::from_setting("auto", id), None);
    }
    assert_eq!(Profile::from_setting("physical", 0x0414), None);
}

#[test]
fn locale_matrix_covers_punctuation_iso_letters_and_altgr() {
    for (profile, code, shift, altgr, expected) in [
        (Profile::Us, 0x03, true, false, Unicode('@')),
        (Profile::Us, 0x1A, false, false, Unicode('[')),
        (Profile::Uk, 0x03, true, false, Unicode('"')),
        (Profile::Uk, 0x04, true, false, Unicode('£')),
        (Profile::Uk, 0x28, true, false, Unicode('@')),
        (Profile::Uk, 0x56, false, false, Unicode('\\')),
        (Profile::Uk, 0x05, false, true, Unicode('€')),
        (Profile::Norwegian, 0x1A, false, false, Unicode('å')),
        (Profile::Norwegian, 0x27, false, false, Unicode('ø')),
        (Profile::Norwegian, 0x28, false, false, Unicode('æ')),
        (Profile::Swedish, 0x27, false, false, Unicode('ö')),
        (Profile::Swedish, 0x28, true, false, Unicode('Ä')),
        (Profile::Swedish, 0x0C, false, true, Unicode('\\')),
        (Profile::Danish, 0x27, false, false, Unicode('æ')),
        (Profile::Danish, 0x28, false, false, Unicode('ø')),
        (Profile::Danish, 0x0D, false, true, Unicode('|')),
        (Profile::German, 0x15, false, false, Unicode('z')),
        (Profile::German, 0x2C, true, false, Unicode('Y')),
        (Profile::German, 0x10, false, true, Unicode('@')),
        (Profile::German, 0x0C, false, true, Unicode('\\')),
        (Profile::German, 0x56, false, true, Unicode('|')),
        (Profile::French, 0x10, false, false, Unicode('a')),
        (Profile::French, 0x1E, false, false, Unicode('q')),
        (Profile::French, 0x27, false, false, Unicode('m')),
        (Profile::French, 0x02, false, false, Unicode('&')),
        (Profile::French, 0x02, true, false, Unicode('1')),
        (Profile::French, 0x0B, false, true, Unicode('@')),
        (Profile::French, 0x09, false, true, Unicode('\\')),
        (Profile::French, 0x12, false, true, Unicode('€')),
    ] {
        assert_eq!(
            profile.translate(code, shift, altgr, false),
            Some(expected),
            "{profile:?} scan={code:#x}"
        );
    }
}

#[test]
fn caps_and_dead_keys_follow_each_layout_not_host_state() {
    assert_eq!(
        Profile::Us.translate(0x02, false, false, true),
        Some(Unicode('1'))
    );
    assert_eq!(
        Profile::Norwegian.translate(0x28, false, false, true),
        Some(Unicode('Æ'))
    );
    assert_eq!(
        Profile::Norwegian.translate(0x28, true, false, true),
        Some(Unicode('æ'))
    );
    assert_eq!(
        Profile::Uk.translate(0x12, false, true, true),
        Some(Unicode('É'))
    );
    assert_eq!(
        Profile::Uk.translate(0x12, true, true, true),
        Some(Unicode('é'))
    );
    assert_eq!(
        Profile::German.translate(0x03, false, false, true),
        Some(Unicode('"'))
    );
    assert_eq!(
        Profile::French.translate(0x03, false, false, true),
        Some(Unicode('2'))
    );
    assert_eq!(
        Profile::French.translate(0x03, true, false, true),
        Some(Unicode('é'))
    );
    assert_eq!(
        Profile::French.translate(0x1A, false, false, true),
        Some(Dead('¨'))
    );
    assert_eq!(
        Profile::French.translate(0x1A, true, false, true),
        Some(Dead('^'))
    );
    assert_eq!(
        Profile::German.translate(0x29, false, false, false),
        Some(Dead('^'))
    );
    assert_eq!(
        Profile::Swedish.translate(0x0D, false, false, false),
        Some(Dead('´'))
    );
    assert_eq!(
        Profile::Danish.translate(0x1B, false, true, false),
        Some(Dead('~'))
    );
}

#[test]
fn french_and_german_altgr_match_windows_dead_key_and_caps_semantics() {
    // Verified against Microsoft's kbdfr/kbdgr Windows keyboard samples.
    // Caps affects the primary levels of these layouts, but not AltGr.
    for caps in [false, true] {
        for (profile, code, expected) in [
            (Profile::French, 0x03, Dead('~')),
            (Profile::French, 0x08, Dead('`')),
            (Profile::French, 0x0A, Unicode('^')),
            (Profile::French, 0x1B, Unicode('¤')),
            (Profile::German, 0x0C, Unicode('\\')),
            (Profile::German, 0x1B, Unicode('~')),
        ] {
            assert_eq!(profile.translate(code, false, true, caps), Some(expected));
            assert_eq!(profile.translate(code, true, true, caps), None);
        }
    }

    for (profile, code, unshifted, shifted) in [
        (Profile::French, 0x02, Unicode('&'), Unicode('1')),
        (Profile::French, 0x1A, Dead('^'), Dead('¨')),
        (Profile::French, 0x2B, Unicode('*'), Unicode('µ')),
        (Profile::French, 0x35, Unicode('!'), Unicode('§')),
        (Profile::German, 0x02, Unicode('1'), Unicode('!')),
        (Profile::German, 0x1B, Unicode('+'), Unicode('*')),
        (Profile::German, 0x2B, Unicode('#'), Unicode('\'')),
    ] {
        assert_eq!(profile.translate(code, false, false, true), Some(shifted));
        assert_eq!(profile.translate(code, true, false, true), Some(unshifted));
    }
    // Unlike adjacent punctuation, these keys ignore Caps Lock.
    for caps in [false, true] {
        assert_eq!(
            Profile::German.translate(0x0D, false, false, caps),
            Some(Dead('´'))
        );
        assert_eq!(
            Profile::German.translate(0x35, false, false, caps),
            Some(Unicode('-'))
        );
        assert_eq!(
            Profile::French.translate(0x56, false, false, caps),
            Some(Unicode('<'))
        );
    }
}

#[test]
fn navigation_and_extended_scan_code_positions_are_not_text_keys() {
    for profile in [
        Profile::Us,
        Profile::Uk,
        Profile::Norwegian,
        Profile::Swedish,
        Profile::Danish,
        Profile::German,
        Profile::French,
    ] {
        for code in [0x01, 0x0E, 0x0F, 0x1C, 0x1D, 0x2A, 0x38, 0x48, 0x5B, 0xFF] {
            assert_eq!(
                profile.translate(code, false, false, false),
                None,
                "{profile:?} scan={code:#x}"
            );
        }
    }
}

#[test]
fn every_supported_layout_has_complete_basic_rows_and_iso_key() {
    for profile in [
        Profile::Us,
        Profile::Uk,
        Profile::Norwegian,
        Profile::Swedish,
        Profile::Danish,
        Profile::German,
        Profile::French,
    ] {
        for code in [0x29, 0x2B, 0x39, 0x56]
            .into_iter()
            .chain(0x02..=0x0D)
            .chain(0x10..=0x1B)
            .chain(0x1E..=0x28)
            .chain(0x2C..=0x35)
        {
            for shifted in [false, true] {
                if profile == Profile::French && code == 0x29 && shifted {
                    assert_eq!(profile.translate(code, shifted, false, false), None);
                    continue;
                }
                assert!(
                    profile.translate(code, shifted, false, false).is_some(),
                    "{profile:?} scan={code:#x} shift={shifted}"
                );
            }
        }
    }
}
