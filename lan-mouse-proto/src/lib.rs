use input_event::{Event as InputEvent, KeyboardEvent, PointerEvent};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use paste::paste;
use std::{
    fmt::{Debug, Display, Formatter},
    mem::size_of,
};
use thiserror::Error;

pub mod discovery;

/// Maximum fixed-size protocol packet. A scoped input wraps the largest
/// legacy input payload with its own type byte and session serial.
pub const MAX_EVENT_SIZE: usize = 2 * size_of::<u8>() + 2 * size_of::<u32>() + 2 * size_of::<f64>();

/// Capability bit indicating support for UTF-8 clipboard text packets.
pub const CAPABILITY_CLIPBOARD_TEXT: u32 = 1 << 0;

/// Capability bit indicating support for the position-carrying
/// [`ProtoEvent::EnterAt`] and [`ProtoEvent::LeaveAt`] events.
pub const CAPABILITY_ENTER_POSITION: u32 = 1 << 1;

/// Capability bit indicating support for generation-scoped control sessions
/// via [`ProtoEvent::EnterSession`] and [`ProtoEvent::InputSession`].
pub const CAPABILITY_CONTROL_SESSION: u32 = 1 << 2;

/// Phase bit applied to a scoped session serial for Leave/Ack. Enter/Ack and
/// InputSession use the low 31-bit session id, so delayed Enter acknowledgments
/// cannot confirm a later Leave handshake.
pub const CONTROL_SESSION_CLOSE_BIT: u32 = 1 << 31;

/// Maximum UTF-8 clipboard payload accepted from a peer.
pub const MAX_CLIPBOARD_TEXT_SIZE: usize = 16 * 1024;

const CLIPBOARD_TEXT_EVENT_ID: u8 = 0x80;
const CLIPBOARD_TEXT_HEADER_SIZE: usize = size_of::<u8>() + size_of::<u32>();

/// Maximum packet size accepted by the network receive loops.
pub const MAX_WIRE_SIZE: usize = CLIPBOARD_TEXT_HEADER_SIZE + MAX_CLIPBOARD_TEXT_SIZE;

/// error type for protocol violations
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// event type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidEventId(#[from] TryFromPrimitiveError<EventType>),
    /// position type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidPosition(#[from] TryFromPrimitiveError<Position>),
    /// a packet has an invalid or inconsistent length
    #[error("invalid packet length: expected {expected} bytes, got {actual}")]
    InvalidPacketLength { expected: usize, actual: usize },
    /// a clipboard payload exceeds the protocol limit
    #[error("clipboard text is too large: {actual} bytes (maximum {maximum})")]
    ClipboardTooLarge { actual: usize, maximum: usize },
    /// clipboard text is not valid UTF-8
    #[error("clipboard text is not valid UTF-8: {0}")]
    InvalidClipboardText(#[from] std::str::Utf8Error),
    /// a scoped input packet wrapped a non-input event
    #[error("scoped input contains a non-input event")]
    InvalidSessionInput,
}

