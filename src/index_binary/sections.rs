//! Fixed section descriptors plus a checked UTF-8 blob. The text is stored once.
//!
//! By default `text.bin` uses `TEXT_KIND = 5` (raw UTF-8 + entity varints).
//! When `LUME_COMPRESS=zstd` is set, `text.bin` uses `TEXT_ZSTD_KIND = 10`,
//! compressing the logical text stream in 64 KiB (`65,536`-byte) blocks with an
//! explicit offset table so snippet/single-section readers decode only their
//! own block(s) while full snapshot decode streams block-by-block with a single
//! 64 KiB buffer.
use super::codec::{Reader, Writer};
use std::borrow::Cow;

const SECTIONS_KIND: u16 = 4;
const TEXT_KIND: u16 = 5;
const TEXT_ZSTD_KIND: u16 = 10;
const RECORD_BYTES: u32 = 72;
const ABSENT: u64 = u64::MAX;
pub const BLOCK_BYTES: usize = 64 * 1024;
const ZSTD_LEVEL: i32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionText {
    pub title: String,
    pub body: String,
    pub filename: Option<String>,
    pub line_number: u64,
    pub entities: Vec<String>,
}

pub fn compress_enabled() -> Result<bool, String> {
    match std::env::var("LUME_COMPRESS") {
        Ok(value) => match value.trim() {
            "" | "0" | "false" | "none" | "off" => Ok(false),
            "zstd" => Ok(true),
            other => Err(format!(
                "Unsupported LUME_COMPRESS={other:?}; expected 'zstd' or 'none'"
            )),
        },
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(_) => Err("Invalid LUME_COMPRESS".into()),
    }
}

struct BlockEncoder {
    compressor: zstd::bulk::Compressor<'static>,
    pending: Vec<u8>,
    offsets: Vec<u64>,
    compressed: Vec<u8>,
    uncompressed_bytes: u64,
}

impl BlockEncoder {
    fn new() -> Result<Self, String> {
        let compressor = zstd::bulk::Compressor::new(ZSTD_LEVEL).map_err(|e| e.to_string())?;
        Ok(Self {
            compressor,
            pending: Vec::with_capacity(BLOCK_BYTES),
            offsets: vec![0],
            compressed: Vec::new(),
            uncompressed_bytes: 0,
        })
    }

    fn push(&mut self, mut input: &[u8]) -> Result<(), String> {
        while !input.is_empty() {
            let space = BLOCK_BYTES - self.pending.len();
            let take = input.len().min(space);
            self.pending.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.pending.len() == BLOCK_BYTES {
                self.flush_block()?;
            }
        }
        Ok(())
    }

