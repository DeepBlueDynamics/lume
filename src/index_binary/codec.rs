//! Portable v4 segment primitives. No native-layout casts or unchecked offsets.
//! Header (40 bytes): magic[8], kind:u16, version:u16, LE marker:u32,
//! total_bytes:u64, count:u64, fixed_record_bytes:u32, reserved:u32.
//! A zero record width denotes a variable-length payload.
const MAGIC: &[u8; 8] = b"LUMEIDX4";
const HEADER_BYTES: usize = 40;
const ENDIAN_MARKER: u32 = 0x0102_0304;

pub struct Writer {
    bytes: Vec<u8>,
}
impl Writer {
    pub fn new(kind: u16, count: u32, record_bytes: u32) -> Self {
        let mut writer = Self { bytes: Vec::new() };
        writer.bytes.extend_from_slice(MAGIC);
        writer.u16(kind);
        writer.u16(1);
        writer.u32(ENDIAN_MARKER);
        writer.u64(0); // Filled at finish.
        writer.u64(u64::from(count));
        writer.u32(record_bytes);
        writer.u32(0);
        writer
    }
    pub fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }
    pub fn u16(&mut self, value: u16) {
        self.raw(&value.to_le_bytes());
    }
    pub fn u32(&mut self, value: u32) {
        self.raw(&value.to_le_bytes());
    }
    pub fn u64(&mut self, value: u64) {
        self.raw(&value.to_le_bytes());
    }
    pub fn f64(&mut self, value: f64) {
        self.u64(value.to_bits());
    }
    pub fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
    pub fn varint(&mut self, mut value: u64) {
        while value >= 128 {
            self.u8((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        self.u8(value as u8);
    }
    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        let length = u64::try_from(self.bytes.len()).map_err(|_| "Segment exceeds u64")?;
        self.bytes[16..24].copy_from_slice(&length.to_le_bytes());
        // Validate the same framing rules used by readers before publication.
        let kind = u16::from_le_bytes(self.bytes[8..10].try_into().unwrap());
        Reader::new(&self.bytes, kind)?;
        Ok(self.bytes)
    }
}

pub struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    pub count: u32,
    pub record_bytes: u32,
}
impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], expected_kind: u16) -> Result<Self, String> {
        if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
            return Err("Invalid or truncated v4 segment header".into());
        }
        let mut reader = Self {
            bytes,
            position: 8,
            count: 0,
            record_bytes: 0,
        };
        if reader.u16()? != expected_kind || reader.u16()? != 1 {
            return Err("Unsupported v4 segment kind or version".into());
        }
        if reader.u32()? != ENDIAN_MARKER {
            return Err("Unsupported v4 segment byte order".into());
        }
        let total_bytes = reader.u64()?;
        if total_bytes != u64::try_from(bytes.len()).map_err(|_| "Segment exceeds u64")? {
            return Err("V4 segment length mismatch".into());
        }
        reader.count = u32::try_from(reader.u64()?).map_err(|_| "V4 count exceeds u32")?;
        reader.record_bytes = reader.u32()?;
        if reader.u32()? != 0 {
            return Err("Nonzero reserved v4 header bits".into());
        }
        let payload_bytes = bytes.len() - HEADER_BYTES;
        if reader.record_bytes != 0 {
            let expected = u64::from(reader.count) * u64::from(reader.record_bytes);
            if expected != payload_bytes as u64 {
                return Err("V4 fixed-record length mismatch".into());
            }
        } else if u64::from(reader.count) > payload_bytes as u64 {
            // Every variable record must consume at least one byte. This bounds
            // downstream allocation from a corrupt count before parsing rows.
            return Err("V4 count exceeds available payload".into());
        }
        Ok(reader)
    }
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
    pub fn raw(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .ok_or("V4 offset overflow")?;
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or("Truncated v4 payload")?;
        self.position = end;
        Ok(result)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.raw(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.raw(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.raw(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.raw(8)?.try_into().unwrap()))
    }
    pub fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.u64()?))
    }
    pub fn varint(&mut self) -> Result<u64, String> {
        let mut value = 0;
        for byte_index in 0..10 {
            let byte = self.u8()?;
            if byte_index == 9 && byte > 1 {
                return Err("V4 varint overflow".into());
            }
            value |= u64::from(byte & 0x7f) << (byte_index * 7);
            if byte & 0x80 == 0 {
                if byte_index != 0 && byte == 0 {
                    return Err("Noncanonical v4 varint".into());
                }
                return Ok(value);
            }
        }
        Err("V4 varint overflow".into())
    }
    pub fn text(&mut self, length: usize) -> Result<&'a str, String> {
        std::str::from_utf8(self.raw(length)?).map_err(|_| "Invalid v4 UTF-8".into())
    }
    pub fn finish(self) -> Result<(), String> {
        if self.remaining() != 0 {
            return Err("Trailing v4 segment bytes".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_and_varint_bits_round_trip_on_unaligned_payloads() {
        let mut writer = Writer::new(3, 1, 0);
        writer.u8(7);
        writer.u16(0x1234);
        writer.u32(0x1234_5678);
        for value in [0, 127, 128, 65536, u64::MAX] {
            writer.varint(value);
        }
        for value in [-0.0_f64, 0.0, 1.25, f64::MAX] {
            writer.f64(value);
        }
        writer.raw("café".as_bytes());
        let bytes = writer.finish().unwrap();
        let mut reader = Reader::new(&bytes, 3).unwrap();
        assert_eq!(reader.count, 1);
        assert_eq!(reader.u8().unwrap(), 7);
        assert_eq!(reader.u16().unwrap(), 0x1234);
        assert_eq!(reader.u32().unwrap(), 0x1234_5678);
        for value in [0, 127, 128, 65536, u64::MAX] {
            assert_eq!(reader.varint().unwrap(), value);
        }
        for value in [-0.0_f64, 0.0, 1.25, f64::MAX] {
            assert_eq!(reader.f64().unwrap().to_bits(), value.to_bits());
        }
        assert_eq!(reader.text(5).unwrap(), "café");
        reader.finish().unwrap();
    }

    #[test]
    fn malformed_framing_offsets_and_counts_fail_closed() {
        let mut writer = Writer::new(1, 1, 4);
        writer.u32(42);
        let bytes = writer.finish().unwrap();
        for length in 0..bytes.len() {
            assert!(Reader::new(&bytes[..length], 1).is_err());
        }
        assert!(Reader::new(&bytes, 2).is_err());
        for offset in [0, 10, 12, 16, 24, 32, 36] {
            let mut invalid = bytes.clone();
            invalid[offset] ^= 1;
            assert!(Reader::new(&invalid, 1).is_err(), "{offset}");
        }
        let mut reader = Reader::new(&bytes, 1).unwrap();
        assert!(reader.raw(usize::MAX).is_err());
        assert_eq!(reader.u32().unwrap(), 42);
        assert!(reader.u8().is_err());
        let reader = Reader::new(&bytes, 1).unwrap();
        assert!(reader.finish().is_err());
    }

    #[test]
    fn malformed_varints_and_utf8_do_not_panic() {
        for bad in [&[0x80, 0][..], &[0xff; 10][..], &[0x80][..]] {
            let mut writer = Writer::new(1, 1, 0);
            writer.raw(bad);
            let bytes = writer.finish().unwrap();
            assert!(Reader::new(&bytes, 1).unwrap().varint().is_err());
        }
        let mut writer = Writer::new(1, 1, 0);
        writer.u8(0xff);
        let bytes = writer.finish().unwrap();
        assert!(Reader::new(&bytes, 1).unwrap().text(1).is_err());
        assert!(Writer::new(1, 1, 0).finish().is_err());
    }
}