/// Position of a client
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Display for Position {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

/// main lan-mouse protocol event type
#[derive(Clone, Copy, Debug)]
pub enum ProtoEvent {
    /// notify a client that the cursor entered its region at the given position
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Enter(Position),
    /// notify a client that the cursor left its region
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Leave(u32),
    /// acknowledge of an [`ProtoEvent::Enter`] or [`ProtoEvent::Leave`] event
    Ack(u32),
    /// Input event
    Input(InputEvent),
    /// Input tied to a generation-scoped control session.
    /// Only sent to peers advertising [`CAPABILITY_CONTROL_SESSION`].
    InputSession { serial: u32, event: InputEvent },
    /// Ping event for tracking unresponsive clients.
    /// A client has to respond with [`ProtoEvent::Pong`].
    Ping,
    /// Response to [`ProtoEvent::Ping`], true if emulation is enabled / available
    Pong(bool),
    /// Build identification for the sending peer. Sent by the
    /// connect side once after the connection authenticates, and
    /// echoed back by the listen side in reply, so each end can
    /// display the peer's build hash and warn (soft) on mismatch.
    /// `commit` is the 8-byte ASCII short commit hash from
    /// `shadow_rs`'s `SHORT_COMMIT`. Old peers that don't
    /// recognize the event type silently skip it per the
    /// forward-compat handling in the receive loop.
    Hello {
        commit: [u8; 8],
        /// Optional feature bits. Legacy Hello packets decode this as zero.
        capabilities: u32,
    },
    /// like [`ProtoEvent::Enter`], but carrying where the cursor crossed the
    /// barrier: `ratio` in [0.0, 1.0] is the position along the entered edge,
    /// normalized against the sender's desktop bounding box (top/left = 0.0).
    /// Only sent to peers advertising [`CAPABILITY_ENTER_POSITION`].
    EnterAt { pos: Position, ratio: f64 },
    /// like [`ProtoEvent::Leave`], but carrying where the cursor crossed the
    /// barrier; `ratio` as in [`ProtoEvent::EnterAt`], normalized against the
    /// sender's desktop bounding box.
    /// Only sent to peers advertising [`CAPABILITY_ENTER_POSITION`].
    LeaveAt { serial: u32, ratio: f64 },
    /// Begin a generation-scoped control session at the given position.
    /// Only sent to peers advertising [`CAPABILITY_CONTROL_SESSION`].
    EnterSession {
        pos: Position,
        serial: u32,
        ratio: f64,
    },
}

impl Display for ProtoEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoEvent::Enter(s) => write!(f, "Enter({s})"),
            ProtoEvent::Leave(s) => write!(f, "Leave({s})"),
            ProtoEvent::Ack(s) => write!(f, "Ack({s})"),
            ProtoEvent::Input(e) => write!(f, "{e}"),
            ProtoEvent::InputSession { serial, event } => {
                write!(f, "InputSession({serial}, {event})")
            }
            ProtoEvent::Ping => write!(f, "ping"),
            ProtoEvent::Pong(alive) => {
                write!(
                    f,
                    "pong: {}",
                    if *alive { "alive" } else { "not available" }
                )
            }
            ProtoEvent::Hello { commit, .. } => {
                let s = std::str::from_utf8(commit).unwrap_or("????????");
                write!(f, "Hello({s})")
            }
            ProtoEvent::EnterAt { pos, ratio } => write!(f, "EnterAt({pos}, {ratio})"),
            ProtoEvent::LeaveAt { serial, ratio } => write!(f, "LeaveAt({serial}, {ratio})"),
            ProtoEvent::EnterSession { pos, serial, ratio } => {
                write!(f, "EnterSession({pos}, {serial}, {ratio})")
            }
        }
    }
}

#[derive(TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum EventType {
    PointerMotion,
    PointerButton,
    PointerAxis,
    PointerAxisValue120,
    KeyboardKey,
    KeyboardModifiers,
    Ping,
    Pong,
    Enter,
    Leave,
    Ack,
    Hello,
    EnterAt,
    LeaveAt,
    EnterSession,
    InputSession,
}

fn input_event_type(event: &InputEvent) -> EventType {
    match event {
        InputEvent::Pointer(pointer) => match pointer {
            PointerEvent::Motion { .. } => EventType::PointerMotion,
            PointerEvent::Button { .. } => EventType::PointerButton,
            PointerEvent::Axis { .. } => EventType::PointerAxis,
            PointerEvent::AxisDiscrete120 { .. } => EventType::PointerAxisValue120,
        },
        InputEvent::Keyboard(keyboard) => match keyboard {
            KeyboardEvent::Key { .. } => EventType::KeyboardKey,
            KeyboardEvent::Modifiers { .. } => EventType::KeyboardModifiers,
        },
    }
}

