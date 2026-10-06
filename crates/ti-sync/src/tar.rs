//! Zero-dependency POSIX ustar tar archive reader and writer.
//!
//! Complies with standard 512-byte block ustar format, suitable for
//! packaging sealed shard files and metadata into `{vessel}/{shard}/{version}.tar`.

use std::io::{Cursor, Read, Write};
use ti_contracts::{Error, Result};

const BLOCK_SIZE: usize = 512;
const USTAR_MAGIC: &[u8; 6] = b"ustar\0";
const USTAR_VERSION: &[u8; 2] = b"00";

/// Create an uncompressed tar archive containing the provided `(name, data)` entries.
pub fn create_tar(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for (name, data) in entries {
        if name.len() > 100 {
            return Err(Error::InvalidInput(format!(
                "tar entry name too long (max 100 chars): {name}"
            )));
        }
        let mut header = [0u8; BLOCK_SIZE];

        // 0..100: name
        header[..name.len()].copy_from_slice(name.as_bytes());

        // 100..108: mode "0000644\0"
        header[100..108].copy_from_slice(b"0000644\0");

        // 108..116: uid "0000000\0"
        header[108..116].copy_from_slice(b"0000000\0");

        // 116..124: gid "0000000\0"
        header[116..124].copy_from_slice(b"0000000\0");

        // 124..136: size (11 octal digits + null byte)
        let size_str = format!("{:011o}\0", data.len());
        header[124..136].copy_from_slice(size_str.as_bytes());

        // 136..148: mtime "00000000000\0"
        header[136..148].copy_from_slice(b"00000000000\0");

        // 156: typeflag '0' (regular file)
        header[156] = b'0';

        // 257..263: magic "ustar\0"
        header[257..263].copy_from_slice(USTAR_MAGIC);

        // 263..265: version "00"
        header[263..265].copy_from_slice(USTAR_VERSION);

        // Checksum calculation: sum of all bytes in header treating checksum field (148..156) as spaces
        let mut sum: u32 = 8 * (b' ' as u32);
        for (i, byte) in header.iter().enumerate() {
            if !(148..156).contains(&i) {
                sum += *byte as u32;
            }
        }
        // 148..156: checksum "000000\0 " (6 octal digits, null, space)
        let chk_str = format!("{:06o}\0 ", sum);
        header[148..156].copy_from_slice(chk_str.as_bytes());

        // Write header
        out.write_all(&header)?;

        // Write data
        out.write_all(data)?;

        // Pad to block boundary
        let remainder = data.len() % BLOCK_SIZE;
        if remainder != 0 {
            let pad = BLOCK_SIZE - remainder;
            out.write_all(&vec![0u8; pad])?;
        }
    }

    // End-of-archive marker: two 512-byte zero blocks (1024 zero bytes)
    out.write_all(&[0u8; BLOCK_SIZE * 2])?;
    Ok(out)
}

/// Extract all `(name, content)` pairs from an uncompressed tar archive.
pub fn parse_tar(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut cursor = Cursor::new(bytes);
    let mut entries = Vec::new();
    let mut header = [0u8; BLOCK_SIZE];

    loop {
        let n = cursor.read(&mut header)?;
        if n == 0 {
            break;
        }
        if n < BLOCK_SIZE {
            return Err(Error::Corrupt("truncated tar header".into()));
        }

        // Two consecutive zero blocks indicate end of archive
        if header.iter().all(|&b| b == 0) {
            break;
        }

        // Validate magic
        if &header[257..263] != USTAR_MAGIC {
            return Err(Error::Corrupt("invalid tar ustar magic".into()));
        }

        // Extract name
        let name_end = header[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&header[..name_end])
            .map_err(|e| Error::Corrupt(format!("invalid tar file name: {e}")))?
            .to_string();

        // Extract size
        let size_str = std::str::from_utf8(&header[124..136])
            .map_err(|e| Error::Corrupt(format!("invalid tar size field: {e}")))?;
        let size_trimmed = size_str.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let size = usize::from_str_radix(size_trimmed, 8)
            .map_err(|e| Error::Corrupt(format!("failed to parse octal size in tar: {e}")))?;

        // Validate checksum
        let mut expected_sum: u32 = 8 * (b' ' as u32);
        for (i, byte) in header.iter().enumerate() {
            if !(148..156).contains(&i) {
                expected_sum += *byte as u32;
            }
        }
        let chk_str = std::str::from_utf8(&header[148..156])
            .map_err(|e| Error::Corrupt(format!("invalid tar checksum field: {e}")))?;
        let chk_trimmed = chk_str.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        let actual_sum = u32::from_str_radix(chk_trimmed, 8)
            .map_err(|e| Error::Corrupt(format!("failed to parse octal checksum: {e}")))?;
        if expected_sum != actual_sum {
            return Err(Error::Corrupt(format!(
                "tar checksum mismatch for {name}: expected {expected_sum}, got {actual_sum}"
            )));
        }

        // Read file contents
        let mut data = vec![0u8; size];
        cursor.read_exact(&mut data)?;

        // Skip padding
        let remainder = size % BLOCK_SIZE;
        if remainder != 0 {
            let pad = BLOCK_SIZE - remainder;
            let mut pad_buf = vec![0u8; pad];
            cursor.read_exact(&mut pad_buf)?;
        }

        entries.push((name, data));
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tar_roundtrip() {
        let file1 = b"Hello, World!";
        let file2 = vec![42u8; 1024];
        let file3 = b"";

        let entries = vec![
            ("hello.txt", &file1[..]),
            ("sub/data.bin", &file2[..]),
            ("empty.dat", &file3[..]),
        ];

        let tar_bytes = create_tar(&entries).unwrap();
        let extracted = parse_tar(&tar_bytes).unwrap();

        assert_eq!(extracted.len(), 3);
        assert_eq!(extracted[0].0, "hello.txt");
        assert_eq!(extracted[0].1, file1);
        assert_eq!(extracted[1].0, "sub/data.bin");
        assert_eq!(extracted[1].1, file2);
        assert_eq!(extracted[2].0, "empty.dat");
        assert_eq!(extracted[2].1, file3);
    }
}
