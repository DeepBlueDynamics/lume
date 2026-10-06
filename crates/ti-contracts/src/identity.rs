//! Opaque entity identities. Validation never rewrites or normalizes stored bytes.
use crate::{Error, Result};

/// Canonical `<kind>.urn:<opaque nonempty suffix>`, retaining Signal K bytes.
pub fn validate_entity_urn(urn: &str) -> Result<()> {
    let Some((kind, suffix)) = urn.split_once(".urn:") else {
        return Err(Error::InvalidInput("entity must be a canonical <kind>.urn:<id>".into()));
    };
    let mut bytes = kind.bytes();
    let valid_kind = bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid_kind || suffix.is_empty() {
        return Err(Error::InvalidInput("invalid canonical entity URN".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_identity_preserves_signal_k_and_accepts_explicit_kinds() {
        for urn in ["vessels.urn:mrn:signalk:uuid:boat", "robots.urn:fleet:A", "Devices_2.urn:opaque λ", "sensor-kind.urn:1"] {
            assert!(validate_entity_urn(urn).is_ok(), "{urn}");
        }
        for urn in ["vessels.self", "urn:robot:1", "robots.urn:", ".urn:x", "2robots.urn:x", "bad.kind.urn:x", "röbots.urn:x"] {
            assert!(validate_entity_urn(urn).is_err(), "{urn}");
        }
    }
}