fn decode_input_event(event_type: EventType, buf: &mut &[u8]) -> Result<InputEvent, ProtocolError> {
    match event_type {
        EventType::PointerMotion => Ok(InputEvent::Pointer(PointerEvent::Motion {
            time: decode_u32(buf)?,
            dx: decode_f64(buf)?,
            dy: decode_f64(buf)?,
        })),
        EventType::PointerButton => Ok(InputEvent::Pointer(PointerEvent::Button {
            time: decode_u32(buf)?,
            button: decode_u32(buf)?,
            state: decode_u32(buf)?,
        })),
        EventType::PointerAxis => Ok(InputEvent::Pointer(PointerEvent::Axis {
            time: decode_u32(buf)?,
            axis: decode_u8(buf)?,
            value: decode_f64(buf)?,
        })),
        EventType::PointerAxisValue120 => Ok(InputEvent::Pointer(PointerEvent::AxisDiscrete120 {
            axis: decode_u8(buf)?,
            value: decode_i32(buf)?,
        })),
        EventType::KeyboardKey => Ok(InputEvent::Keyboard(KeyboardEvent::Key {
            time: decode_u32(buf)?,
            key: decode_u32(buf)?,
            state: decode_u8(buf)?,
        })),
        EventType::KeyboardModifiers => Ok(InputEvent::Keyboard(KeyboardEvent::Modifiers {
            depressed: decode_u32(buf)?,
            latched: decode_u32(buf)?,
            locked: decode_u32(buf)?,
            group: decode_u32(buf)?,
        })),
        _ => Err(ProtocolError::InvalidSessionInput),
    }
}

fn encode_input_payload(event: InputEvent, buf: &mut &mut [u8], len: &mut usize) {
    match event {
        InputEvent::Pointer(pointer) => match pointer {
            PointerEvent::Motion { time, dx, dy } => {
                encode_u32(buf, len, time);
                encode_f64(buf, len, dx);
                encode_f64(buf, len, dy);
            }
            PointerEvent::Button {
                time,
                button,
                state,
            } => {
                encode_u32(buf, len, time);
                encode_u32(buf, len, button);
                encode_u32(buf, len, state);
            }
            PointerEvent::Axis { time, axis, value } => {
                encode_u32(buf, len, time);
                encode_u8(buf, len, axis);
                encode_f64(buf, len, value);
            }
            PointerEvent::AxisDiscrete120 { axis, value } => {
                encode_u8(buf, len, axis);
                encode_i32(buf, len, value);
            }
        },
        InputEvent::Keyboard(keyboard) => match keyboard {
            KeyboardEvent::Key { time, key, state } => {
                encode_u32(buf, len, time);
                encode_u32(buf, len, key);
                encode_u8(buf, len, state);
            }
            KeyboardEvent::Modifiers {
                depressed,
                latched,
                locked,
                group,
            } => {
                encode_u32(buf, len, depressed);
                encode_u32(buf, len, latched);
                encode_u32(buf, len, locked);
                encode_u32(buf, len, group);
            }
        },
    }
}

impl ProtoEvent {
    fn event_type(&self) -> EventType {
        match self {
            ProtoEvent::Input(event) => input_event_type(event),
            ProtoEvent::InputSession { .. } => EventType::InputSession,
            ProtoEvent::Ping => EventType::Ping,
            ProtoEvent::Pong(_) => EventType::Pong,
            ProtoEvent::Enter(_) => EventType::Enter,
            ProtoEvent::Leave(_) => EventType::Leave,
            ProtoEvent::Ack(_) => EventType::Ack,
            ProtoEvent::Hello { .. } => EventType::Hello,
            ProtoEvent::EnterAt { .. } => EventType::EnterAt,
            ProtoEvent::LeaveAt { .. } => EventType::LeaveAt,
            ProtoEvent::EnterSession { .. } => EventType::EnterSession,
        }
    }
}

impl TryFrom<[u8; MAX_EVENT_SIZE]> for ProtoEvent {
    type Error = ProtocolError;

