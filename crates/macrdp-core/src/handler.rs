use ironrdp_server::{ClientInputContext, KeyboardEvent, MouseEvent, RdpServerInputHandler};
use macrdp_input::{KeyboardInjector, MouseButton, MouseInjector, RemoteSessionWake};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maps RDP desktop coordinates to macOS logical coordinates.
///
/// Completely independent of capture/encode resolution — only cares about
/// the RDP desktop size (what the client thinks) and the macOS CG logical
/// display size (what CGEvent expects).
///
/// Formula: `mac = rdp × cg_logical ÷ rdp_desktop`
#[derive(Debug, Clone)]
pub struct MouseCoordMapper(Arc<Mutex<CoordState>>);

#[derive(Debug)]
struct CoordState {
    logical_w: f64,
    logical_h: f64,
    rdp_w: f64,
    rdp_h: f64,
}

impl MouseCoordMapper {
    pub fn new(logical_w: u16, logical_h: u16, rdp_w: u16, rdp_h: u16) -> Self {
        tracing::info!(
            logical_w,
            logical_h,
            rdp_w,
            rdp_h,
            "MouseCoordMapper initialized"
        );
        Self(Arc::new(Mutex::new(CoordState {
            logical_w: logical_w as f64,
            logical_h: logical_h as f64,
            rdp_w: rdp_w.max(1) as f64,
            rdp_h: rdp_h.max(1) as f64,
        })))
    }

    pub fn map(&self, rdp_x: u16, rdp_y: u16) -> (u16, u16) {
        let s = self.0.lock().unwrap();
        // ScreenCaptureKit capture uses aspect-fit scaling when the client asks
        // for a different desktop shape. That can introduce horizontal or
        // vertical bars inside the negotiated RDP desktop, so map against the
        // active image rect rather than the full RDP surface.
        let scale = (s.rdp_w / s.logical_w).min(s.rdp_h / s.logical_h);
        let active_w = (s.logical_w * scale).max(1.0);
        let active_h = (s.logical_h * scale).max(1.0);
        let offset_x = ((s.rdp_w - active_w) * 0.5).max(0.0);
        let offset_y = ((s.rdp_h - active_h) * 0.5).max(0.0);

        let rel_x = ((rdp_x as f64) - offset_x).clamp(0.0, active_w);
        let rel_y = ((rdp_y as f64) - offset_y).clamp(0.0, active_h);

        let mac_x = (rel_x * s.logical_w / active_w).clamp(0.0, s.logical_w - 1.0) as u16;
        let mac_y = (rel_y * s.logical_h / active_h).clamp(0.0, s.logical_h - 1.0) as u16;
        (mac_x, mac_y)
    }

    pub fn update_rdp_size(&self, rdp_w: u16, rdp_h: u16) {
        let mut s = self.0.lock().unwrap();
        s.rdp_w = rdp_w.max(1) as f64;
        s.rdp_h = rdp_h.max(1) as f64;
        tracing::info!(
            new_rdp_w = rdp_w,
            new_rdp_h = rdp_h,
            logical_w = s.logical_w,
            logical_h = s.logical_h,
            "MouseCoordMapper: RDP desktop size updated"
        );
    }
}

/// Bridges RDP input events to macOS CGEvent injection
pub struct MacInputHandler {
    keyboard: Option<KeyboardInjector>,
    mouse: Option<MouseInjector>,
    wake: Option<RemoteSessionWake>,
    last_mouse_x: u16,
    last_mouse_y: u16,
    last_sync_lock_bits: Option<u8>,
    coord_mapper: MouseCoordMapper,
    client_context: ClientInputContext,
    modifier_state: ModifierState,
    pending_dead_key: Option<char>,
    translated_key_releases: HashMap<(u8, bool), Vec<u16>>,
}

#[derive(Debug, Default, Clone, Copy)]
struct ModifierState {
    left_shift: bool,
    right_shift: bool,
    left_ctrl: bool,
    right_ctrl: bool,
    left_alt: bool,
    right_alt: bool,
    left_win: bool,
    right_win: bool,
}

