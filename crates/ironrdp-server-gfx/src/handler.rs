use ironrdp_ainput as ainput;
use ironrdp_pdu::input::fast_path;
pub use ironrdp_pdu::input::fast_path::SynchronizeFlags;
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::mouse_rel::PointerRelFlags;
use ironrdp_pdu::input::mouse_x::PointerXFlags;
use ironrdp_pdu::input::sync::SyncToggleFlags;
use ironrdp_pdu::input::{scan_code, unicode, MousePdu, MouseRelPdu, MouseXPdu};

/// Keyboard Event
///
/// Describes a keyboard event received from the client
///
#[derive(Debug)]
pub enum KeyboardEvent {
    Pressed { code: u8, extended: bool },
    Released { code: u8, extended: bool },
    UnicodePressed(u16),
    UnicodeReleased(u16),
    Synchronize(SynchronizeFlags),
}

/// Mouse Event
///
/// Describes a mouse event received from the client
///
#[derive(Debug, PartialEq, Eq)]
pub enum MouseEvent {
    Move {
        x: u16,
        y: u16,
    },
    RightPressed,
    RightReleased,
    LeftPressed,
    LeftReleased,
    MiddlePressed,
    MiddleReleased,
    Button4Pressed,
    Button4Released,
    Button5Pressed,
    Button5Released,
    VerticalScroll {
        value: i16,
    },
    Scroll {
        x: i32,
        y: i32,
    },
    /// Additional input wheel units in signed 16.16 fixed-point format.
    ScrollHighResolution {
        x: i32,
        y: i32,
    },
    RelMove {
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ClientInputContext {
    pub client_name: String,
    pub keyboard_layout: u32,
}

/// Input Event Handler for an RDP server
///
/// Whenever the RDP server will receive an input event from a client, the relevant callback from
/// this handler will be called
///
/// # Example
///
/// ```
/// use ironrdp_server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
///
/// pub struct InputHandler;
///
/// impl RdpServerInputHandler for InputHandler {
///     fn keyboard(&mut self, event: KeyboardEvent) {
///         match event {
///             KeyboardEvent::Pressed { code, .. } => println!("Pressed {}", code),
///             KeyboardEvent::Released { code, .. } => println!("Released {}", code),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
///
///     fn mouse(&mut self, event: MouseEvent) {
///         let result = match event {
///             MouseEvent::Move { x, y } => println!("Moved mouse to {} {}", x, y),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
/// }
/// ```
pub trait RdpServerInputHandler: Send {
    /// Admit an authenticated connection before input, capture or channel startup.
    /// A platform security guard can reject access without exposing the desktop.
    /// Resize reactivation must retain the original guard instead of opening one.
    fn prepare_connection(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    /// Refresh input context after initial activation and resize reactivation.
    fn update_client_context(&mut self, _context: ClientInputContext) {}
    /// Start a connection once; resize reactivation does not start a new session.
    fn on_connect(&mut self, _context: ClientInputContext, _peer_addr: Option<String>) {}
    /// End the real connection, including failed reactivation or capture setup.
    fn on_disconnect(&mut self) {}
    fn keyboard(&mut self, event: KeyboardEvent);
    fn mouse(&mut self, event: MouseEvent);

    /// Forward an absolute pointer packet without losing the position of button events.
    ///
    /// A button packet carries its own desktop coordinates, even when MOVE is absent.
    /// Deliver the position first so handlers using the last pointer position inject the
    /// button at the packet's location. Wheel coordinates are unspecified by MS-RDPBCGR
    /// and must not move the pointer, including when other flags accompany the wheel.
    fn mouse_pdu(&mut self, event: MousePdu) {
        if event
            .flags
            .intersects(PointerFlags::VERTICAL_WHEEL | PointerFlags::HORIZONTAL_WHEEL)
        {
            self.mouse(event.into());
            return;
        }

        let buttons = PointerFlags::LEFT_BUTTON
            | PointerFlags::RIGHT_BUTTON
            | PointerFlags::MIDDLE_BUTTON_OR_WHEEL;
        if !event.flags.intersects(PointerFlags::MOVE | buttons) {
            return;
        }
        self.mouse(MouseEvent::Move {
            x: event.x_position,
            y: event.y_position,
        });
        let down = event.flags.contains(PointerFlags::DOWN);
        for (flag, pressed, released) in [
            (
                PointerFlags::LEFT_BUTTON,
                MouseEvent::LeftPressed,
                MouseEvent::LeftReleased,
            ),
            (
                PointerFlags::RIGHT_BUTTON,
                MouseEvent::RightPressed,
                MouseEvent::RightReleased,
            ),
            (
                PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                MouseEvent::MiddlePressed,
                MouseEvent::MiddleReleased,
            ),
        ] {
            if event.flags.contains(flag) {
                self.mouse(if down { pressed } else { released });
            }
        }
    }

    /// Forward extended buttons at their packet coordinates (buttons 4 and 5).
    fn mouse_x_pdu(&mut self, event: MouseXPdu) {
        if !event
            .flags
            .intersects(PointerXFlags::BUTTON1 | PointerXFlags::BUTTON2)
        {
            return;
        }
        self.mouse(MouseEvent::Move {
            x: event.x_position,
            y: event.y_position,
        });
        let down = event.flags.contains(PointerXFlags::DOWN);
        for (flag, pressed, released) in [
            (
                PointerXFlags::BUTTON1,
                MouseEvent::Button4Pressed,
                MouseEvent::Button4Released,
            ),
            (
                PointerXFlags::BUTTON2,
                MouseEvent::Button5Pressed,
                MouseEvent::Button5Released,
            ),
        ] {
            if event.flags.contains(flag) {
                self.mouse(if down { pressed } else { released });
            }
        }
    }

    /// Relative packets can combine movement with several button transitions.
    fn mouse_rel_pdu(&mut self, event: MouseRelPdu) {
        let actions = PointerRelFlags::MOVE
            | PointerRelFlags::BUTTON1
            | PointerRelFlags::BUTTON2
            | PointerRelFlags::BUTTON3
            | PointerRelFlags::XBUTTON1
            | PointerRelFlags::XBUTTON2;
        if !event.flags.intersects(actions) {
            return;
        }
        if event.x_delta != 0 || event.y_delta != 0 {
            self.mouse(MouseEvent::RelMove {
                x: event.x_delta.into(),
                y: event.y_delta.into(),
            });
        }
        let down = event.flags.contains(PointerRelFlags::DOWN);
        for (flag, pressed, released) in [
            (
                PointerRelFlags::BUTTON1,
                MouseEvent::LeftPressed,
                MouseEvent::LeftReleased,
            ),
            (
                PointerRelFlags::BUTTON2,
                MouseEvent::RightPressed,
                MouseEvent::RightReleased,
            ),
            (
                PointerRelFlags::BUTTON3,
                MouseEvent::MiddlePressed,
                MouseEvent::MiddleReleased,
            ),
            (
                PointerRelFlags::XBUTTON1,
                MouseEvent::Button4Pressed,
                MouseEvent::Button4Released,
            ),
            (
                PointerRelFlags::XBUTTON2,
                MouseEvent::Button5Pressed,
                MouseEvent::Button5Released,
            ),
        ] {
            if event.flags.contains(flag) {
                self.mouse(if down { pressed } else { released });
            }
        }
    }

    /// Preserve all additional-input packet components and never interpret wheel
    /// rotation units as cursor coordinates or warp an unknown event to (0, 0).
    fn ainput_pdu(&mut self, event: ainput::MousePdu) {
        use ainput::MouseEventFlags as F;
        if event.flags.contains(F::WHEEL) {
            self.mouse(MouseEvent::ScrollHighResolution {
                x: event.x,
                y: event.y,
            });
            return;
        }
        let buttons = F::BUTTON1 | F::BUTTON2 | F::BUTTON3 | F::XBUTTON1 | F::XBUTTON2;
        if !event.flags.intersects(F::MOVE | buttons) {
            return;
        }
        if event.flags.contains(F::REL) {
            if event.x != 0 || event.y != 0 {
                self.mouse(MouseEvent::RelMove {
                    x: event.x,
                    y: event.y,
                });
            }
        } else if event.flags.intersects(F::MOVE | buttons) {
            self.mouse(MouseEvent::Move {
                x: event.x.clamp(0, i32::from(u16::MAX)) as u16,
                y: event.y.clamp(0, i32::from(u16::MAX)) as u16,
            });
        }
        let down = event.flags.contains(F::DOWN);
        for (flag, pressed, released) in [
            (
                F::BUTTON1,
                MouseEvent::LeftPressed,
                MouseEvent::LeftReleased,
            ),
            (
                F::BUTTON2,
                MouseEvent::RightPressed,
                MouseEvent::RightReleased,
            ),
            (
                F::BUTTON3,
                MouseEvent::MiddlePressed,
                MouseEvent::MiddleReleased,
            ),
            (
                F::XBUTTON1,
                MouseEvent::Button4Pressed,
                MouseEvent::Button4Released,
            ),
            (
                F::XBUTTON2,
                MouseEvent::Button5Pressed,
                MouseEvent::Button5Released,
            ),
        ] {
            if event.flags.contains(flag) {
                self.mouse(if down { pressed } else { released });
            }
        }
    }
}

impl From<(u8, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u8, fast_path::KeyboardFlags)) -> Self {
        let extended = flags.contains(fast_path::KeyboardFlags::EXTENDED);
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released {
                code: key,
                extended,
            }
        } else {
            KeyboardEvent::Pressed {
                code: key,
                extended,
            }
        }
    }
}