    fn try_from(buf: [u8; MAX_EVENT_SIZE]) -> Result<Self, Self::Error> {
        let mut buf = &buf[..];
        let event_type = decode_u8(&mut buf)?;
        match EventType::try_from(event_type)? {
            event_type @ (EventType::PointerMotion
            | EventType::PointerButton
            | EventType::PointerAxis
            | EventType::PointerAxisValue120
            | EventType::KeyboardKey
            | EventType::KeyboardModifiers) => {
                Ok(Self::Input(decode_input_event(event_type, &mut buf)?))
            }
            EventType::Ping => Ok(Self::Ping),
            EventType::Pong => Ok(Self::Pong(decode_u8(&mut buf)? != 0)),
            EventType::Enter => Ok(Self::Enter(decode_u8(&mut buf)?.try_into()?)),
            EventType::Leave => Ok(Self::Leave(decode_u32(&mut buf)?)),
            EventType::Ack => Ok(Self::Ack(decode_u32(&mut buf)?)),
            EventType::Hello => {
                let mut commit = [0u8; 8];
                for b in commit.iter_mut() {
                    *b = decode_u8(&mut buf)?;
                }
                let capabilities = decode_u32(&mut buf)?;
                Ok(Self::Hello {
                    commit,
                    capabilities,
                })
            }
            EventType::EnterAt => Ok(Self::EnterAt {
                pos: decode_u8(&mut buf)?.try_into()?,
                ratio: decode_f64(&mut buf)?,
            }),
            EventType::LeaveAt => Ok(Self::LeaveAt {
                serial: decode_u32(&mut buf)?,
                ratio: decode_f64(&mut buf)?,
            }),
            EventType::EnterSession => Ok(Self::EnterSession {
                pos: decode_u8(&mut buf)?.try_into()?,
                serial: decode_u32(&mut buf)?,
                ratio: decode_f64(&mut buf)?,
            }),
            EventType::InputSession => {
                let serial = decode_u32(&mut buf)?;
                let input_type = EventType::try_from(decode_u8(&mut buf)?)?;
                let event = decode_input_event(input_type, &mut buf)?;
                Ok(Self::InputSession { serial, event })
            }
        }
    }
}

