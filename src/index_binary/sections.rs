//! Fixed section descriptors plus a checked UTF-8 blob. The text is stored once.
use super::codec::{Reader, Writer};

const SECTIONS_KIND: u16 = 4;
const TEXT_KIND: u16 = 5;
const RECORD_BYTES: u32 = 72;
const ABSENT: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionText {
    pub title: String,
    pub body: String,
    pub filename: Option<String>,
    pub line_number: u64,
    pub entities: Vec<String>,
}

pub fn encode(sections: &[SectionText]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let count = u32::try_from(sections.len()).map_err(|_| "Section count exceeds u32")?;
    let mut records = Writer::new(SECTIONS_KIND, count, RECORD_BYTES);
    let mut text = Writer::new(TEXT_KIND, 0, 0);
    let mut offset = 0_u64;
    let mut add = |value: &str| -> Result<(u64, u64), String> {
        let length = u64::try_from(value.len()).map_err(|_| "Section text exceeds u64")?;
        let start = offset;
        offset = offset
            .checked_add(length)
            .ok_or("Section text offset overflow")?;
        text.raw(value.as_bytes());
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
            let mut value = length;
            let mut varint_bytes = 1_u64;
            while value >= 128 {
                value >>= 7;
                varint_bytes += 1;
            }
            offset = offset
                .checked_add(varint_bytes)
                .and_then(|n| n.checked_add(length))
                .ok_or("Entity text offset overflow")?;
            text.varint(length);
            text.raw(entity.as_bytes());
        }
    }
    Ok((records.finish()?, text.finish()?))
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

pub fn decode(records: &[u8], text: &[u8]) -> Result<Vec<SectionText>, String> {
    let mut table = Reader::new(records, SECTIONS_KIND)?;
    if table.record_bytes != RECORD_BYTES {
        return Err("Invalid section descriptor width".into());
    }
    let mut blob = Reader::new(text, TEXT_KIND)?;
    if blob.count != 0 || blob.record_bytes != 0 {
        return Err("Invalid section text framing".into());
    }
    let payload = blob.raw(blob.remaining())?;
    blob.finish()?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(table.count as usize)
        .map_err(|_| "Cannot allocate sections")?;
    let mut ordinary_end = 0_usize;
    let mut entity_ranges = Vec::new();
    entity_ranges
        .try_reserve_exact(table.count as usize)
        .map_err(|_| "Cannot allocate entity ranges")?;
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
            let start = usize::try_from(offset).map_err(|_| "Section offset exceeds usize")?;
            let length = usize::try_from(length).map_err(|_| "Section length exceeds usize")?;
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
        let offset = usize::try_from(table.u64()?).map_err(|_| "Entity offset exceeds usize")?;
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
        let bytes = payload.get(offset..).ok_or("Entity offset exceeds blob")?;
        if count > bytes.len() {
            return Err("Entity count exceeds payload".into());
        }
        // Reuse the checked canonical-varint reader on a temporary framed view
        // is avoided here: entity ranges use a borrowed cursor into the blob.
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
        entity_end = offset
            .checked_add(cursor)
            .ok_or("Entity text range overflow")?;
        section.entities = entities;
    }
    if entity_end != payload.len() {
        return Err("Trailing section text bytes".into());
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
        let expected = encode(&owned).unwrap();
        let table = borrowed_table(&borrowed).unwrap();
        let mut text = Vec::new();
        write_borrowed_text(&borrowed, &mut text).unwrap();
        assert_eq!((table, text), expected);
        let mut empty = Vec::new();
        write_borrowed_text(&[], &mut empty).unwrap();
        assert_eq!((borrowed_table(&[]).unwrap(), empty), encode(&[]).unwrap());
        struct Failed;
        impl std::io::Write for Failed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("injected write failure"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(write_borrowed_text(&borrowed, &mut Failed)
            .unwrap_err()
            .contains("injected write failure"));
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
        let (records, text) = encode(&sections).unwrap();
        assert_eq!(decode(&records, &text).unwrap(), sections);
        let (records, text) = encode(&[]).unwrap();
        assert!(decode(&records, &text).unwrap().is_empty());
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
        let (records, text) = encode(&sections).unwrap();
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
    }
}
