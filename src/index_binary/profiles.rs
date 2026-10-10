//! Exact fixed-width document lengths and prime filters.
use super::codec::{Reader, Writer};
const PROFILES_KIND: u16 = 8;
const RECORD_BYTES: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentProfile {
    pub title_len: u64,
    pub body_len: u64,
    pub term_mask: u64,
    pub tag_signature: u128,
}

pub fn encode(profiles: &[DocumentProfile]) -> Result<Vec<u8>, String> {
    let count = u32::try_from(profiles.len()).map_err(|_| "Document profile count exceeds u32")?;
    let mut writer = Writer::new(PROFILES_KIND, count, RECORD_BYTES);
    for profile in profiles {
        writer.u64(profile.title_len);
        writer.u64(profile.body_len);
        writer.u64(profile.term_mask);
        writer.u64(profile.tag_signature as u64);
        writer.u64((profile.tag_signature >> 64) as u64);
    }
    writer.finish()
}

pub fn decode(bytes: &[u8]) -> Result<Vec<DocumentProfile>, String> {
    let mut reader = Reader::new(bytes, PROFILES_KIND)?;
    if reader.record_bytes != RECORD_BYTES {
        return Err("Invalid document profile width".into());
    }
    let mut profiles = Vec::new();
    profiles
        .try_reserve_exact(reader.count as usize)
        .map_err(|_| "Cannot allocate document profiles")?;
    for _ in 0..reader.count {
        let title_len = reader.u64()?;
        let body_len = reader.u64()?;
        let term_mask = reader.u64()?;
        let low = reader.u64()?;
        let high = reader.u64()?;
        profiles.push(DocumentProfile {
            title_len,
            body_len,
            term_mask,
            tag_signature: u128::from(low) | (u128::from(high) << 64),
        });
    }
    reader.finish()?;
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_width_values_round_trip() {
        let profiles = vec![
            DocumentProfile {
                title_len: 0,
                body_len: u64::MAX,
                term_mask: u64::MAX,
                tag_signature: u128::MAX,
            },
            DocumentProfile {
                title_len: u64::MAX,
                body_len: 0,
                term_mask: 0,
                tag_signature: 0,
            },
        ];
        assert_eq!(decode(&encode(&profiles).unwrap()).unwrap(), profiles);
        assert!(decode(&encode(&[]).unwrap()).unwrap().is_empty());
    }
    #[test]
    fn incorrect_width_and_truncation_fail_closed() {
        let bytes = encode(&[DocumentProfile {
            title_len: 1,
            body_len: 2,
            term_mask: 3,
            tag_signature: 5,
        }])
        .unwrap();
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_err());
        }
        let mut bad = bytes.clone();
        bad[32] ^= 1;
        assert!(decode(&bad).is_err());
    }
}