impl From<ProtoEvent> for ([u8; MAX_EVENT_SIZE], usize) {
    fn from(event: ProtoEvent) -> Self {
        let mut buf = [0u8; MAX_EVENT_SIZE];
        let mut len = 0usize;
        {
            let mut buf = &mut buf[..];
            let buf = &mut buf;
            let len = &mut len;
            encode_u8(buf, len, event.event_type() as u8);
            match event {
                ProtoEvent::Input(event) => encode_input_payload(event, buf, len),
                ProtoEvent::InputSession { serial, event } => {
                    encode_u32(buf, len, serial);
                    encode_u8(buf, len, input_event_type(&event) as u8);
                    encode_input_payload(event, buf, len);
                }
                ProtoEvent::Ping => {}
                ProtoEvent::Pong(alive) => encode_u8(buf, len, alive as u8),
                ProtoEvent::Enter(pos) => encode_u8(buf, len, pos as u8),
                ProtoEvent::Leave(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Ack(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Hello {
                    commit,
                    capabilities,
                } => {
                    for b in commit.iter() {
                        encode_u8(buf, len, *b);
                    }
                    encode_u32(buf, len, capabilities);
                }
                ProtoEvent::EnterAt { pos, ratio } => {
                    encode_u8(buf, len, pos as u8);
                    encode_f64(buf, len, ratio);
                }
                ProtoEvent::LeaveAt { serial, ratio } => {
                    encode_u32(buf, len, serial);
                    encode_f64(buf, len, ratio);
                }
                ProtoEvent::EnterSession { pos, serial, ratio } => {
                    encode_u8(buf, len, pos as u8);
                    encode_u32(buf, len, serial);
                    encode_f64(buf, len, ratio);
                }
            }
        }
        (buf, len)
    }
}

/// A decoded network packet. Input events remain fixed-size while clipboard
/// text uses a separately negotiated variable-size packet.
#[derive(Clone, Debug)]
pub enum WireEvent {
    Protocol(ProtoEvent),
    ClipboardText(String),
}

/// Encode a UTF-8 clipboard text packet.
pub fn encode_clipboard_text(text: &str) -> Result<Vec<u8>, ProtocolError> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_CLIPBOARD_TEXT_SIZE {
        return Err(ProtocolError::ClipboardTooLarge {
            actual: bytes.len(),
            maximum: MAX_CLIPBOARD_TEXT_SIZE,
        });
    }

    let mut packet = Vec::with_capacity(CLIPBOARD_TEXT_HEADER_SIZE + bytes.len());
    packet.push(CLIPBOARD_TEXT_EVENT_ID);
    packet.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    packet.extend_from_slice(bytes);
    Ok(packet)
}

/// Decode either a fixed-size input protocol event or a clipboard text packet.
pub fn decode_wire_event(data: &[u8]) -> Result<WireEvent, ProtocolError> {
    let Some(event_id) = data.first().copied() else {
        return Err(ProtocolError::InvalidPacketLength {
            expected: 1,
            actual: 0,
        });
    };

    if event_id == CLIPBOARD_TEXT_EVENT_ID {
        if data.len() < CLIPBOARD_TEXT_HEADER_SIZE {
            return Err(ProtocolError::InvalidPacketLength {
                expected: CLIPBOARD_TEXT_HEADER_SIZE,
                actual: data.len(),
            });
        }
        let payload_len = u32::from_be_bytes(data[1..5].try_into().expect("length slice")) as usize;
        if payload_len > MAX_CLIPBOARD_TEXT_SIZE {
            return Err(ProtocolError::ClipboardTooLarge {
                actual: payload_len,
                maximum: MAX_CLIPBOARD_TEXT_SIZE,
            });
        }
        let expected = CLIPBOARD_TEXT_HEADER_SIZE + payload_len;
        if data.len() != expected {
            return Err(ProtocolError::InvalidPacketLength {
                expected,
                actual: data.len(),
            });
        }
        return Ok(WireEvent::ClipboardText(
            std::str::from_utf8(&data[CLIPBOARD_TEXT_HEADER_SIZE..])?.to_owned(),
        ));
    }

    if data.len() > MAX_EVENT_SIZE {
        return Err(ProtocolError::InvalidPacketLength {
            expected: MAX_EVENT_SIZE,
            actual: data.len(),
        });
    }
    let mut event = [0u8; MAX_EVENT_SIZE];
    event[..data.len()].copy_from_slice(data);
    Ok(WireEvent::Protocol(event.try_into()?))
}

macro_rules! decode_impl {
    ($t:ty) => {
        paste! {
            fn [<decode_ $t>](data: &mut &[u8]) -> Result<$t, ProtocolError> {
                let (int_bytes, rest) = data.split_at(size_of::<$t>());
                *data = rest;
                Ok($t::from_be_bytes(int_bytes.try_into().unwrap()))
            }
        }
    };
}

decode_impl!(u8);
decode_impl!(u32);
decode_impl!(i32);
decode_impl!(f64);

macro_rules! encode_impl {
    ($t:ty) => {
        paste! {
            fn [<encode_ $t>](buf: &mut &mut [u8], amt: &mut usize, n: $t) {
                let src = n.to_be_bytes();
                let data = std::mem::take(buf);
                let (int_bytes, rest) = data.split_at_mut(size_of::<$t>());
                int_bytes.copy_from_slice(&src);
                *amt += size_of::<$t>();
                *buf = rest
            }
        }
    };
}

encode_impl!(u8);
encode_impl!(u32);
encode_impl!(i32);
encode_impl!(f64);

#[cfg(test)]
mod tests {
    use super::{
        CAPABILITY_CLIPBOARD_TEXT, CAPABILITY_CONTROL_SESSION, CAPABILITY_ENTER_POSITION,
        CONTROL_SESSION_CLOSE_BIT, EventType, MAX_CLIPBOARD_TEXT_SIZE, MAX_EVENT_SIZE,
        MAX_WIRE_SIZE, Position, ProtoEvent, ProtocolError, WireEvent, decode_wire_event,
        encode_clipboard_text,
    };
    use input_event::{Event as InputEvent, KeyboardEvent, PointerEvent};

    fn encode(event: ProtoEvent) -> Vec<u8> {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        buf[..len].to_vec()
    }