impl ModifierState {
    fn shift(self) -> bool {
        self.left_shift || self.right_shift
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextTranslation {
    Unicode(char),
    Dead(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardTranslationProfile {
    GuacamoleUs,
    NorwegianPhysical,
}

impl MacInputHandler {
    pub fn new(coord_mapper: MouseCoordMapper) -> Self {
        let keyboard = KeyboardInjector::new()
            .map_err(|e| tracing::error!("Failed to create keyboard injector: {e}"))
            .ok();
        let mouse = MouseInjector::new()
            .map_err(|e| tracing::error!("Failed to create mouse injector: {e}"))
            .ok();

        if keyboard.is_none() || mouse.is_none() {
            tracing::warn!("Input injection may fail — ensure Accessibility permission is granted");
        }

        let mut wake = RemoteSessionWake::new()
            .map_err(|e| tracing::warn!("Failed to initialize remote wake support: {e}"))
            .ok();
        if let Some(wake_session) = &mut wake {
            if let Err(e) = wake_session.wake_display_for_session_start() {
                tracing::warn!("Failed to wake display for new remote session: {e}");
            }
        }

        Self {
            keyboard,
            mouse,
            wake,
            last_mouse_x: 0,
            last_mouse_y: 0,
            last_sync_lock_bits: None,
            coord_mapper,
            client_context: ClientInputContext::default(),
            modifier_state: ModifierState::default(),
            pending_dead_key: None,
            translated_key_releases: HashMap::new(),
        }
    }

    fn map_right_alt_to_option(&self) -> bool {
        let layout = self.client_context.keyboard_layout;
        let is_non_us_layout = layout != 0 && layout != 0x0409;
        let is_guacamole = self.client_context.client_name.contains("Guacamole");
        is_non_us_layout || is_guacamole
    }

    fn treat_right_ctrl_as_altgr_companion(&self) -> bool {
        matches!(
            self.translation_profile(),
            Some(KeyboardTranslationProfile::NorwegianPhysical)
        )
    }

    fn translation_profile(&self) -> Option<KeyboardTranslationProfile> {
        if self.client_context.client_name.contains("Guacamole") {
            Some(KeyboardTranslationProfile::GuacamoleUs)
        } else if self.client_context.keyboard_layout == 0x0414 {
            Some(KeyboardTranslationProfile::NorwegianPhysical)
        } else {
            None
        }
    }

    fn translate_guacamole_us_text_key(&self, code: u8, shifted: bool) -> Option<TextTranslation> {
        match (code, shifted) {
            (0x29, false) => Some(TextTranslation::Unicode('`')),
            (0x29, true) => Some(TextTranslation::Unicode('~')),
            (0x02, false) => Some(TextTranslation::Unicode('1')),
            (0x02, true) => Some(TextTranslation::Unicode('!')),
            (0x03, false) => Some(TextTranslation::Unicode('2')),
            (0x03, true) => Some(TextTranslation::Unicode('@')),
            (0x04, false) => Some(TextTranslation::Unicode('3')),
            (0x04, true) => Some(TextTranslation::Unicode('#')),
            (0x05, false) => Some(TextTranslation::Unicode('4')),
            (0x05, true) => Some(TextTranslation::Unicode('$')),
            (0x06, false) => Some(TextTranslation::Unicode('5')),
            (0x06, true) => Some(TextTranslation::Unicode('%')),
            (0x07, false) => Some(TextTranslation::Unicode('6')),
            (0x07, true) => Some(TextTranslation::Unicode('^')),
            (0x08, false) => Some(TextTranslation::Unicode('7')),
            (0x08, true) => Some(TextTranslation::Unicode('&')),
            (0x09, false) => Some(TextTranslation::Unicode('8')),
            (0x09, true) => Some(TextTranslation::Unicode('*')),
            (0x0A, false) => Some(TextTranslation::Unicode('9')),
            (0x0A, true) => Some(TextTranslation::Unicode('(')),
            (0x0B, false) => Some(TextTranslation::Unicode('0')),
            (0x0B, true) => Some(TextTranslation::Unicode(')')),
            (0x0C, false) => Some(TextTranslation::Unicode('-')),
            (0x0C, true) => Some(TextTranslation::Unicode('_')),
            (0x0D, false) => Some(TextTranslation::Unicode('=')),
            (0x0D, true) => Some(TextTranslation::Unicode('+')),
            (0x10, false) => Some(TextTranslation::Unicode('q')),
            (0x10, true) => Some(TextTranslation::Unicode('Q')),
            (0x11, false) => Some(TextTranslation::Unicode('w')),
            (0x11, true) => Some(TextTranslation::Unicode('W')),
            (0x12, false) => Some(TextTranslation::Unicode('e')),
            (0x12, true) => Some(TextTranslation::Unicode('E')),
            (0x13, false) => Some(TextTranslation::Unicode('r')),
            (0x13, true) => Some(TextTranslation::Unicode('R')),
            (0x14, false) => Some(TextTranslation::Unicode('t')),
            (0x14, true) => Some(TextTranslation::Unicode('T')),
            (0x15, false) => Some(TextTranslation::Unicode('y')),
            (0x15, true) => Some(TextTranslation::Unicode('Y')),
            (0x16, false) => Some(TextTranslation::Unicode('u')),
            (0x16, true) => Some(TextTranslation::Unicode('U')),
            (0x17, false) => Some(TextTranslation::Unicode('i')),
            (0x17, true) => Some(TextTranslation::Unicode('I')),
            (0x18, false) => Some(TextTranslation::Unicode('o')),
            (0x18, true) => Some(TextTranslation::Unicode('O')),
            (0x19, false) => Some(TextTranslation::Unicode('p')),
            (0x19, true) => Some(TextTranslation::Unicode('P')),
            (0x1A, false) => Some(TextTranslation::Unicode('[')),
            (0x1A, true) => Some(TextTranslation::Unicode('{')),
            (0x1B, false) => Some(TextTranslation::Unicode(']')),
            (0x1B, true) => Some(TextTranslation::Unicode('}')),
            (0x1E, false) => Some(TextTranslation::Unicode('a')),
            (0x1E, true) => Some(TextTranslation::Unicode('A')),
            (0x1F, false) => Some(TextTranslation::Unicode('s')),
            (0x1F, true) => Some(TextTranslation::Unicode('S')),
            (0x20, false) => Some(TextTranslation::Unicode('d')),
            (0x20, true) => Some(TextTranslation::Unicode('D')),
            (0x21, false) => Some(TextTranslation::Unicode('f')),
            (0x21, true) => Some(TextTranslation::Unicode('F')),
            (0x22, false) => Some(TextTranslation::Unicode('g')),
            (0x22, true) => Some(TextTranslation::Unicode('G')),
            (0x23, false) => Some(TextTranslation::Unicode('h')),
            (0x23, true) => Some(TextTranslation::Unicode('H')),
            (0x24, false) => Some(TextTranslation::Unicode('j')),
            (0x24, true) => Some(TextTranslation::Unicode('J')),
            (0x25, false) => Some(TextTranslation::Unicode('k')),
            (0x25, true) => Some(TextTranslation::Unicode('K')),
            (0x26, false) => Some(TextTranslation::Unicode('l')),
            (0x26, true) => Some(TextTranslation::Unicode('L')),
            (0x27, false) => Some(TextTranslation::Unicode(';')),
            (0x27, true) => Some(TextTranslation::Unicode(':')),
            (0x28, false) => Some(TextTranslation::Unicode('\'')),
            (0x28, true) => Some(TextTranslation::Unicode('"')),
            (0x2B, false) => Some(TextTranslation::Unicode('\\')),
            (0x2B, true) => Some(TextTranslation::Unicode('|')),
            (0x2C, false) => Some(TextTranslation::Unicode('z')),
            (0x2C, true) => Some(TextTranslation::Unicode('Z')),
            (0x2D, false) => Some(TextTranslation::Unicode('x')),
            (0x2D, true) => Some(TextTranslation::Unicode('X')),
            (0x2E, false) => Some(TextTranslation::Unicode('c')),
            (0x2E, true) => Some(TextTranslation::Unicode('C')),
            (0x2F, false) => Some(TextTranslation::Unicode('v')),
            (0x2F, true) => Some(TextTranslation::Unicode('V')),
            (0x30, false) => Some(TextTranslation::Unicode('b')),
            (0x30, true) => Some(TextTranslation::Unicode('B')),
            (0x31, false) => Some(TextTranslation::Unicode('n')),
            (0x31, true) => Some(TextTranslation::Unicode('N')),
            (0x32, false) => Some(TextTranslation::Unicode('m')),
            (0x32, true) => Some(TextTranslation::Unicode('M')),
            (0x33, false) => Some(TextTranslation::Unicode(',')),
            (0x33, true) => Some(TextTranslation::Unicode('<')),
            (0x34, false) => Some(TextTranslation::Unicode('.')),
            (0x34, true) => Some(TextTranslation::Unicode('>')),
            (0x35, false) => Some(TextTranslation::Unicode('/')),
            (0x35, true) => Some(TextTranslation::Unicode('?')),
            (0x39, false) | (0x39, true) => Some(TextTranslation::Unicode(' ')),
            _ => None,
        }
    }

    fn translate_norwegian_text_key(&self, code: u8, shifted: bool, altgr: bool) -> Option<TextTranslation> {
        if altgr {
            return match code {
                0x03 => Some(TextTranslation::Unicode('@')),
                0x04 => Some(TextTranslation::Unicode('£')),
                0x05 => Some(TextTranslation::Unicode('$')),
                0x06 => Some(TextTranslation::Unicode('€')),
                0x08 => Some(TextTranslation::Unicode('{')),
                0x09 => Some(TextTranslation::Unicode('[')),
                0x0A => Some(TextTranslation::Unicode(']')),
                0x0B => Some(TextTranslation::Unicode('}')),
                0x0D => Some(TextTranslation::Dead('´')),
                0x12 => Some(TextTranslation::Unicode('€')),
                0x19 => Some(TextTranslation::Dead('~')),
                0x32 => Some(TextTranslation::Unicode('µ')),
                0x56 => Some(TextTranslation::Unicode('\\')),
                _ => None,
            };
        }

        match (code, shifted) {
            (0x29, false) => Some(TextTranslation::Unicode('|')),
            (0x29, true) => Some(TextTranslation::Unicode('§')),
            (0x02, false) => Some(TextTranslation::Unicode('1')),
            (0x02, true) => Some(TextTranslation::Unicode('!')),
            (0x03, false) => Some(TextTranslation::Unicode('2')),
            (0x03, true) => Some(TextTranslation::Unicode('"')),
            (0x04, false) => Some(TextTranslation::Unicode('3')),
            (0x04, true) => Some(TextTranslation::Unicode('#')),
            (0x05, false) => Some(TextTranslation::Unicode('4')),
            (0x05, true) => Some(TextTranslation::Unicode('¤')),
            (0x06, false) => Some(TextTranslation::Unicode('5')),
            (0x06, true) => Some(TextTranslation::Unicode('%')),
            (0x07, false) => Some(TextTranslation::Unicode('6')),
            (0x07, true) => Some(TextTranslation::Unicode('&')),
            (0x08, false) => Some(TextTranslation::Unicode('7')),
            (0x08, true) => Some(TextTranslation::Unicode('/')),
            (0x09, false) => Some(TextTranslation::Unicode('8')),
            (0x09, true) => Some(TextTranslation::Unicode('(')),
            (0x0A, false) => Some(TextTranslation::Unicode('9')),
            (0x0A, true) => Some(TextTranslation::Unicode(')')),
            (0x0B, false) => Some(TextTranslation::Unicode('0')),
            (0x0B, true) => Some(TextTranslation::Unicode('=')),
            (0x0C, false) => Some(TextTranslation::Unicode('+')),
            (0x0C, true) => Some(TextTranslation::Unicode('?')),
            (0x0D, false) => Some(TextTranslation::Unicode('\\')),
            (0x0D, true) => Some(TextTranslation::Dead('`')),
            (0x10, false) => Some(TextTranslation::Unicode('q')),
            (0x10, true) => Some(TextTranslation::Unicode('Q')),
            (0x11, false) => Some(TextTranslation::Unicode('w')),
            (0x11, true) => Some(TextTranslation::Unicode('W')),
            (0x12, false) => Some(TextTranslation::Unicode('e')),
            (0x12, true) => Some(TextTranslation::Unicode('E')),
            (0x13, false) => Some(TextTranslation::Unicode('r')),
            (0x13, true) => Some(TextTranslation::Unicode('R')),
            (0x14, false) => Some(TextTranslation::Unicode('t')),
            (0x14, true) => Some(TextTranslation::Unicode('T')),
            (0x15, false) => Some(TextTranslation::Unicode('y')),
            (0x15, true) => Some(TextTranslation::Unicode('Y')),
            (0x16, false) => Some(TextTranslation::Unicode('u')),
            (0x16, true) => Some(TextTranslation::Unicode('U')),
            (0x17, false) => Some(TextTranslation::Unicode('i')),
            (0x17, true) => Some(TextTranslation::Unicode('I')),
            (0x18, false) => Some(TextTranslation::Unicode('o')),
            (0x18, true) => Some(TextTranslation::Unicode('O')),
            (0x19, false) => Some(TextTranslation::Unicode('p')),
            (0x19, true) => Some(TextTranslation::Unicode('P')),
            (0x1A, false) => Some(TextTranslation::Unicode('å')),
            (0x1A, true) => Some(TextTranslation::Unicode('Å')),
            (0x1B, false) => Some(TextTranslation::Dead('¨')),
            (0x1B, true) => Some(TextTranslation::Dead('^')),
            (0x1E, false) => Some(TextTranslation::Unicode('a')),
            (0x1E, true) => Some(TextTranslation::Unicode('A')),
            (0x1F, false) => Some(TextTranslation::Unicode('s')),
            (0x1F, true) => Some(TextTranslation::Unicode('S')),
            (0x20, false) => Some(TextTranslation::Unicode('d')),
            (0x20, true) => Some(TextTranslation::Unicode('D')),
            (0x21, false) => Some(TextTranslation::Unicode('f')),
            (0x21, true) => Some(TextTranslation::Unicode('F')),
            (0x22, false) => Some(TextTranslation::Unicode('g')),
            (0x22, true) => Some(TextTranslation::Unicode('G')),
            (0x23, false) => Some(TextTranslation::Unicode('h')),
            (0x23, true) => Some(TextTranslation::Unicode('H')),
            (0x24, false) => Some(TextTranslation::Unicode('j')),
            (0x24, true) => Some(TextTranslation::Unicode('J')),
            (0x25, false) => Some(TextTranslation::Unicode('k')),
            (0x25, true) => Some(TextTranslation::Unicode('K')),
            (0x26, false) => Some(TextTranslation::Unicode('l')),
            (0x26, true) => Some(TextTranslation::Unicode('L')),
            (0x27, false) => Some(TextTranslation::Unicode('ø')),
            (0x27, true) => Some(TextTranslation::Unicode('Ø')),
            (0x28, false) => Some(TextTranslation::Unicode('æ')),
            (0x28, true) => Some(TextTranslation::Unicode('Æ')),
            (0x2B, false) => Some(TextTranslation::Unicode('\'')),
            (0x2B, true) => Some(TextTranslation::Unicode('*')),
            (0x2C, false) => Some(TextTranslation::Unicode('z')),
            (0x2C, true) => Some(TextTranslation::Unicode('Z')),
            (0x2D, false) => Some(TextTranslation::Unicode('x')),
            (0x2D, true) => Some(TextTranslation::Unicode('X')),
            (0x2E, false) => Some(TextTranslation::Unicode('c')),
            (0x2E, true) => Some(TextTranslation::Unicode('C')),
            (0x2F, false) => Some(TextTranslation::Unicode('v')),
            (0x2F, true) => Some(TextTranslation::Unicode('V')),
            (0x30, false) => Some(TextTranslation::Unicode('b')),
            (0x30, true) => Some(TextTranslation::Unicode('B')),
            (0x31, false) => Some(TextTranslation::Unicode('n')),
            (0x31, true) => Some(TextTranslation::Unicode('N')),
            (0x32, false) => Some(TextTranslation::Unicode('m')),
            (0x32, true) => Some(TextTranslation::Unicode('M')),
            (0x33, false) => Some(TextTranslation::Unicode(',')),
            (0x33, true) => Some(TextTranslation::Unicode(';')),
            (0x34, false) => Some(TextTranslation::Unicode('.')),
            (0x34, true) => Some(TextTranslation::Unicode(':')),
            (0x35, false) => Some(TextTranslation::Unicode('-')),
            (0x35, true) => Some(TextTranslation::Unicode('_')),
            (0x39, false) | (0x39, true) => Some(TextTranslation::Unicode(' ')),
            (0x56, false) => Some(TextTranslation::Unicode('<')),
            (0x56, true) => Some(TextTranslation::Unicode('>')),
            _ => None,
        }
    }

    fn translate_text_key(&self, code: u8, extended: bool) -> Option<TextTranslation> {
        if extended {
            return None;
        }

        let profile = self.translation_profile()?;
        let shifted = self.modifier_state.shift();
        let altgr = self.modifier_state.right_alt;
        let ctrl_blocks_text = self.modifier_state.left_ctrl
            || (self.modifier_state.right_ctrl
                && !(matches!(profile, KeyboardTranslationProfile::NorwegianPhysical) && altgr));
        let other_blocks_text =
            self.modifier_state.left_alt || self.modifier_state.left_win || self.modifier_state.right_win;

        if ctrl_blocks_text || other_blocks_text {
            return None;
        }

        match profile {
            KeyboardTranslationProfile::GuacamoleUs => self.translate_guacamole_us_text_key(code, shifted),
            KeyboardTranslationProfile::NorwegianPhysical => {
                self.translate_norwegian_text_key(code, shifted, altgr)
            }
        }
    }

    fn compose_dead_key(accent: char, base: char) -> Option<char> {
        match (accent, base) {
            ('¨', 'a') => Some('ä'),
            ('¨', 'e') => Some('ë'),
            ('¨', 'i') => Some('ï'),
            ('¨', 'o') => Some('ö'),
            ('¨', 'u') => Some('ü'),
            ('¨', 'y') => Some('ÿ'),
            ('¨', 'A') => Some('Ä'),
            ('¨', 'E') => Some('Ë'),
            ('¨', 'I') => Some('Ï'),
            ('¨', 'O') => Some('Ö'),
            ('¨', 'U') => Some('Ü'),
            ('¨', ' ') => Some('¨'),
            ('^', 'a') => Some('â'),
            ('^', 'e') => Some('ê'),
            ('^', 'i') => Some('î'),
            ('^', 'o') => Some('ô'),
            ('^', 'u') => Some('û'),
            ('^', 'A') => Some('Â'),
            ('^', 'E') => Some('Ê'),
            ('^', 'I') => Some('Î'),
            ('^', 'O') => Some('Ô'),
            ('^', 'U') => Some('Û'),
            ('^', ' ') => Some('^'),
            ('~', 'a') => Some('ã'),
            ('~', 'o') => Some('õ'),
            ('~', 'n') => Some('ñ'),
            ('~', 'A') => Some('Ã'),
            ('~', 'O') => Some('Õ'),
            ('~', 'N') => Some('Ñ'),
            ('~', ' ') => Some('~'),
            ('´', 'a') => Some('á'),
            ('´', 'e') => Some('é'),
            ('´', 'i') => Some('í'),
            ('´', 'o') => Some('ó'),
            ('´', 'u') => Some('ú'),
            ('´', 'y') => Some('ý'),
            ('´', 'A') => Some('Á'),
            ('´', 'E') => Some('É'),
            ('´', 'I') => Some('Í'),
            ('´', 'O') => Some('Ó'),
            ('´', 'U') => Some('Ú'),
            ('´', 'Y') => Some('Ý'),
            ('´', ' ') => Some('´'),
            ('`', 'a') => Some('à'),
            ('`', 'e') => Some('è'),
            ('`', 'i') => Some('ì'),
            ('`', 'o') => Some('ò'),
            ('`', 'u') => Some('ù'),
            ('`', 'A') => Some('À'),
            ('`', 'E') => Some('È'),
            ('`', 'I') => Some('Ì'),
            ('`', 'O') => Some('Ò'),
            ('`', 'U') => Some('Ù'),
            ('`', ' ') => Some('`'),
            _ => None,
        }
    }

    fn char_to_u16(ch: char) -> Option<u16> {
        u16::try_from(u32::from(ch)).ok()
    }

    fn inject_unicode_sequence(kb: &KeyboardInjector, chars: &[u16], pressed: bool) -> anyhow::Result<()> {
        if pressed {
            for ch in chars {
                kb.inject_unicode(*ch, true)?;
            }
        } else {
            for ch in chars.iter().rev() {
                kb.inject_unicode(*ch, false)?;
            }
        }

        Ok(())
    }

    fn handle_translated_key_press(&mut self, code: u8, extended: bool) -> Option<anyhow::Result<()>> {
        let translation = self.translate_text_key(code, extended)?;
        let kb = self.keyboard.as_ref().expect("keyboard checked above");

        let mut emitted = Vec::new();
        let result = match translation {
            TextTranslation::Unicode(ch) => {
                if let Some(accent) = self.pending_dead_key.take() {
                    if let Some(composed) = Self::compose_dead_key(accent, ch) {
                        if let Some(unit) = Self::char_to_u16(composed) {
                            emitted.push(unit);
                            Self::inject_unicode_sequence(kb, &emitted, true)
                        } else {
                            Ok(())
                        }
                    } else {
                        let Some(accent_unit) = Self::char_to_u16(accent) else {
                            return Some(Ok(()));
                        };
                        let Some(ch_unit) = Self::char_to_u16(ch) else {
                            return Some(Ok(()));
                        };
                        emitted.extend([accent_unit, ch_unit]);
                        Self::inject_unicode_sequence(kb, &[accent_unit], true)
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[accent_unit], false))
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[ch_unit], true))
                    }
                } else if let Some(unit) = Self::char_to_u16(ch) {
                    emitted.push(unit);
                    Self::inject_unicode_sequence(kb, &emitted, true)
                } else {
                    Ok(())
                }
            }
            TextTranslation::Dead(accent) => {
                if let Some(previous) = self.pending_dead_key.replace(accent) {
                    if let Some(unit) = Self::char_to_u16(previous) {
                        Self::inject_unicode_sequence(kb, &[unit], true)
                            .and_then(|_| Self::inject_unicode_sequence(kb, &[unit], false))
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
        };

        if !emitted.is_empty() {
            self.translated_key_releases
                .insert((code, extended), emitted);
        }

        Some(result)
    }

    fn update_modifier_state(&mut self, code: u8, extended: bool, pressed: bool) {
        match (code, extended) {
            (0x2A, false) => self.modifier_state.left_shift = pressed,
            (0x36, false) => self.modifier_state.right_shift = pressed,
            (0x1D, false) => self.modifier_state.left_ctrl = pressed,
            (0x1D, true) => self.modifier_state.right_ctrl = pressed,
            (0x38, false) => self.modifier_state.left_alt = pressed,
            (0x38, true) => self.modifier_state.right_alt = pressed,
            (0x5B, true) => self.modifier_state.left_win = pressed,
            (0x5C, true) => self.modifier_state.right_win = pressed,
            _ => {}
        }
    }
}

impl RdpServerInputHandler for MacInputHandler {
    fn update_client_context(&mut self, context: ClientInputContext) {
        tracing::info!(
            client_name = context.client_name,
            keyboard_layout = format_args!("{:#06x}", context.keyboard_layout),
            "Updated client input context"
        );
        self.client_context = context;
    }

    fn keyboard(&mut self, event: KeyboardEvent) {
        if self.keyboard.is_none() {
            return;
        }

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("keyboard") {
                tracing::warn!("Failed to declare remote keyboard activity: {e}");
            }
        }

        let result = match event {
            KeyboardEvent::Pressed { code, extended } => {
                self.update_modifier_state(code, extended, true);

                if extended && code == 0x1D && self.treat_right_ctrl_as_altgr_companion() {
                    Ok(())
                } else if extended && code == 0x38 && self.translation_profile().is_some() {
                    Ok(())
                } else if let Some(result) = self.handle_translated_key_press(code, extended) {
                    result
                } else if extended && code == 0x38 && self.map_right_alt_to_option() {
                    let kb = self.keyboard.as_ref().expect("keyboard checked above");
                    kb.inject_keycode(0x3D, true)
                } else {
                    let kb = self.keyboard.as_ref().expect("keyboard checked above");
                    kb.inject_key(code, extended, true)
                }
            }
            KeyboardEvent::Released { code, extended } => {
                self.update_modifier_state(code, extended, false);
                let kb = self.keyboard.as_ref().expect("keyboard checked above");

                if let Some(chars) = self.translated_key_releases.remove(&(code, extended)) {
                    Self::inject_unicode_sequence(kb, &chars, false)
                } else if extended && code == 0x1D && self.treat_right_ctrl_as_altgr_companion() {
                    Ok(())
                } else if extended && code == 0x38 && self.translation_profile().is_some() {
                    Ok(())
                } else if extended && code == 0x38 && self.map_right_alt_to_option() {
                    kb.inject_keycode(0x3D, false)
                } else {
                    kb.inject_key(code, extended, false)
                }
            }
            KeyboardEvent::UnicodePressed(ch) => self
                .keyboard
                .as_ref()
                .expect("keyboard checked above")
                .inject_unicode(ch, true),
            KeyboardEvent::UnicodeReleased(ch) => self
                .keyboard
                .as_ref()
                .expect("keyboard checked above")
                .inject_unicode(ch, false),
            KeyboardEvent::Synchronize(flags) => {
                let kb = self.keyboard.as_ref().expect("keyboard checked above");
                let mut result = kb.release_transient_modifiers();
                let sync_bits = flags.bits();

                if let Some(previous_bits) = self.last_sync_lock_bits {
                    let changed = previous_bits ^ sync_bits;

                    if result.is_ok() && changed & 0x04 != 0 {
                        result = kb.tap_key(0x3A, false); // Caps Lock
                    }
                    if result.is_ok() && changed & 0x02 != 0 {
                        result = kb.tap_key(0x45, false); // Num Lock / Clear
                    }
                    if result.is_ok() && changed & 0x01 != 0 {
                        result = kb.tap_key(0x46, false); // Scroll Lock / F14
                    }

                    if changed & 0x08 != 0 {
                        tracing::debug!(
                            sync_bits,
                            "KANA lock changed in synchronize event; no macOS mapping is applied"
                        );
                    }
                } else {
                    tracing::debug!(sync_bits, "Captured initial keyboard synchronize state");
                }

                self.last_sync_lock_bits = Some(sync_bits);
                self.modifier_state = ModifierState::default();
                self.pending_dead_key = None;
                self.translated_key_releases.clear();
                result
            }
        };

        if let Err(e) = result {
            tracing::warn!("Keyboard injection failed: {e}");
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(m) = &self.mouse else { return };

        if let Some(wake) = &mut self.wake {
            if let Err(e) = wake.wake_display_for_input("mouse") {
                tracing::warn!("Failed to declare remote mouse activity: {e}");
            }
        }

        let result = match event {
            MouseEvent::Move { x, y } => {
                let (mx, my) = self.coord_mapper.map(x, y);
                self.last_mouse_x = mx;
                self.last_mouse_y = my;
                m.move_to(mx, my)
            }
            MouseEvent::LeftPressed => m.button_event(
                MouseButton::Left,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::LeftReleased => m.button_event(
                MouseButton::Left,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::RightPressed => m.button_event(
                MouseButton::Right,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::RightReleased => m.button_event(
                MouseButton::Right,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::MiddlePressed => m.button_event(
                MouseButton::Middle,
                true,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::MiddleReleased => m.button_event(
                MouseButton::Middle,
                false,
                self.last_mouse_x,
                self.last_mouse_y,
            ),
            MouseEvent::VerticalScroll { value } => m.scroll(value),
            _ => {
                tracing::trace!(?event, "Unhandled mouse event");
                Ok(())
            }
        };

        if let Err(e) = result {
            tracing::warn!("Mouse injection failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClientInputContext, KeyboardTranslationProfile, MacInputHandler, ModifierState,
        MouseCoordMapper, TextTranslation,
    };

    fn handler_for(client_name: &str, keyboard_layout: u32) -> MacInputHandler {
        let mut handler = MacInputHandler::new(MouseCoordMapper::new(1920, 1200, 1920, 1200));
        handler.client_context = ClientInputContext {
            client_name: client_name.to_string(),
            keyboard_layout,
        };
        handler
    }

    #[test]
    fn guacamole_uses_norwegian_text_translation_for_comma_key() {
        let mut handler = handler_for("Guacamole RDP", 0x0409);
        assert_eq!(
            handler.translation_profile(),
            Some(KeyboardTranslationProfile::GuacamoleUs)
        );
        assert_eq!(
            handler.translate_text_key(0x33, false),
            Some(TextTranslation::Unicode(','))
        );
        handler.modifier_state.left_shift = true;
        assert_eq!(
            handler.translate_text_key(0x33, false),
            Some(TextTranslation::Unicode('<'))
        );
    }

    #[test]
    fn norwegian_star_key_translates_to_expected_characters() {
        let mut handler = handler_for("Windows App", 0x0414);
        assert_eq!(
            handler.translation_profile(),
            Some(KeyboardTranslationProfile::NorwegianPhysical)
        );
        assert_eq!(
            handler.translate_text_key(0x2B, false),
            Some(TextTranslation::Unicode('\''))
        );
        handler.modifier_state.right_shift = true;
        assert_eq!(
            handler.translate_text_key(0x2B, false),
            Some(TextTranslation::Unicode('*'))
        );
    }

    #[test]
    fn translation_is_disabled_for_shortcuts() {
        let mut handler = handler_for("Guacamole RDP", 0x0409);
        handler.modifier_state = ModifierState {
            left_ctrl: true,
            ..ModifierState::default()
        };
        assert_eq!(handler.translate_text_key(0x33, false), None);
    }

    #[test]
    fn native_norwegian_altgr_keys_translate_to_symbols() {
        let mut handler = handler_for("Windows App", 0x0414);
        handler.modifier_state.right_alt = true;

        assert_eq!(
            handler.translate_text_key(0x03, false),
            Some(TextTranslation::Unicode('@'))
        );
        assert_eq!(
            handler.translate_text_key(0x08, false),
            Some(TextTranslation::Unicode('{'))
        );
        assert_eq!(
            handler.translate_text_key(0x09, false),
            Some(TextTranslation::Unicode('['))
        );
        assert_eq!(
            handler.translate_text_key(0x0A, false),
            Some(TextTranslation::Unicode(']'))
        );
        assert_eq!(
            handler.translate_text_key(0x0B, false),
            Some(TextTranslation::Unicode('}'))
        );
        assert_eq!(
            handler.translate_text_key(0x12, false),
            Some(TextTranslation::Unicode('€'))
        );
        assert_eq!(
            handler.translate_text_key(0x32, false),
            Some(TextTranslation::Unicode('µ'))
        );
    }

    #[test]
    fn norwegian_letters_map_to_expected_glyphs() {
        let mut handler = handler_for("Windows App", 0x0414);
        assert_eq!(
            handler.translate_text_key(0x1A, false),
            Some(TextTranslation::Unicode('å'))
        );
        assert_eq!(
            handler.translate_text_key(0x27, false),
            Some(TextTranslation::Unicode('ø'))
        );
        assert_eq!(
            handler.translate_text_key(0x28, false),
            Some(TextTranslation::Unicode('æ'))
        );
        handler.modifier_state.left_shift = true;
        assert_eq!(
            handler.translate_text_key(0x27, false),
            Some(TextTranslation::Unicode('Ø'))
        );
    }

    #[test]
    fn guacamole_us_profile_matches_reported_symbol_positions() {
        let mut handler = handler_for("Guacamole RDP", 0x0409);

        handler.modifier_state.left_shift = true;
        assert_eq!(handler.translate_text_key(0x0A, false), Some(TextTranslation::Unicode('(')));
        assert_eq!(handler.translate_text_key(0x0B, false), Some(TextTranslation::Unicode(')')));
        assert_eq!(handler.translate_text_key(0x0D, false), Some(TextTranslation::Unicode('+')));
        assert_eq!(handler.translate_text_key(0x35, false), Some(TextTranslation::Unicode('?')));
        assert_eq!(handler.translate_text_key(0x28, false), Some(TextTranslation::Unicode('"')));
        assert_eq!(handler.translate_text_key(0x2B, false), Some(TextTranslation::Unicode('|')));
        assert_eq!(handler.translate_text_key(0x33, false), Some(TextTranslation::Unicode('<')));

        handler.modifier_state = ModifierState::default();
        assert_eq!(handler.translate_text_key(0x1A, false), Some(TextTranslation::Unicode('[')));
        assert_eq!(handler.translate_text_key(0x1B, false), Some(TextTranslation::Unicode(']')));
        assert_eq!(handler.translate_text_key(0x0C, false), Some(TextTranslation::Unicode('-')));
        assert_eq!(handler.translate_text_key(0x2B, false), Some(TextTranslation::Unicode('\\')));
        assert_eq!(handler.translate_text_key(0x35, false), Some(TextTranslation::Unicode('/')));
    }

    #[test]
    fn native_norwegian_allows_altgr_even_if_right_ctrl_is_also_set() {
        let mut handler = handler_for("Windows App", 0x0414);
        handler.modifier_state.right_alt = true;
        handler.modifier_state.right_ctrl = true;

        assert_eq!(
            handler.translate_text_key(0x09, false),
            Some(TextTranslation::Unicode('['))
        );
    }

    #[test]
    fn native_norwegian_treats_right_ctrl_as_altgr_companion() {
        let handler = handler_for("Windows App", 0x0414);
        assert!(handler.treat_right_ctrl_as_altgr_companion());

        let handler = handler_for("Guacamole RDP", 0x0409);
        assert!(!handler.treat_right_ctrl_as_altgr_companion());
    }

    #[test]
    fn dead_key_composition_matches_expected_output() {
        assert_eq!(MacInputHandler::compose_dead_key('¨', 'o'), Some('ö'));
        assert_eq!(MacInputHandler::compose_dead_key('´', 'e'), Some('é'));
        assert_eq!(MacInputHandler::compose_dead_key('~', 'n'), Some('ñ'));
        assert_eq!(MacInputHandler::compose_dead_key('`', ' '), Some('`'));
    }
}
