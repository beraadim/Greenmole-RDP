/*
 * Licensed to the Apache Software Foundation (ASF) under one
 * or more contributor license agreements. See the NOTICE file
 * distributed with this work for additional information
 * regarding copyright ownership. The ASF licenses this file
 * to you under the Apache License, Version 2.0 (the
 * "License"); you may not use this file except in compliance
 * with the License. You may obtain a copy of the License at
 *
 *   http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing,
 * software distributed under the License is distributed on an
 * "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
 * KIND, either express or implied. See the License for the
 * specific language governing permissions and limitations
 * under the License.
 */

//! Adapted from Apache Guacamole's en_gb, sv_se, da_dk, de_de and fr_fr keymaps.
//! Downstream changes: invert keysyms-to-scancodes into compact scan-code rows,
//! retain Greenmole RDP's existing US/Norwegian behavior, and resolve ambiguous
//! dead-key alternatives to the physical Windows key's normal output.
//! French dead-key and German AltGr semantics were checked against Microsoft's
//! Windows-driver-samples/input/layout/all_kbds/{kbdfr,kbdgr} layout definitions.
//! See this directory's NOTICE and LICENSE-APACHE for attribution.

use super::{KeyboardTranslationProfile as Profile, TextTranslation};
use TextTranslation::{Dead, Unicode};

