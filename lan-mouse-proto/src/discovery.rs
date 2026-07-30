//! LAN discovery: how a frontend finds devices running a CrossDesk service.
//!
//! The service answers UDP probes on the fixed [`DISCOVERY_PORT`] with an
//! [`Announcement`]. The port is deliberately independent of the configurable
//! service port: a frontend can always probe the same port, and the
//! announcement itself reports where the actual service listens.
//!
//! Wire format, one datagram each way:
//! - probe: exactly [`DISCOVERY_PROBE`]
//! - announcement: `CROSSDESK_ANNOUNCE_V1\n<port>\n<hostname>`

use std::fmt::{self, Display};

/// UDP port the discovery responder listens on.
///
/// Fixed, so frontends do not need to know the configured service port. The
/// Windows frontend IPC uses *TCP* 4243 on loopback; UDP and TCP port spaces
/// are separate, so there is no collision.
pub const DISCOVERY_PORT: u16 = 4243;

/// Payload of a discovery probe datagram. Anything else is ignored.
pub const DISCOVERY_PROBE: &[u8] = b"CROSSDESK_DISCOVER_V1";

const ANNOUNCE_MAGIC: &[u8] = b"CROSSDESK_ANNOUNCE_V1\n";

/// Longest hostname put on the wire (RFC 1035).
const MAX_HOSTNAME_LEN: usize = 253;

/// The most bytes an encoded announcement can take up.
pub const MAX_ANNOUNCEMENT_SIZE: usize = ANNOUNCE_MAGIC.len() + 5 + 1 + MAX_HOSTNAME_LEN;

/// What a device reveals about itself in response to a probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Announcement {
    /// port the actual service listens on
    pub port: u16,
    /// OS hostname, may be empty when it could not be determined
    pub hostname: String,
}

/// An announcement that cannot or should not go on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidAnnouncement;

impl Display for InvalidAnnouncement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid discovery announcement")
    }
}

impl std::error::Error for InvalidAnnouncement {}

impl Announcement {
    /// An announcement is only valid with a real port and a hostname that
    /// fits the wire format (no separators, no control characters).
    pub fn new(port: u16, hostname: impl Into<String>) -> Result<Self, InvalidAnnouncement> {
        let hostname = hostname.into();
        if port == 0
            || hostname.len() > MAX_HOSTNAME_LEN
            || hostname.bytes().any(|byte| byte < b' ' || byte == 0x7f)
        {
            return Err(InvalidAnnouncement);
        }
        Ok(Self { port, hostname })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(MAX_ANNOUNCEMENT_SIZE);
        buf.extend_from_slice(ANNOUNCE_MAGIC);
        buf.extend_from_slice(self.port.to_string().as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(self.hostname.as_bytes());
        buf
    }

    /// `None` for anything that is not a well-formed announcement - unknown
    /// datagrams on the discovery port must never be fatal.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let rest = buf.strip_prefix(ANNOUNCE_MAGIC)?;
        let newline = rest.iter().position(|byte| *byte == b'\n')?;
        let port = std::str::from_utf8(&rest[..newline]).ok()?.parse().ok()?;
        let hostname = std::str::from_utf8(&rest[newline + 1..]).ok()?;
        Self::new(port, hostname).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_round_trips() {
        let announcement = Announcement::new(4242, "mac-mini").expect("valid announcement");
        let decoded = Announcement::decode(&announcement.encode()).expect("decodes");
        assert_eq!(decoded, announcement);
    }

    #[test]
    fn announcement_with_empty_hostname_round_trips() {
        let announcement = Announcement::new(1, "").expect("empty hostname is valid");
        let decoded = Announcement::decode(&announcement.encode()).expect("decodes");
        assert_eq!(decoded.hostname, "");
        assert_eq!(decoded.port, 1);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Announcement::decode(b"").is_none());
        assert!(Announcement::decode(DISCOVERY_PROBE).is_none());
        assert!(Announcement::decode(b"CROSSDESK_ANNOUNCE_V2\n4242\nhost").is_none());
        assert!(Announcement::decode(b"CROSSDESK_ANNOUNCE_V1\n").is_none());
        assert!(Announcement::decode(b"CROSSDESK_ANNOUNCE_V1\n0\nhost").is_none());
        assert!(Announcement::decode(b"CROSSDESK_ANNOUNCE_V1\n65536\nhost").is_none());
        assert!(Announcement::decode(b"CROSSDESK_ANNOUNCE_V1\n4242\nho\xffst").is_none());
    }

    #[test]
    fn new_rejects_unencodable_values() {
        assert!(Announcement::new(0, "host").is_err());
        assert!(Announcement::new(4242, "ho\nst").is_err());
        assert!(Announcement::new(4242, "a".repeat(MAX_HOSTNAME_LEN + 1)).is_err());
        assert!(Announcement::new(4242, "a".repeat(MAX_HOSTNAME_LEN)).is_ok());
    }

    #[test]
    fn encoded_announcement_stays_within_bounds() {
        let announcement =
            Announcement::new(65535, "a".repeat(MAX_HOSTNAME_LEN)).expect("valid announcement");
        assert_eq!(announcement.encode().len(), MAX_ANNOUNCEMENT_SIZE);
    }
}