    /// Every peer on the network decodes these bytes, so the encoding is a
    /// compatibility contract: changing it silently breaks interoperability
    /// with peers running an older build. Update these expectations only
    /// together with a deliberate, negotiated protocol change.
    #[test]
    fn wire_format_is_frozen() {
        // event id, then big-endian fields in declaration order
        assert_eq!(
            encode(ProtoEvent::Input(InputEvent::Pointer(
                PointerEvent::Motion {
                    time: 1,
                    dx: 2.0,
                    dy: -3.0,
                }
            ))),
            [
                EventType::PointerMotion as u8,
                0,
                0,
                0,
                1, // time
                0x40,
                0,
                0,
                0,
                0,
                0,
                0,
                0, // dx = 2.0
                0xc0,
                0x08,
                0,
                0,
                0,
                0,
                0,
                0, // dy = -3.0
            ]
        );
        assert_eq!(
            encode(ProtoEvent::Input(InputEvent::Pointer(
                PointerEvent::Button {
                    time: 1,
                    button: 0x110,
                    state: 1,
                }
            ))),
            [
                EventType::PointerButton as u8,
                0,
                0,
                0,
                1, // time
                0,
                0,
                1,
                0x10, // button
                0,
                0,
                0,
                1, // state
            ]
        );
        assert_eq!(
            encode(ProtoEvent::Input(InputEvent::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: 1,
                    value: -120
                }
            ))),
            [
                EventType::PointerAxisValue120 as u8,
                1, // axis
                0xff,
                0xff,
                0xff,
                0x88, // value = -120
            ]
        );
        assert_eq!(
            encode(ProtoEvent::Input(InputEvent::Keyboard(
                KeyboardEvent::Key {
                    time: 1,
                    key: 30,
                    state: 1,
                }
            ))),
            [
                EventType::KeyboardKey as u8,
                0,
                0,
                0,
                1, // time
                0,
                0,
                0,
                30, // key
                1,  // state
            ]
        );
        assert_eq!(encode(ProtoEvent::Ping), [EventType::Ping as u8]);
        assert_eq!(encode(ProtoEvent::Pong(true)), [EventType::Pong as u8, 1]);
        assert_eq!(
            encode(ProtoEvent::Enter(Position::Bottom)),
            [EventType::Enter as u8, Position::Bottom as u8]
        );
        assert_eq!(
            encode(ProtoEvent::Leave(7)),
            [EventType::Leave as u8, 0, 0, 0, 7]
        );
        assert_eq!(
            encode(ProtoEvent::Ack(7)),
            [EventType::Ack as u8, 0, 0, 0, 7]
        );
        assert_eq!(
            encode(ProtoEvent::Hello {
                commit: *b"deadbeef",
                capabilities: CAPABILITY_CLIPBOARD_TEXT,
            }),
            [
                EventType::Hello as u8,
                b'd',
                b'e',
                b'a',
                b'd',
                b'b',
                b'e',
                b'e',
                b'f', // commit
                0,
                0,
                0,
                1, // capabilities
            ]
        );

        assert_eq!(
            encode(ProtoEvent::EnterAt {
                pos: Position::Left,
                ratio: 0.5,
            }),
            [
                EventType::EnterAt as u8,
                Position::Left as u8,
                0x3f,
                0xe0,
                0,
                0,
                0,
                0,
                0,
                0, // ratio = 0.5
            ]
        );
        assert_eq!(
            encode(ProtoEvent::LeaveAt {
                serial: 7,
                ratio: 0.5,
            }),
            [
                EventType::LeaveAt as u8,
                0,
                0,
                0,
                7, // serial
                0x3f,
                0xe0,
                0,
                0,
                0,
                0,
                0,
                0, // ratio = 0.5
            ]
        );
        assert_eq!(
            encode(ProtoEvent::EnterSession {
                pos: Position::Top,
                serial: 0x0102_0304,
                ratio: 0.5,
            }),
            [
                14, // EnterSession event id; append-only protocol contract
                2,  // Position::Top
                1, 2, 3, 4, // serial
                0x3f, 0xe0, 0, 0, 0, 0, 0, 0, // ratio = 0.5
            ]
        );
        assert_eq!(
            encode(ProtoEvent::InputSession {
                serial: 0x0102_0304,
                event: InputEvent::Pointer(PointerEvent::Button {
                    time: 5,
                    button: 0x110,
                    state: 1,
                }),
            }),
            [
                15, // InputSession event id
                1, 2, 3, 4, // serial
                1, // nested PointerButton event id
                0, 0, 0, 5, // time
                0, 0, 1, 0x10, // button
                0, 0, 0, 1, // state
            ]
        );