struct Rows {
    numbers: [&'static str; 2], // 0x29, 0x02..=0x0D
    upper: [&'static str; 2],   // 0x10..=0x1B
    home: [&'static str; 2],    // 0x1E..=0x28, 0x2B
    lower: [&'static str; 2],   // 0x2C..=0x35
    iso: [char; 2],             // 0x56: the extra key next to left Shift
}

const US: Rows = Rows {
    numbers: ["`1234567890-=", "~!@#$%^&*()_+"],
    upper: ["qwertyuiop[]", "QWERTYUIOP{}"],
    home: ["asdfghjkl;'\\", "ASDFGHJKL:\"|"],
    lower: ["zxcvbnm,./", "ZXCVBNM<>?"],
    iso: ['\\', '|'],
};
const UK: Rows = Rows {
    numbers: ["`1234567890-=", "¬!\"£$%^&*()_+"],
    upper: ["qwertyuiop[]", "QWERTYUIOP{}"],
    home: ["asdfghjkl;'#", "ASDFGHJKL:@~"],
    lower: ["zxcvbnm,./", "ZXCVBNM<>?"],
    iso: ['\\', '|'],
};
const NO: Rows = Rows {
    numbers: ["|1234567890+\\", "§!\"#¤%&/()=?`"],
    upper: ["qwertyuiopå¨", "QWERTYUIOPÅ^"],
    home: ["asdfghjkløæ'", "ASDFGHJKLØÆ*"],
    lower: ["zxcvbnm,.-", "ZXCVBNM;:_"],
    iso: ['<', '>'],
};
const SE: Rows = Rows {
    numbers: ["§1234567890+´", "½!\"#¤%&/()=?`"],
    upper: ["qwertyuiopå¨", "QWERTYUIOPÅ^"],
    home: ["asdfghjklöä'", "ASDFGHJKLÖÄ*"],
    lower: ["zxcvbnm,.-", "ZXCVBNM;:_"],
    iso: ['<', '>'],
};
const DK: Rows = Rows {
    numbers: ["§1234567890+´", "½!\"#¤%&/()=?`"],
    upper: ["qwertyuiopå¨", "QWERTYUIOPÅ^"],
    home: ["asdfghjklæø'", "ASDFGHJKLÆØ*"],
    lower: ["zxcvbnm,.-", "ZXCVBNM;:_"],
    iso: ['<', '>'],
};
const DE: Rows = Rows {
    numbers: ["^1234567890ß´", "°!\"§$%&/()=?`"],
    upper: ["qwertzuiopü+", "QWERTZUIOPÜ*"],
    home: ["asdfghjklöä#", "ASDFGHJKLÖÄ'"],
    lower: ["yxcvbnm,.-", "YXCVBNM;:_"],
    iso: ['<', '>'],
};
const FR: Rows = Rows {
    numbers: ["²&é\"'(-è_çà)=", "²1234567890°+"],
    upper: ["azertyuiop^$", "AZERTYUIOP¨£"],
    home: ["qsdfghjklmù*", "QSDFGHJKLM%µ"],
    lower: ["wxcvbn,;:!", "WXCVBN?./§"],
    iso: ['<', '>'],
};

pub(super) fn base(profile: Profile, code: u8, shifted: bool) -> Option<TextTranslation> {
    if profile == Profile::French && code == 0x29 && shifted {
        return None;
    }
    let row = match profile {
        Profile::Us => &US,
        Profile::Uk => &UK,
        Profile::Norwegian => &NO,
        Profile::Swedish => &SE,
        Profile::Danish => &DK,
        Profile::German => &DE,
        Profile::French => &FR,
    };
    let shift = usize::from(shifted);
    let ch = match code {
        0x29 => row.numbers[shift].chars().next(),
        0x02..=0x0D => row.numbers[shift].chars().nth(usize::from(code - 1)),
        0x10..=0x1B => row.upper[shift].chars().nth(usize::from(code - 0x10)),
        0x1E..=0x28 => row.home[shift].chars().nth(usize::from(code - 0x1E)),
        0x2B => row.home[shift].chars().nth(11),
        0x2C..=0x35 => row.lower[shift].chars().nth(usize::from(code - 0x2C)),
        0x39 => Some(' '),
        0x56 => Some(row.iso[shift]),
        _ => None,
    }?;
    let dead = match profile {
        Profile::Norwegian => code == 0x1B || (code == 0x0D && shifted),
        Profile::Swedish | Profile::Danish => code == 0x1B || code == 0x0D,
        Profile::German => code == 0x0D || (code == 0x29 && !shifted),
        Profile::French => code == 0x1A,
        Profile::Us | Profile::Uk => false,
    };
    Some(if dead { Dead(ch) } else { Unicode(ch) })
}

pub(super) fn altgr(
    profile: Profile,
    code: u8,
    shifted: bool,
    caps: bool,
) -> Option<TextTranslation> {
    if profile == Profile::Uk {
        let letter = match code {
            0x12 => Some(('é', 'É')),
            0x16 => Some(('ú', 'Ú')),
            0x17 => Some(('í', 'Í')),
            0x18 => Some(('ó', 'Ó')),
            0x1E => Some(('á', 'Á')),
            0x2E => Some(('ç', 'Ç')),
            _ => None,
        };
        if let Some((lower, upper)) = letter {
            return Some(Unicode(if shifted ^ caps { upper } else { lower }));
        }
    }
    // Norwegian's existing compatibility mapping intentionally accepts Shift
    // with AltGr. Other layouts only define their ordinary AltGr level here.
    if shifted && profile != Profile::Norwegian {
        return None;
    }
    match profile {
        Profile::Us => None,
        Profile::Uk => match code {
            0x29 => Some(Unicode('¦')),
            0x05 => Some(Unicode('€')),
            _ => None,
        },
        Profile::Norwegian | Profile::Swedish | Profile::Danish => match code {
            0x03 => Some(Unicode('@')),
            0x04 => Some(Unicode('£')),
            0x05 => Some(Unicode('$')),
            0x06 if profile == Profile::Norwegian => Some(Unicode('€')),
            0x08 => Some(Unicode('{')),
            0x09 => Some(Unicode('[')),
            0x0A => Some(Unicode(']')),
            0x0B => Some(Unicode('}')),
            0x0C if profile == Profile::Swedish => Some(Unicode('\\')),
            0x0D if profile == Profile::Norwegian => Some(Dead('´')),
            0x0D if profile == Profile::Danish => Some(Unicode('|')),
            0x12 => Some(Unicode('€')),
            // Keep the existing Norwegian AltGr+P tilde compatibility chord,
            // while also accepting the standard physical dead-tilde key.
            0x19 if profile == Profile::Norwegian => Some(Dead('~')),
            0x1B => Some(Dead('~')),
            0x32 => Some(Unicode('µ')),
            0x56 => Some(Unicode(if profile == Profile::Swedish {
                '|'
            } else {
                '\\'
            })),
            _ => None,
        },
        Profile::German => match code {
            0x03 => Some(Unicode('²')),
            0x04 => Some(Unicode('³')),
            0x08 => Some(Unicode('{')),
            0x09 => Some(Unicode('[')),
            0x0A => Some(Unicode(']')),
            0x0B => Some(Unicode('}')),
            0x0C => Some(Unicode('\\')),
            0x10 => Some(Unicode('@')),
            0x12 => Some(Unicode('€')),
            0x1B => Some(Unicode('~')),
            0x32 => Some(Unicode('µ')),
            0x56 => Some(Unicode('|')),
            _ => None,
        },
        Profile::French => match code {
            // Windows French treats these as composition starters, unlike
            // the literal circumflex produced by AltGr+9.
            0x03 => Some(Dead('~')),
            0x08 => Some(Dead('`')),
            0x03..=0x0D => "~#{[|`\\^@]}"
                .chars()
                .nth(usize::from(code - 0x03))
                .map(Unicode),
            0x12 => Some(Unicode('€')),
            0x1B => Some(Unicode('¤')),
            _ => None,
        },
    }
}