impl From<(u16, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, fast_path::KeyboardFlags)) -> Self {
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<(u16, scan_code::KeyboardFlags)> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from((key, flags): (u16, scan_code::KeyboardFlags)) -> Self {
        let extended = flags.contains(scan_code::KeyboardFlags::EXTENDED);

        if flags.contains(scan_code::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released {
                code: key as u8,
                extended,
            }
        } else {
            KeyboardEvent::Pressed {
                code: key as u8,
                extended,
            }
        }
    }
}

impl From<(u16, unicode::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, unicode::KeyboardFlags)) -> Self {
        if flags.contains(unicode::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<SynchronizeFlags> for KeyboardEvent {
    fn from(value: SynchronizeFlags) -> Self {
        KeyboardEvent::Synchronize(value)
    }
}

impl From<SyncToggleFlags> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from(value: SyncToggleFlags) -> Self {
        KeyboardEvent::Synchronize(SynchronizeFlags::from_bits_truncate(value.bits() as u8))
    }
}

/// ironrdp-pdu 0.7 decodes the sign bit as sign-magnitude and drops the raw
/// low-byte flags. Recover the protocol's signed nine-bit value at the adapter.
/// Wire fixtures below intentionally fail if that dependency changes behavior.
fn wheel_rotation_units(value: &MousePdu) -> i16 {
    if value.flags.contains(PointerFlags::WHEEL_NEGATIVE) {
        (-256 - i32::from(value.number_of_wheel_rotation_units)).clamp(-256, -1) as i16
    } else {
        value.number_of_wheel_rotation_units
    }
}

impl From<MousePdu> for MouseEvent {
    fn from(value: MousePdu) -> Self {
        // Wheel packets ignore other flags and their position fields. Use mouse_pdu
        // when forwarding a whole packet: a single event cannot also carry a button's position.
        if value.flags.contains(PointerFlags::VERTICAL_WHEEL) {
            MouseEvent::VerticalScroll {
                value: wheel_rotation_units(&value),
            }
        } else if value.flags.contains(PointerFlags::HORIZONTAL_WHEEL) {
            MouseEvent::Scroll {
                x: i32::from(wheel_rotation_units(&value)),
                y: 0,
            }
        } else if value.flags.contains(PointerFlags::LEFT_BUTTON) {
            if value.flags.contains(PointerFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerFlags::RIGHT_BUTTON) {
            if value.flags.contains(PointerFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(PointerFlags::MIDDLE_BUTTON_OR_WHEEL) {
            if value.flags.contains(PointerFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else {
            MouseEvent::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseXPdu> for MouseEvent {
    fn from(value: MouseXPdu) -> Self {
        if value.flags.contains(PointerXFlags::BUTTON1) {
            if value.flags.contains(PointerXFlags::DOWN) {
                MouseEvent::Button4Pressed
            } else {
                MouseEvent::Button4Released
            }
        } else if value.flags.contains(PointerXFlags::BUTTON2) {
            if value.flags.contains(PointerXFlags::DOWN) {
                MouseEvent::Button5Pressed
            } else {
                MouseEvent::Button5Released
            }
        } else {
            MouseEvent::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseRelPdu> for MouseEvent {
    fn from(value: MouseRelPdu) -> Self {
        if value.flags.contains(PointerRelFlags::BUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON3) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button4Pressed
            } else {
                MouseEvent::Button4Released
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button5Pressed
            } else {
                MouseEvent::Button5Released
            }
        } else {
            MouseEvent::RelMove {
                x: value.x_delta.into(),
                y: value.y_delta.into(),
            }
        }
    }
}

impl From<ainput::MousePdu> for MouseEvent {
    fn from(value: ainput::MousePdu) -> Self {
        use ainput::MouseEventFlags;

        if value.flags.contains(MouseEventFlags::BUTTON1) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON2) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON3) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(MouseEventFlags::WHEEL) {
            MouseEvent::ScrollHighResolution {
                x: value.x,
                y: value.y,
            }
        } else if value.flags.contains(MouseEventFlags::REL) {
            MouseEvent::RelMove {
                x: value.x,
                y: value.y,
            }
        } else if value.flags.contains(MouseEventFlags::MOVE) {
            // assume moves are 0 <= u16::MAX
            MouseEvent::Move {
                x: value.x.try_into().unwrap_or(0),
                y: value.y.try_into().unwrap_or(0),
            }
        } else {
            MouseEvent::Move { x: 0, y: 0 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        KeyboardEvent, MouseEvent, MousePdu, MouseXPdu, PointerFlags, PointerXFlags,
        RdpServerInputHandler,
    };

    #[derive(Default)]
    struct RecordingHandler {
        events: Vec<MouseEvent>,
    }

    impl RdpServerInputHandler for RecordingHandler {
        fn keyboard(&mut self, _event: KeyboardEvent) {}

        fn mouse(&mut self, event: MouseEvent) {
            self.events.push(event);
        }
    }

    fn pointer(flags: PointerFlags, x: u16, y: u16) -> MousePdu {
        MousePdu {
            flags,
            number_of_wheel_rotation_units: 0,
            x_position: x,
            y_position: y,
        }
    }

    #[test]
    fn guacamole_double_click_keeps_both_complete_clicks_at_packet_position() {
        let mut handler = RecordingHandler::default();
        // Guacamole sends two ordinary down/up pairs, without a MOVE flag or
        // a double-click marker. Native injection must infer the click count;
        // neither repeated position nor repeated button may discard a pair.
        for flags in [
            PointerFlags::LEFT_BUTTON | PointerFlags::DOWN,
            PointerFlags::LEFT_BUTTON,
            PointerFlags::LEFT_BUTTON | PointerFlags::DOWN,
            PointerFlags::LEFT_BUTTON,
        ] {
            let wire = ironrdp_core::encode_vec(&pointer(flags, 400, 250)).unwrap();
            let packet: MousePdu = ironrdp_core::decode(&wire).unwrap();
            handler.mouse_pdu(packet);
        }
        assert_eq!(
            handler.events,
            [
                MouseEvent::Move { x: 400, y: 250 },
                MouseEvent::LeftPressed,
                MouseEvent::Move { x: 400, y: 250 },
                MouseEvent::LeftReleased,
                MouseEvent::Move { x: 400, y: 250 },
                MouseEvent::LeftPressed,
                MouseEvent::Move { x: 400, y: 250 },
                MouseEvent::LeftReleased,
            ]
        );
    }

    #[test]
    fn button_packets_update_position_before_press_and_release_without_move_flag() {
        for (flag, pressed, released) in [
            (
                PointerFlags::LEFT_BUTTON,
                MouseEvent::LeftPressed,
                MouseEvent::LeftReleased,
            ),
            (
                PointerFlags::RIGHT_BUTTON,
                MouseEvent::RightPressed,
                MouseEvent::RightReleased,
            ),
            (
                PointerFlags::MIDDLE_BUTTON_OR_WHEEL,
                MouseEvent::MiddlePressed,
                MouseEvent::MiddleReleased,
            ),
        ] {
            let mut handler = RecordingHandler::default();
            handler.mouse_pdu(pointer(PointerFlags::MOVE, 10, 20));
            handler.mouse_pdu(pointer(flag | PointerFlags::DOWN, 1600, 900));
            handler.mouse_pdu(pointer(flag, 1200, 800));

            assert_eq!(
                handler.events,
                [
                    MouseEvent::Move { x: 10, y: 20 },
                    MouseEvent::Move { x: 1600, y: 900 },
                    pressed,
                    MouseEvent::Move { x: 1200, y: 800 },
                    released,
                ]
            );
        }
    }

    #[test]
    fn combined_move_and_buttons_deliver_one_position_then_every_button() {
        let mut handler = RecordingHandler::default();
        handler.mouse_pdu(pointer(
            PointerFlags::MOVE
                | PointerFlags::LEFT_BUTTON
                | PointerFlags::RIGHT_BUTTON
                | PointerFlags::DOWN,
            2560,
            1440,
        ));

        assert_eq!(
            handler.events,
            [
                MouseEvent::Move { x: 2560, y: 1440 },
                MouseEvent::LeftPressed,
                MouseEvent::RightPressed,
            ]
        );
    }

    #[test]
    fn wheel_packets_never_use_coordinates_or_other_pointer_flags() {
        for (flag, expected) in [
            (
                PointerFlags::VERTICAL_WHEEL,
                MouseEvent::VerticalScroll { value: -120 },
            ),
            (
                PointerFlags::HORIZONTAL_WHEEL,
                MouseEvent::Scroll { x: -120, y: 0 },
            ),
            (
                PointerFlags::VERTICAL_WHEEL | PointerFlags::HORIZONTAL_WHEEL,
                MouseEvent::VerticalScroll { value: -120 },
            ),
        ] {
            let mut handler = RecordingHandler::default();
            handler.mouse_pdu(MousePdu {
                flags: flag | PointerFlags::MOVE | PointerFlags::LEFT_BUTTON | PointerFlags::DOWN,
                number_of_wheel_rotation_units: -120,
                x_position: u16::MAX,
                y_position: u16::MAX,
            });

            assert_eq!(handler.events, [expected]);
        }
    }

    #[test]
    fn extended_buttons_preserve_coordinates_and_button_identity() {
        for (flag, pressed, released) in [
            (
                PointerXFlags::BUTTON1,
                MouseEvent::Button4Pressed,
                MouseEvent::Button4Released,
            ),
            (
                PointerXFlags::BUTTON2,
                MouseEvent::Button5Pressed,
                MouseEvent::Button5Released,
            ),
        ] {
            let mut handler = RecordingHandler::default();
            handler.mouse_x_pdu(MouseXPdu {
                flags: flag | PointerXFlags::DOWN,
                x_position: 1920,
                y_position: 1080,
            });
            handler.mouse_x_pdu(MouseXPdu {
                flags: flag,
                x_position: 800,
                y_position: 600,
            });

            assert_eq!(
                handler.events,
                [
                    MouseEvent::Move { x: 1920, y: 1080 },
                    pressed,
                    MouseEvent::Move { x: 800, y: 600 },
                    released,
                ]
            );
        }
    }

    #[test]
    fn combined_extended_buttons_deliver_one_position_then_every_button() {
        let mut handler = RecordingHandler::default();
        handler.mouse_x_pdu(MouseXPdu {
            flags: PointerXFlags::BUTTON1 | PointerXFlags::BUTTON2,
            x_position: 3840,
            y_position: 2160,
        });

        assert_eq!(
            handler.events,
            [
                MouseEvent::Move { x: 3840, y: 2160 },
                MouseEvent::Button4Released,
                MouseEvent::Button5Released,
            ]
        );
    }

    #[test]
    fn packets_without_move_or_button_flags_do_not_reposition_pointer() {
        let mut handler = RecordingHandler::default();
        handler.mouse_pdu(pointer(PointerFlags::empty(), 0, 0));
        handler.mouse_pdu(pointer(PointerFlags::DOWN, 0, 0));
        handler.mouse_x_pdu(MouseXPdu {
            flags: PointerXFlags::DOWN,
            x_position: 0,
            y_position: 0,
        });

        assert!(handler.events.is_empty());
    }

    #[test]
    fn wire_wheel_values_use_signed_nine_bit_rotation_for_both_axes() {
        // Guacamole's concrete +/-120 packets are 0x0278 and 0x0388.
        // Cover every legal wire value, including high-resolution +/-1.
        for delta in -256i16..=255 {
            for wheel in [PointerFlags::VERTICAL_WHEEL, PointerFlags::HORIZONTAL_WHEEL] {
                let mut bytes = [0u8; 6];
                let flags = wheel.bits() | ((delta as u16) & 0x01FF);
                bytes[..2].copy_from_slice(&flags.to_le_bytes());
                let pdu: MousePdu = ironrdp_core::decode(&bytes).unwrap();
                let mut handler = RecordingHandler::default();
                handler.mouse_pdu(pdu);
                let expected = if wheel == PointerFlags::VERTICAL_WHEEL {
                    MouseEvent::VerticalScroll { value: delta }
                } else {
                    MouseEvent::Scroll {
                        x: i32::from(delta),
                        y: 0,
                    }
                };
                assert_eq!(handler.events, [expected], "wire delta {delta}");
            }
        }
    }

    #[test]
    fn relative_packet_preserves_motion_and_all_button_transitions() {
        use super::{MouseRelPdu, PointerRelFlags as F};
        let mut handler = RecordingHandler::default();
        handler.mouse_rel_pdu(MouseRelPdu {
            flags: F::DOWN | F::BUTTON1 | F::XBUTTON1 | F::XBUTTON2,
            x_delta: -10,
            y_delta: 20,
        });
        handler.mouse_rel_pdu(MouseRelPdu {
            flags: F::BUTTON1 | F::XBUTTON1 | F::XBUTTON2,
            x_delta: 0,
            y_delta: 0,
        });
        handler.mouse_rel_pdu(MouseRelPdu {
            flags: F::empty(),
            x_delta: 100,
            y_delta: 100,
        });
        assert_eq!(
            handler.events,
            [
                MouseEvent::RelMove { x: -10, y: 20 },
                MouseEvent::LeftPressed,
                MouseEvent::Button4Pressed,
                MouseEvent::Button5Pressed,
                MouseEvent::LeftReleased,
                MouseEvent::Button4Released,
                MouseEvent::Button5Released,
            ]
        );
    }

    #[test]
    fn advanced_packets_preserve_combined_motion_buttons_and_wheel_units() {
        use super::ainput::{MouseEventFlags as F, MousePdu as Pdu};
        let mut handler = RecordingHandler::default();
        handler.ainput_pdu(Pdu {
            time: 0,
            flags: F::REL | F::DOWN | F::BUTTON1 | F::XBUTTON2,
            x: -10,
            y: 20,
        });
        handler.ainput_pdu(Pdu {
            time: 0,
            flags: F::XBUTTON2,
            x: 200,
            y: 300,
        });
        handler.ainput_pdu(Pdu {
            time: 0,
            flags: F::WHEEL | F::MOVE | F::BUTTON1,
            x: 120 << 16,
            y: -120 << 16,
        });
        handler.ainput_pdu(Pdu {
            time: 0,
            flags: F::empty(),
            x: 999,
            y: 999,
        });
        handler.ainput_pdu(Pdu {
            time: 0,
            flags: F::REL,
            x: 999,
            y: 999,
        });
        assert_eq!(
            handler.events,
            [
                MouseEvent::RelMove { x: -10, y: 20 },
                MouseEvent::LeftPressed,
                MouseEvent::Button5Pressed,
                MouseEvent::Move { x: 200, y: 300 },
                MouseEvent::Button5Released,
                MouseEvent::ScrollHighResolution {
                    x: 120 << 16,
                    y: -120 << 16
                },
            ]
        );
    }
}