    fn flush_block(&mut self) -> Result<(), String> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.uncompressed_bytes = self
            .uncompressed_bytes
            .checked_add(self.pending.len() as u64)
            .ok_or("Section text offset overflow")?;
        let block = self
            .compressor
            .compress(&self.pending)
            .map_err(|e| format!("Zstd block compression failed: {e}"))?;
        self.compressed.extend_from_slice(&block);
        self.offsets.push(self.compressed.len() as u64);
        self.pending.clear();
        Ok(())
    }

    fn write_into(mut self, output: &mut dyn std::io::Write) -> Result<(), String> {
        self.flush_block()?;
        let block_count =
            u32::try_from(self.offsets.len() - 1).map_err(|_| "Text block count exceeds u32")?;
        let table_bytes = self
            .offsets
            .len()
            .checked_mul(8)
            .ok_or("Text block table overflow")?;
        let payload_bytes = 16_usize
            .checked_add(table_bytes)
            .and_then(|n| n.checked_add(self.compressed.len()))
            .ok_or("Text segment length overflow")?;
        let mut header = Writer::new(TEXT_ZSTD_KIND, 0, 0).finish()?;
        let total = (header.len() as u64)
            .checked_add(payload_bytes as u64)
            .ok_or("Text segment length overflow")?;
        header[16..24].copy_from_slice(&total.to_le_bytes());
        header[24..32].copy_from_slice(&u64::from(block_count).to_le_bytes());
        output.write_all(&header).map_err(|e| e.to_string())?;
        output
            .write_all(&self.uncompressed_bytes.to_le_bytes())
            .map_err(|e| e.to_string())?;
        output
            .write_all(&(BLOCK_BYTES as u32).to_le_bytes())
            .map_err(|e| e.to_string())?;
        output
            .write_all(&0_u32.to_le_bytes())
            .map_err(|e| e.to_string())?;
        for offset in &self.offsets {
            output
                .write_all(&offset.to_le_bytes())
                .map_err(|e| e.to_string())?;
        }
        output
            .write_all(&self.compressed)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub fn encode(sections: &[SectionText]) -> Result<(Vec<u8>, Vec<u8>), String> {
    encode_with_compression(sections, compress_enabled()?)
}

pub fn encode_with_compression(
    sections: &[SectionText],
    compress: bool,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let count = u32::try_from(sections.len()).map_err(|_| "Section count exceeds u32")?;
    let mut records = Writer::new(SECTIONS_KIND, count, RECORD_BYTES);
    let mut raw_text = Vec::new();
    let mut offset = 0_u64;
    let mut add = |value: &str| -> Result<(u64, u64), String> {
        let length = u64::try_from(value.len()).map_err(|_| "Section text exceeds u64")?;
        let start = offset;
        offset = offset
            .checked_add(length)
            .ok_or("Section text offset overflow")?;
        raw_text.extend_from_slice(value.as_bytes());
        Ok((start, length))
    };
    // Entities are concatenated strings in the same blob; a separate list of
    // length-prefixed UTF-8 names starts at the entity descriptor's offset.
    // A second pass appends entity lists after ordinary text.
    let mut descriptors = Vec::with_capacity(sections.len());
    for section in sections {
        let title = add(&section.title)?;
        let body = add(&section.body)?;
        let filename = match &section.filename {
            Some(name) => add(name)?,
            None => (ABSENT, 0),
        };
        descriptors.push((title, body, filename));
    }
    for (section, (title, body, filename)) in sections.iter().zip(descriptors) {
        for (start, length) in [title, body, filename] {
            records.u64(start);
            records.u64(length);
        }
        records.u64(section.line_number);
        records.u64(offset);
        records
            .u32(u32::try_from(section.entities.len()).map_err(|_| "Section entities exceed u32")?);
        records.u32(0);
        for entity in &section.entities {
            let length = u64::try_from(entity.len()).map_err(|_| "Entity text exceeds u64")?;
            let varint_bytes = varint_length(length);
            offset = offset
                .checked_add(varint_bytes)
                .and_then(|n| n.checked_add(length))
                .ok_or("Entity text offset overflow")?;
            write_varint(length, &mut raw_text);
            raw_text.extend_from_slice(entity.as_bytes());
        }
    }
    let text_bytes = if compress {
        let mut encoder = BlockEncoder::new()?;
        encoder.push(&raw_text)?;
        let mut out = Vec::new();
        encoder.write_into(&mut out)?;
        out
    } else {
        let mut text = Writer::new(TEXT_KIND, 0, 0);
        text.raw(&raw_text);
        text.finish()?
    };
    Ok((records.finish()?, text_bytes))
}

fn write_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Encode descriptors from borrowed search sections, without cloning bodies.
pub fn borrowed_table(sections: &[crate::bm25::Section]) -> Result<Vec<u8>, String> {
    let count = u32::try_from(sections.len()).map_err(|_| "Section count exceeds u32")?;
    let mut entity_offset = ordinary_bytes(sections)?;
    let mut ordinary_offset = 0_u64;
    let mut records = Writer::new(SECTIONS_KIND, count, RECORD_BYTES);
    for section in sections {
        for value in [
            Some(section.title.as_str()),
            Some(section.body.as_str()),
            section.filename.as_deref(),
        ] {
            if let Some(value) = value {
                records.u64(ordinary_offset);
                records.u64(value.len() as u64);
                ordinary_offset = ordinary_offset
                    .checked_add(value.len() as u64)
                    .ok_or("Section text offset overflow")?;
            } else {
                records.u64(ABSENT);
                records.u64(0);
            }
        }
        records.u64(section.line_number as u64);
        records.u64(entity_offset);
        records
            .u32(u32::try_from(section.entities.len()).map_err(|_| "Section entities exceed u32")?);
        records.u32(0);
        for entity in &section.entities {
            let length = entity.len() as u64;
            entity_offset = entity_offset
                .checked_add(varint_length(length))
                .and_then(|n| n.checked_add(length))
                .ok_or("Entity text offset overflow")?;
        }
    }
    records.finish()
}

fn ordinary_bytes(sections: &[crate::bm25::Section]) -> Result<u64, String> {
    sections.iter().try_fold(0_u64, |total, section| {
        [
            Some(section.title.as_str()),
            Some(section.body.as_str()),
            section.filename.as_deref(),
        ]
        .into_iter()
        .flatten()
        .try_fold(total, |n, value| {
            n.checked_add(value.len() as u64)
                .ok_or("Section text offset overflow".into())
        })
    })
}

fn varint_length(mut value: u64) -> u64 {
    let mut length = 1;
    while value >= 128 {
        value >>= 7;
        length += 1;
    }
    length
}

/// The length is known before writing, so no corpus-sized buffer or seek is needed.
pub fn write_borrowed_text(
    sections: &[crate::bm25::Section],
    output: &mut dyn std::io::Write,
) -> Result<(), String> {
    write_borrowed_text_with_compression(sections, output, compress_enabled()?)
}

pub fn write_borrowed_text_with_compression(
    sections: &[crate::bm25::Section],
    output: &mut dyn std::io::Write,
    compress: bool,
) -> Result<(), String> {
    let mut length = ordinary_bytes(sections)?;
    for section in sections {
        for entity in &section.entities {
            let bytes = entity.len() as u64;
            length = length
                .checked_add(varint_length(bytes))
                .and_then(|n| n.checked_add(bytes))
                .ok_or("Entity text offset overflow")?;
        }
    }
    if compress {
        let mut encoder = BlockEncoder::new()?;
        for section in sections {
            for value in [
                Some(section.title.as_str()),
                Some(section.body.as_str()),
                section.filename.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                encoder.push(value.as_bytes())?;
            }
        }
        for section in sections {
            for entity in &section.entities {
                let mut value = entity.len() as u64;
                let mut bytes = [0_u8; 10];
                let mut used = 0;
                while value >= 128 {
                    bytes[used] = (value as u8 & 0x7f) | 0x80;
                    used += 1;
                    value >>= 7;
                }
                bytes[used] = value as u8;
                encoder.push(&bytes[..used + 1])?;
                encoder.push(entity.as_bytes())?;
            }
        }
        return encoder.write_into(output);
    }
    let mut header = Writer::new(TEXT_KIND, 0, 0).finish()?;
    let total = length
        .checked_add(header.len() as u64)
        .ok_or("Text segment length overflow")?;
    header[16..24].copy_from_slice(&total.to_le_bytes());
    output.write_all(&header).map_err(|e| e.to_string())?;
    for section in sections {
        for value in [
            Some(section.title.as_str()),
            Some(section.body.as_str()),
            section.filename.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            output
                .write_all(value.as_bytes())
                .map_err(|e| e.to_string())?;
        }
    }
    for section in sections {
        for entity in &section.entities {
            let mut value = entity.len() as u64;
            let mut bytes = [0_u8; 10];
            let mut used = 0;
            while value >= 128 {
                bytes[used] = (value as u8 & 0x7f) | 0x80;
                used += 1;
                value >>= 7;
            }
            bytes[used] = value as u8;
            output
                .write_all(&bytes[..used + 1])
                .map_err(|e| e.to_string())?;
            output
                .write_all(entity.as_bytes())
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Checked view over a `text.bin` segment (`TEXT_KIND = 5` or `TEXT_ZSTD_KIND = 10`).
pub enum TextBlob<'a> {
    Uncompressed(&'a [u8]),
    Zstd {
        uncompressed_bytes: usize,
        block_count: usize,
        offsets_raw: &'a [u8],
        compressed_data: &'a [u8],
    },
}

impl<'a> TextBlob<'a> {
    pub fn open(text: &'a [u8]) -> Result<Self, String> {
        let kind = text
            .get(8..10)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
            .ok_or("Invalid or truncated v4 segment header")?;
        match kind {
            TEXT_KIND => {
                let mut blob = Reader::new(text, TEXT_KIND)?;
                if blob.count != 0 || blob.record_bytes != 0 {
                    return Err("Invalid section text framing".into());
                }
                let payload = blob.raw(blob.remaining())?;
                blob.finish()?;
                Ok(Self::Uncompressed(payload))
            }
            TEXT_ZSTD_KIND => {
                let mut blob = Reader::new(text, TEXT_ZSTD_KIND)?;
                if blob.record_bytes != 0 {
                    return Err("Invalid compressed section text framing".into());
                }
                let uncompressed_u64 = blob.u64()?;
                let block_bytes = blob.u32()?;
                let reserved = blob.u32()?;
                if block_bytes != BLOCK_BYTES as u32 || reserved != 0 {
                    return Err("Invalid compressed section text parameters".into());
                }
                let expected_blocks = if uncompressed_u64 == 0 {
                    0
                } else {
                    uncompressed_u64.div_ceil(BLOCK_BYTES as u64)
                };
                if u64::from(blob.count) != expected_blocks {
                    return Err("Compressed section text block count mismatch".into());
                }
                let uncompressed_bytes = usize::try_from(uncompressed_u64)
                    .map_err(|_| "Section text length exceeds usize")?;
                let block_count = blob.count as usize;
                let table_entries = block_count
                    .checked_add(1)
                    .ok_or("Text block count overflow")?;
                let table_bytes = table_entries
                    .checked_mul(8)
                    .ok_or("Text block table size overflow")?;
                let offsets_raw = blob.raw(table_bytes)?;
                let compressed_data = blob.raw(blob.remaining())?;
                blob.finish()?;

                let read_offset = |idx: usize| -> u64 {
                    u64::from_le_bytes(offsets_raw[idx * 8..idx * 8 + 8].try_into().unwrap())
                };
                if read_offset(0) != 0 {
                    return Err("Nonzero initial compressed text block offset".into());
                }
                let mut prev = 0_u64;
                for idx in 1..=block_count {
                    let current = read_offset(idx);
                    if current <= prev {
                        return Err("Nonmonotonic compressed text block offsets".into());
                    }
                    prev = current;
                }
                if prev != compressed_data.len() as u64 {
                    return Err("Compressed text block table end mismatch".into());
                }
                Ok(Self::Zstd {
                    uncompressed_bytes,
                    block_count,
                    offsets_raw,
                    compressed_data,
                })
            }
            _ => Err("Unsupported v4 segment kind or version".into()),
        }
    }

    pub fn uncompressed_len(&self) -> usize {
        match self {
            Self::Uncompressed(payload) => payload.len(),
            Self::Zstd {
                uncompressed_bytes, ..
            } => *uncompressed_bytes,
        }
    }

    pub fn is_compressed(&self) -> bool {
        matches!(self, Self::Zstd { .. })
    }

    pub fn block_count(&self) -> usize {
        match self {
            Self::Uncompressed(payload) => payload.len().div_ceil(BLOCK_BYTES),
            Self::Zstd { block_count, .. } => *block_count,
        }
    }

    fn decode_zstd_block_into(
        &self,
        block_index: usize,
        decompressor: &mut zstd::bulk::Decompressor<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), String> {
        let Self::Zstd {
            uncompressed_bytes,
            block_count,
            offsets_raw,
            compressed_data,
        } = self
        else {
            return Err("Not a zstd text blob".into());
        };
        if block_index >= *block_count {
            return Err("Compressed text block index out of bounds".into());
        }
        let logical_start = block_index
            .checked_mul(BLOCK_BYTES)
            .ok_or("Compressed text block offset overflow")?;
        let expected_len = (*uncompressed_bytes - logical_start).min(BLOCK_BYTES);
        let start = usize::try_from(u64::from_le_bytes(
            offsets_raw[block_index * 8..block_index * 8 + 8]
                .try_into()
                .unwrap(),
        ))
        .map_err(|_| "Compressed block offset exceeds usize")?;
        let end = usize::try_from(u64::from_le_bytes(
            offsets_raw[(block_index + 1) * 8..(block_index + 1) * 8 + 8]
                .try_into()
                .unwrap(),
        ))
        .map_err(|_| "Compressed block offset exceeds usize")?;
        let frame = compressed_data
            .get(start..end)
            .ok_or("Compressed block range out of bounds")?;
        out.resize(expected_len, 0);
        let written = decompressor
            .decompress_to_buffer(frame, out)
            .map_err(|e| format!("Invalid compressed section text block: {e}"))?;
        if written != expected_len {
            return Err("Decompressed section text block length mismatch".into());
        }
        Ok(())
    }

    /// Decode only the 64 KiB block(s) overlapping `[offset, offset + length)`.
    pub fn decode_span(&self, offset: u64, length: u64) -> Result<Cow<'a, str>, String> {
        let start = usize::try_from(offset).map_err(|_| "Section offset exceeds usize")?;
        let length = usize::try_from(length).map_err(|_| "Section length exceeds usize")?;
        let end = start
            .checked_add(length)
            .ok_or("Section text range overflow")?;
        if end > self.uncompressed_len() {
            return Err("Section text range exceeds blob".into());
        }
        match self {
            Self::Uncompressed(payload) => {
                let s = std::str::from_utf8(&payload[start..end])
                    .map_err(|_| "Invalid section UTF-8")?;
                Ok(Cow::Borrowed(s))
            }
            Self::Zstd { .. } => {
                if length == 0 {
                    return Ok(Cow::Borrowed(""));
                }
                let mut reader = CachedBlockReader::new(self)?;
                reader.read_string(start, length).map(Cow::Owned)
            }
        }
    }
}

struct CachedBlockReader<'a, 'b> {
    blob: &'b TextBlob<'a>,
    decompressor: zstd::bulk::Decompressor<'static>,
    cached_index: Option<usize>,
    cached_block: Vec<u8>,
}

impl<'a, 'b> CachedBlockReader<'a, 'b> {
    fn new(blob: &'b TextBlob<'a>) -> Result<Self, String> {
        let decompressor = zstd::bulk::Decompressor::new().map_err(|e| e.to_string())?;
        Ok(Self {
            blob,
            decompressor,
            cached_index: None,
            cached_block: Vec::with_capacity(BLOCK_BYTES),
        })
    }

    fn ensure_block(&mut self, block_index: usize) -> Result<&[u8], String> {
        if self.cached_index != Some(block_index) {
            self.blob.decode_zstd_block_into(
                block_index,
                &mut self.decompressor,
                &mut self.cached_block,
            )?;
            self.cached_index = Some(block_index);
        }
        Ok(&self.cached_block)
    }

    fn read_byte(&mut self, offset: usize) -> Result<u8, String> {
        if offset >= self.blob.uncompressed_len() {
            return Err("Truncated entity length".into());
        }
        let block_index = offset / BLOCK_BYTES;
        let in_block = offset % BLOCK_BYTES;
        let block = self.ensure_block(block_index)?;
        Ok(block[in_block])
    }

    fn read_string(&mut self, start: usize, length: usize) -> Result<String, String> {
        let end = start
            .checked_add(length)
            .ok_or("Section text range overflow")?;
        if end > self.blob.uncompressed_len() {
            return Err("Section text range exceeds blob".into());
        }
        if length == 0 {
            return Ok(String::new());
        }
        let first_block = start / BLOCK_BYTES;
        let last_block = (end - 1) / BLOCK_BYTES;
        if first_block == last_block {
            let in_block = start % BLOCK_BYTES;
            let block = self.ensure_block(first_block)?;
            let slice = &block[in_block..in_block + length];
            return std::str::from_utf8(slice)
                .map(str::to_owned)
                .map_err(|_| "Invalid section UTF-8".into());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| "Cannot allocate section string")?;
        let mut cursor = start;
        while cursor < end {
            let block_index = cursor / BLOCK_BYTES;
            let in_block = cursor % BLOCK_BYTES;
            let block = self.ensure_block(block_index)?;
            let take = (block.len() - in_block).min(end - cursor);
            bytes.extend_from_slice(&block[in_block..in_block + take]);
            cursor += take;
        }
        String::from_utf8(bytes).map_err(|_| "Invalid section UTF-8".into())
    }

    fn read_entities(
        &mut self,
        offset: usize,
        count: usize,
    ) -> Result<(Vec<String>, usize), String> {
        let total_len = self.blob.uncompressed_len();
        if offset > total_len {
            return Err("Entity offset exceeds blob".into());
        }
        if count > total_len - offset {
            return Err("Entity count exceeds payload".into());
        }
        let mut cursor = offset;
        let mut entities = Vec::new();
        entities
            .try_reserve_exact(count)
            .map_err(|_| "Cannot allocate entity names")?;
        for _ in 0..count {
            let mut length = 0_u64;
            let mut complete = false;
            for i in 0..10 {
                let byte = self.read_byte(cursor)?;
                cursor += 1;
                if i == 9 && byte > 1 {
                    return Err("Entity length overflow".into());
                }
                length |= u64::from(byte & 0x7f) << (7 * i);
                if byte & 0x80 == 0 {
                    if i > 0 && byte == 0 {
                        return Err("Noncanonical entity length".into());
                    }
                    complete = true;
                    break;
                }
            }
            if !complete {
                return Err("Entity length overflow".into());
            }
            let length = usize::try_from(length).map_err(|_| "Entity length exceeds usize")?;
            let end = cursor.checked_add(length).ok_or("Entity range overflow")?;
            if end > total_len {
                return Err("Truncated entity text".into());
            }
            let name = self
                .read_string(cursor, length)
                .map_err(|_| "Invalid entity UTF-8")?;
            entities.push(name);
            cursor = end;
        }
        Ok((entities, cursor))
    }
}

/// Decode a single UTF-8 span `[offset, offset + length)` from `text.bin`,
/// decompressing only the 64 KiB block(s) overlapping the span.
pub fn decode_span(text: &[u8], offset: u64, length: u64) -> Result<String, String> {
    Ok(TextBlob::open(text)?
        .decode_span(offset, length)?
        .into_owned())
}

/// Decode a single section from `sections.tbl` and `text.bin` by index,
/// decompressing only the 64 KiB block(s) referenced by that section.
pub fn decode_section(
    records: &[u8],
    text: &[u8],
    section_index: usize,
) -> Result<SectionText, String> {
    let table = Reader::new(records, SECTIONS_KIND)?;
    if table.record_bytes != RECORD_BYTES {
        return Err("Invalid section descriptor width".into());
    }
    if section_index >= table.count as usize {
        return Err("Section index out of bounds".into());
    }
    let blob = TextBlob::open(text)?;
    let record_start = 40_usize
        .checked_add(
            section_index
                .checked_mul(RECORD_BYTES as usize)
                .ok_or("Section descriptor offset overflow")?,
        )
        .ok_or("Section descriptor offset overflow")?;
    let record = records
        .get(record_start..record_start + RECORD_BYTES as usize)
        .ok_or("Truncated section descriptor")?;
    let u64_at = |off: usize| u64::from_le_bytes(record[off..off + 8].try_into().unwrap());
    let u32_at = |off: usize| u32::from_le_bytes(record[off..off + 4].try_into().unwrap());

    let title_off = u64_at(0);
    let title_len = u64_at(8);
    let body_off = u64_at(16);
    let body_len = u64_at(24);
    let file_off = u64_at(32);
    let file_len = u64_at(40);
    let line_number = u64_at(48);
    let entity_off = usize::try_from(u64_at(56)).map_err(|_| "Entity offset exceeds usize")?;
    let entity_count = u32_at(64) as usize;
    if u32_at(68) != 0 {
        return Err("Nonzero reserved section bits".into());
    }
    if title_off
        .checked_add(title_len)
        .ok_or("Section text range overflow")?
        != body_off
    {
        return Err("Noncontiguous section text offsets".into());
    }
    match &blob {
        TextBlob::Uncompressed(_) => {
            let title = blob.decode_span(title_off, title_len)?.into_owned();
            let body = blob.decode_span(body_off, body_len)?.into_owned();
            let filename = if file_off == ABSENT {
                if file_len != 0 {
                    return Err("Absent filename has nonzero length".into());
                }
                None
            } else {
                if body_off
                    .checked_add(body_len)
                    .ok_or("Section text range overflow")?
                    != file_off
                {
                    return Err("Noncontiguous section text offsets".into());
                }
                Some(blob.decode_span(file_off, file_len)?.into_owned())
            };
            let TextBlob::Uncompressed(payload) = blob else {
                unreachable!()
            };
            let (entities, _) = decode_uncompressed_entities(payload, entity_off, entity_count)?;
            Ok(SectionText {
                title,
                body,
                filename,
                line_number,
                entities,
            })
        }
        TextBlob::Zstd { .. } => {
            let mut reader = CachedBlockReader::new(&blob)?;
            let title = reader.read_string(
                usize::try_from(title_off).map_err(|_| "Section offset exceeds usize")?,
                usize::try_from(title_len).map_err(|_| "Section length exceeds usize")?,
            )?;
            let body = reader.read_string(
                usize::try_from(body_off).map_err(|_| "Section offset exceeds usize")?,
                usize::try_from(body_len).map_err(|_| "Section length exceeds usize")?,
            )?;
            let filename = if file_off == ABSENT {
                if file_len != 0 {
                    return Err("Absent filename has nonzero length".into());
                }
                None
            } else {
                if body_off
                    .checked_add(body_len)
                    .ok_or("Section text range overflow")?
                    != file_off
                {
                    return Err("Noncontiguous section text offsets".into());
                }
                Some(reader.read_string(
                    usize::try_from(file_off).map_err(|_| "Section offset exceeds usize")?,
                    usize::try_from(file_len).map_err(|_| "Section length exceeds usize")?,
                )?)
            };
            let (entities, _) = reader.read_entities(entity_off, entity_count)?;
            Ok(SectionText {
                title,
                body,
                filename,
                line_number,
                entities,
            })
        }
    }
}

fn decode_uncompressed_entities(
    payload: &[u8],
    offset: usize,
    count: usize,
) -> Result<(Vec<String>, usize), String> {
    let bytes = payload.get(offset..).ok_or("Entity offset exceeds blob")?;
    if count > bytes.len() {
        return Err("Entity count exceeds payload".into());
    }
    let mut cursor = 0_usize;
    let mut entities = Vec::new();
    entities
        .try_reserve_exact(count)
        .map_err(|_| "Cannot allocate entity names")?;
    for _ in 0..count {
        let mut length = 0_u64;
        let mut complete = false;
        for i in 0..10 {
            let byte = *bytes.get(cursor).ok_or("Truncated entity length")?;
            cursor += 1;
            if i == 9 && byte > 1 {
                return Err("Entity length overflow".into());
            }
            length |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                if i > 0 && byte == 0 {
                    return Err("Noncanonical entity length".into());
                }
                complete = true;
                break;
            }
        }
        if !complete {
            return Err("Entity length overflow".into());
        }
        let length = usize::try_from(length).map_err(|_| "Entity length exceeds usize")?;
        let end = cursor.checked_add(length).ok_or("Entity range overflow")?;
        let name = bytes.get(cursor..end).ok_or("Truncated entity text")?;
        entities.push(
            std::str::from_utf8(name)
                .map_err(|_| "Invalid entity UTF-8")?
                .to_owned(),
        );
        cursor = end;
    }
    let entity_end = offset
        .checked_add(cursor)
        .ok_or("Entity text range overflow")?;
    Ok((entities, entity_end))
}

pub fn decode(records: &[u8], text: &[u8]) -> Result<Vec<SectionText>, String> {
    let mut table = Reader::new(records, SECTIONS_KIND)?;
    if table.record_bytes != RECORD_BYTES {
        return Err("Invalid section descriptor width".into());
    }
    let blob = TextBlob::open(text)?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(table.count as usize)
        .map_err(|_| "Cannot allocate sections")?;
    let mut ordinary_end = 0_usize;
    let mut entity_ranges = Vec::new();
    entity_ranges
        .try_reserve_exact(table.count as usize)
        .map_err(|_| "Cannot allocate entity ranges")?;

    match &blob {
        TextBlob::Uncompressed(payload) => {
            for _ in 0..table.count {
                let mut string = |optional: bool| -> Result<Option<String>, String> {
                    let offset = table.u64()?;
                    let length = table.u64()?;
                    if optional && offset == ABSENT {
                        if length != 0 {
                            return Err("Absent filename has nonzero length".into());
                        }
                        return Ok(None);
                    }
                    let start =
                        usize::try_from(offset).map_err(|_| "Section offset exceeds usize")?;
                    let length =
                        usize::try_from(length).map_err(|_| "Section length exceeds usize")?;
                    if start != ordinary_end {
                        return Err("Noncontiguous section text offsets".into());
                    }
                    let end = start
                        .checked_add(length)
                        .ok_or("Section text range overflow")?;
                    ordinary_end = end;
                    let bytes = payload
                        .get(start..end)
                        .ok_or("Section text range exceeds blob")?;
                    let value = std::str::from_utf8(bytes).map_err(|_| "Invalid section UTF-8")?;
                    Ok(Some(value.to_owned()))
                };
                let title = string(false)?.unwrap();
                let body = string(false)?.unwrap();
                let filename = string(true)?;
                let line_number = table.u64()?;
                let offset =
                    usize::try_from(table.u64()?).map_err(|_| "Entity offset exceeds usize")?;
                let count = table.u32()? as usize;
                if table.u32()? != 0 {
                    return Err("Nonzero reserved section bits".into());
                }
                entity_ranges.push((offset, count));
                sections.push(SectionText {
                    title,
                    body,
                    filename,
                    line_number,
                    entities: Vec::new(),
                });
            }
            table.finish()?;
            let mut entity_end = ordinary_end;
            for (section, (offset, count)) in sections.iter_mut().zip(entity_ranges) {
                if offset != entity_end {
                    return Err("Noncontiguous entity offsets".into());
                }
                let (entities, next_end) = decode_uncompressed_entities(payload, offset, count)?;
                entity_end = next_end;
                section.entities = entities;
            }
            if entity_end != payload.len() {
                return Err("Trailing section text bytes".into());
            }
        }
        TextBlob::Zstd {
            uncompressed_bytes, ..
        } => {
            let mut reader = CachedBlockReader::new(&blob)?;
            for _ in 0..table.count {
                let mut string = |optional: bool| -> Result<Option<String>, String> {
                    let offset = table.u64()?;
                    let length = table.u64()?;
                    if optional && offset == ABSENT {
                        if length != 0 {
                            return Err("Absent filename has nonzero length".into());
                        }
                        return Ok(None);
                    }
                    let start =
                        usize::try_from(offset).map_err(|_| "Section offset exceeds usize")?;
                    let length =
                        usize::try_from(length).map_err(|_| "Section length exceeds usize")?;
                    if start != ordinary_end {
                        return Err("Noncontiguous section text offsets".into());
                    }
                    let end = start
                        .checked_add(length)
                        .ok_or("Section text range overflow")?;
                    ordinary_end = end;
                    Ok(Some(reader.read_string(start, length)?))
                };
                let title = string(false)?.unwrap();
                let body = string(false)?.unwrap();
                let filename = string(true)?;
                let line_number = table.u64()?;
                let offset =
                    usize::try_from(table.u64()?).map_err(|_| "Entity offset exceeds usize")?;
                let count = table.u32()? as usize;
                if table.u32()? != 0 {
                    return Err("Nonzero reserved section bits".into());
                }
                entity_ranges.push((offset, count));
                sections.push(SectionText {
                    title,
                    body,
                    filename,
                    line_number,
                    entities: Vec::new(),
                });
            }
            table.finish()?;
            let mut entity_end = ordinary_end;
            for (section, (offset, count)) in sections.iter_mut().zip(entity_ranges) {
                if offset != entity_end {
                    return Err("Noncontiguous entity offsets".into());
                }
                let (entities, next_end) = reader.read_entities(offset, count)?;
                entity_end = next_end;
                section.entities = entities;
            }
            if entity_end != *uncompressed_bytes {
                return Err("Trailing section text bytes".into());
            }
        }
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_stream_matches_owned_codec_and_propagates_write_failure() {
        let borrowed = vec![
            crate::bm25::Section {
                title: "café".into(),
                body: "bilge\nwater\0".into(),
                filename: None,
                line_number: usize::MAX,
                entities: vec!["pump".into(), "x".repeat(129)],
            },
            crate::bm25::Section {
                title: String::new(),
                body: String::new(),
                filename: Some(String::new()),
                line_number: 0,
                entities: Vec::new(),
            },
        ];
        let owned: Vec<_> = borrowed
            .iter()
            .map(|section| SectionText {
                title: section.title.clone(),
                body: section.body.clone(),
                filename: section.filename.clone(),
                line_number: section.line_number as u64,
                entities: section.entities.clone(),
            })
            .collect();
        for compress in [false, true] {
            let expected = encode_with_compression(&owned, compress).unwrap();
            let table = borrowed_table(&borrowed).unwrap();
            let mut text = Vec::new();
            write_borrowed_text_with_compression(&borrowed, &mut text, compress).unwrap();
            assert_eq!((table, text), expected);
            let mut empty = Vec::new();
            write_borrowed_text_with_compression(&[], &mut empty, compress).unwrap();
            assert_eq!(
                (borrowed_table(&[]).unwrap(), empty),
                encode_with_compression(&[], compress).unwrap()
            );
        }
        struct Failed;
        impl std::io::Write for Failed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected write failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(
            write_borrowed_text_with_compression(&borrowed, &mut Failed, false)
                .unwrap_err()
                .contains("injected write failure")
        );
        assert!(
            write_borrowed_text_with_compression(&borrowed, &mut Failed, true)
                .unwrap_err()
                .contains("injected write failure")
        );
    }

    #[test]
    fn all_text_and_optional_fields_round_trip() {
        let sections = vec![
            SectionText {
                title: "café".into(),
                body: "bilge\nwater\0".into(),
                filename: None,
                line_number: u64::MAX,
                entities: vec!["pump".into(), "".into()],
            },
            SectionText {
                title: "".into(),
                body: "".into(),
                filename: Some("".into()),
                line_number: 0,
                entities: vec![],
            },
        ];
        for compress in [false, true] {
            let (records, text) = encode_with_compression(&sections, compress).unwrap();
            assert_eq!(decode(&records, &text).unwrap(), sections);
            assert_eq!(decode_section(&records, &text, 0).unwrap(), sections[0]);
            assert_eq!(decode_section(&records, &text, 1).unwrap(), sections[1]);
            assert!(decode_section(&records, &text, 2).is_err());
            let (records, text) = encode_with_compression(&[], compress).unwrap();
            assert!(decode(&records, &text).unwrap().is_empty());
        }
    }

    #[test]
    fn zstd_blocks_decode_only_target_block_for_snippets_and_fail_closed_on_corruption() {
        // Build 6 sections (~30 KiB each, ~180 KiB total across 3 x 64 KiB blocks)
        // with no entities so each early section is self-contained in its block.
        let sections: Vec<SectionText> = (0..6)
            .map(|idx| SectionText {
                title: format!("Section Title {idx}"),
                body: format!("section-{idx}: ") + &"marine engine cooling bilge ".repeat(1100),
                filename: Some(format!("manual-{idx}.md")),
                line_number: (idx + 1) as u64,
                entities: Vec::new(),
            })
            .collect();
        let (records, text) = encode_with_compression(&sections, true).unwrap();
        let blob = TextBlob::open(&text).unwrap();
        assert!(blob.is_compressed());
        assert_eq!(blob.block_count(), 3);
        assert!(text.len() < blob.uncompressed_len() / 5);
        assert_eq!(decode(&records, &text).unwrap(), sections);

        // Corrupt the compressed payload of block 2 (the final block) while keeping
        // block 0 intact. Decoding section 0 (which resides entirely in block 0)
        // must succeed without touching block 2, whereas full decode must fail closed.
        let block2_start_offset = 40 + 16 + 2 * 8;
        let block2_rel = u64::from_le_bytes(
            text[block2_start_offset..block2_start_offset + 8]
                .try_into()
                .unwrap(),
        ) as usize;
        let data_start = 40 + 16 + (blob.block_count() + 1) * 8;
        let mut damaged_last_block = text.clone();
        damaged_last_block[data_start + block2_rel + 4] ^= 0xff;
        assert_eq!(
            decode_section(&records, &damaged_last_block, 0).unwrap(),
            sections[0]
        );
        assert!(decode(&records, &damaged_last_block).is_err());
    }

    #[test]
    fn malformed_offsets_lengths_and_utf8_are_rejected() {
        let sections = vec![SectionText {
            title: "a".into(),
            body: "b".into(),
            filename: None,
            line_number: 1,
            entities: vec!["c".into()],
        }];
        let (records, text) = encode_with_compression(&sections, false).unwrap();
        for n in 0..records.len() {
            assert!(decode(&records[..n], &text).is_err());
        }
        for n in 0..text.len() {
            assert!(decode(&records, &text[..n]).is_err());
        }
        for offset in [40, 48, 56, 64, 80, 96, 104, 108] {
            let mut bad = records.clone();
            bad[offset] = 0xff;
            assert!(decode(&bad, &text).is_err(), "{offset}");
        }
        let mut bad = text.clone();
        bad[40] = 0xff;
        assert!(decode(&records, &bad).is_err());

        let (_, ztext) = encode_with_compression(&sections, true).unwrap();
        for n in 0..ztext.len() {
            assert!(decode(&records, &ztext[..n]).is_err());
        }
        // Corrupt uncompressed_bytes, block_bytes, reserved, or offsets in zstd text header.
        for offset in [40, 48, 52, 56, 64] {
            let mut bad = ztext.clone();
            bad[offset] ^= 1;
            assert!(decode(&records, &bad).is_err(), "zstd offset {offset}");
        }
    }
}
