//! Wire framing for the UDP address-validation exchange.
//!
//! These messages are consumed by the UDP transports before they reach the
//! client/server event loops. Keeping the original HELLO inside the response
//! lets the server remain stateless until the peer proves it can receive the
//! challenge at its claimed address.

use crate::{Envelope, EnvelopeError, ENVELOPE_HEADER_SIZE};
use bytes::{BufMut, Bytes, BytesMut};

/// Version byte + monotonic issue timestamp + full keyed BLAKE3 tag.
pub const UDP_ADDRESS_COOKIE_SIZE: usize = 1 + 8 + 32;

/// Stateless cookie sent by the server to an unvalidated UDP peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpAddressChallenge {
    /// Opaque server-generated cookie; clients must return it unchanged.
    pub cookie: [u8; UDP_ADDRESS_COOKIE_SIZE],
}

impl UdpAddressChallenge {
    pub fn to_bytes(&self) -> Bytes {
        Bytes::copy_from_slice(&self.cookie)
    }

    pub fn from_bytes(bytes: &Bytes) -> Result<Self, EnvelopeError> {
        let cookie: [u8; UDP_ADDRESS_COOKIE_SIZE] = bytes.as_ref().try_into().map_err(|_| {
            EnvelopeError::Invalid(format!(
                "UDP address challenge must be {UDP_ADDRESS_COOKIE_SIZE} bytes"
            ))
        })?;
        Ok(Self { cookie })
    }
}

/// Cookie echo carrying the exact HELLO that caused the challenge.
#[derive(Debug, Clone, PartialEq)]
pub struct UdpAddressResponse {
    /// Cookie copied verbatim from [`UdpAddressChallenge`].
    pub cookie: [u8; UDP_ADDRESS_COOKIE_SIZE],
    /// Exact HELLO envelope that triggered the challenge.
    pub hello: Envelope,
}

impl UdpAddressResponse {
    pub fn to_bytes(&self) -> Bytes {
        let hello = self.hello.to_bytes();
        let mut bytes = BytesMut::with_capacity(UDP_ADDRESS_COOKIE_SIZE + hello.len());
        bytes.put_slice(&self.cookie);
        bytes.put_slice(&hello);
        bytes.freeze()
    }

    pub fn from_bytes(bytes: &Bytes) -> Result<Self, EnvelopeError> {
        let minimum = UDP_ADDRESS_COOKIE_SIZE + ENVELOPE_HEADER_SIZE;
        if bytes.len() < minimum {
            return Err(EnvelopeError::BufferTooShort {
                need: minimum,
                have: bytes.len(),
            });
        }

        let cookie: [u8; UDP_ADDRESS_COOKIE_SIZE] = bytes[..UDP_ADDRESS_COOKIE_SIZE]
            .try_into()
            .expect("slice length checked");
        let hello_bytes = bytes.slice(UDP_ADDRESS_COOKIE_SIZE..);
        let hello = Envelope::from_bytes(hello_bytes.clone())?;
        if hello.total_size() != hello_bytes.len() {
            return Err(EnvelopeError::Invalid(
                "UDP address response contains trailing HELLO bytes".to_string(),
            ));
        }

        Ok(Self { cookie, hello })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{messages::routes, EnvelopeFlags, CURRENT_PROTOCOL_VERSION};

    fn hello() -> Envelope {
        Envelope::new_simple(
            CURRENT_PROTOCOL_VERSION,
            1,
            0,
            routes::HELLO,
            1,
            EnvelopeFlags::RELIABLE,
            Bytes::from_static(b"hello"),
        )
    }

    #[test]
    fn challenge_round_trip_and_length_check() {
        let challenge = UdpAddressChallenge {
            cookie: [7; UDP_ADDRESS_COOKIE_SIZE],
        };
        assert_eq!(
            UdpAddressChallenge::from_bytes(&challenge.to_bytes()).unwrap(),
            challenge
        );
        assert!(UdpAddressChallenge::from_bytes(&Bytes::from_static(b"short")).is_err());
    }

    #[test]
    fn response_round_trip_rejects_trailing_bytes() {
        let response = UdpAddressResponse {
            cookie: [9; UDP_ADDRESS_COOKIE_SIZE],
            hello: hello(),
        };
        assert_eq!(
            UdpAddressResponse::from_bytes(&response.to_bytes()).unwrap(),
            response
        );

        let mut malformed = BytesMut::from(response.to_bytes().as_ref());
        malformed.put_u8(0);
        assert!(UdpAddressResponse::from_bytes(&malformed.freeze()).is_err());
    }
}