        // clipboard packets use a reserved id outside the EventType range,
        // followed by a big-endian payload length
        assert_eq!(
            encode_clipboard_text("hi").expect("encode clipboard"),
            [0x80, 0, 0, 0, 2, b'h', b'i']
        );
    }

    /// Sizing constants are part of the same contract: peers allocate receive
    /// buffers from them and reject anything larger.
    #[test]
    fn protocol_limits_are_frozen() {
        assert_eq!(MAX_EVENT_SIZE, 26);
        assert_eq!(MAX_CLIPBOARD_TEXT_SIZE, 16 * 1024);
        assert_eq!(MAX_WIRE_SIZE, 5 + 16 * 1024);
        assert_eq!(CAPABILITY_CLIPBOARD_TEXT, 1);
        assert_eq!(CAPABILITY_ENTER_POSITION, 2);
        assert_eq!(CAPABILITY_CONTROL_SESSION, 4);
        assert_eq!(CONTROL_SESSION_CLOSE_BIT, 1 << 31);
    }

    #[test]
    fn event_type_ids_are_frozen() {
        assert_eq!(
            [
                EventType::PointerMotion as u8,
                EventType::PointerButton as u8,
                EventType::PointerAxis as u8,
                EventType::PointerAxisValue120 as u8,
                EventType::KeyboardKey as u8,
                EventType::KeyboardModifiers as u8,
                EventType::Ping as u8,
                EventType::Pong as u8,
                EventType::Enter as u8,
                EventType::Leave as u8,
                EventType::Ack as u8,
                EventType::Hello as u8,
                EventType::EnterAt as u8,
                EventType::LeaveAt as u8,
                EventType::EnterSession as u8,
                EventType::InputSession as u8,
            ],
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
    }

    #[test]
    fn every_event_round_trips() {
        let events = [
            ProtoEvent::Enter(Position::Left),
            ProtoEvent::Enter(Position::Right),
            ProtoEvent::Enter(Position::Top),
            ProtoEvent::Enter(Position::Bottom),
            ProtoEvent::Leave(u32::MAX),
            ProtoEvent::Ack(0),
            ProtoEvent::Ping,
            ProtoEvent::Pong(false),
            ProtoEvent::Pong(true),
            ProtoEvent::Hello {
                commit: *b"01234567",
                capabilities: u32::MAX,
            },
            ProtoEvent::EnterAt {
                pos: Position::Right,
                ratio: 0.75,
            },
            ProtoEvent::LeaveAt {
                serial: 0,
                ratio: 0.25,
            },
            ProtoEvent::EnterSession {
                pos: Position::Top,
                serial: u32::MAX,
                ratio: 0.125,
            },
            ProtoEvent::InputSession {
                serial: 99,
                event: InputEvent::Keyboard(KeyboardEvent::Key {
                    time: 42,
                    key: 103,
                    state: 1,
                }),
            },
            ProtoEvent::Input(InputEvent::Pointer(PointerEvent::Motion {
                time: 42,
                dx: -1.5,
                dy: 0.25,
            })),
            ProtoEvent::Input(InputEvent::Pointer(PointerEvent::Button {
                time: 42,
                button: 0x111,
                state: 0,
            })),
            ProtoEvent::Input(InputEvent::Pointer(PointerEvent::Axis {
                time: 42,
                axis: 1,
                value: 12.5,
            })),
            ProtoEvent::Input(InputEvent::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 0,
                value: 120,
            })),
            ProtoEvent::Input(InputEvent::Keyboard(KeyboardEvent::Key {
                time: 42,
                key: 103,
                state: 1,
            })),
            ProtoEvent::Input(InputEvent::Keyboard(KeyboardEvent::Modifiers {
                depressed: 1,
                latched: 2,
                locked: 4,
                group: 8,
            })),
        ];

        for event in events {
            let encoded = encode(event);
            let WireEvent::Protocol(decoded) =
                decode_wire_event(&encoded).expect("decode round-tripped event")
            else {
                panic!("expected a protocol event for {event:?}");
            };
            // ProtoEvent has no PartialEq; compare the canonical encoding
            assert_eq!(encode(decoded), encoded, "round trip changed {event:?}");
        }
    }

    #[test]
    fn enter_session_display_includes_generation_and_position() {
        assert_eq!(
            ProtoEvent::EnterSession {
                pos: Position::Bottom,
                serial: 42,
                ratio: 0.25,
            }
            .to_string(),
            "EnterSession(bottom, 42, 0.25)"
        );
    }

    #[test]
    fn scoped_input_rejects_a_non_input_nested_event() {
        let packet = [
            EventType::InputSession as u8,
            0,
            0,
            0,
            7,
            EventType::Ping as u8,
        ];
        assert!(matches!(
            decode_wire_event(&packet),
            Err(ProtocolError::InvalidSessionInput)
        ));
    }

    #[test]
    fn oversized_clipboard_packet_is_rejected_without_allocating() {
        // a peer claiming a huge payload must be rejected on the header alone
        let mut packet = vec![0x80];
        packet.extend_from_slice(&(u32::MAX).to_be_bytes());

        assert!(matches!(
            decode_wire_event(&packet),
            Err(ProtocolError::ClipboardTooLarge { .. })
        ));
    }

    #[test]
    fn truncated_clipboard_packet_is_rejected() {
        let mut packet = encode_clipboard_text("hello").expect("encode clipboard");
        packet.pop();

        assert!(matches!(
            decode_wire_event(&packet),
            Err(ProtocolError::InvalidPacketLength { .. })
        ));
    }

    #[test]
    fn non_utf8_clipboard_payload_is_rejected() {
        let packet = [0x80, 0, 0, 0, 1, 0xff];

        assert!(matches!(
            decode_wire_event(&packet),
            Err(ProtocolError::InvalidClipboardText(_))
        ));
    }

    #[test]
    fn empty_packet_is_rejected() {
        assert!(matches!(
            decode_wire_event(&[]),
            Err(ProtocolError::InvalidPacketLength { .. })
        ));
    }

    #[test]
    fn unknown_event_id_is_rejected() {
        // forward compatibility: callers skip the datagram rather than
        // dropping the connection, so decoding must fail cleanly
        assert!(matches!(
            decode_wire_event(&[0x7f]),
            Err(ProtocolError::InvalidEventId(_))
        ));
    }

    #[test]
    fn hello_capabilities_round_trip() {
        let event = ProtoEvent::Hello {
            commit: *b"deadbeef",
            capabilities: CAPABILITY_CLIPBOARD_TEXT,
        };
        let (packet, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();

        let WireEvent::Protocol(ProtoEvent::Hello {
            commit,
            capabilities,
        }) = decode_wire_event(&packet[..len]).expect("decode hello")
        else {
            panic!("expected hello event");
        };
        assert_eq!(commit, *b"deadbeef");
        assert_eq!(capabilities, CAPABILITY_CLIPBOARD_TEXT);
    }

    #[test]
    fn legacy_hello_defaults_capabilities_to_zero() {
        let mut packet = [0u8; MAX_EVENT_SIZE];
        packet[0] = super::EventType::Hello as u8;
        packet[1..9].copy_from_slice(b"cafebabe");

        let ProtoEvent::Hello { capabilities, .. } =
            ProtoEvent::try_from(packet).expect("decode legacy hello")
        else {
            panic!("expected hello event");
        };
        assert_eq!(capabilities, 0);
    }

    #[test]
    fn clipboard_text_round_trips_as_utf8() {
        let packet = encode_clipboard_text("CrossDesk clipboard: hello").expect("encode clipboard");

        let WireEvent::ClipboardText(text) = decode_wire_event(&packet).expect("decode clipboard")
        else {
            panic!("expected clipboard text");
        };
        assert_eq!(text, "CrossDesk clipboard: hello");
    }

    #[test]
    fn clipboard_text_limit_is_enforced() {
        let text = "x".repeat(MAX_CLIPBOARD_TEXT_SIZE + 1);

        assert!(matches!(
            encode_clipboard_text(&text),
            Err(ProtocolError::ClipboardTooLarge { .. })
        ));
    }
}
