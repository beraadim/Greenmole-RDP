use ironrdp_ainput as ainput;
use ironrdp_pdu::input::fast_path::{self, SynchronizeFlags};
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
    Move { x: u16, y: u16 },
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
    VerticalScroll { value: i16 },
    Scroll { x: i32, y: i32 },
    RelMove { x: i32, y: i32 },
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

impl From<MousePdu> for MouseEvent {
    fn from(value: MousePdu) -> Self {
        // Wheel packets ignore other flags and their position fields. Use mouse_pdu
        // when forwarding a whole packet: a single event cannot also carry a button's position.
        if value.flags.contains(PointerFlags::VERTICAL_WHEEL) {
            MouseEvent::VerticalScroll {
                value: value.number_of_wheel_rotation_units,
            }
        } else if value.flags.contains(PointerFlags::HORIZONTAL_WHEEL) {
            MouseEvent::Scroll {
                x: i32::from(value.number_of_wheel_rotation_units),
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
            MouseEvent::Scroll {
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
}
