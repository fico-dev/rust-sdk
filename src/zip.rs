//! Reading one file out of a zip archive, without a crate: the SDK's `uv`
//! for Windows comes as a zip.
//!
//! It reads what zip writers, `uv`'s release builds included, use for small
//! archives: the central directory at the end, and entries stored or
//! compressed with deflate. ZIP64, encrypted entries and multi-disk archives
//! are rejected rather than misread.

use std::io::Read;

use flate2::Crc;
use flate2::read::DeflateDecoder;

/// The signature of the end of the central directory record.
const END_SIGNATURE: u32 = 0x0605_4b50;
/// The signature of a central directory file header.
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// The signature of a local file header.
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// The size of the end of the central directory record, without its
/// comment.
const END_LEN: usize = 22;
/// The longest comment the end record can hold.
const MAX_COMMENT_LEN: usize = u16::MAX as usize;
/// Compression method: stored.
const STORED: u16 = 0;
/// Compression method: deflate.
const DEFLATE: u16 = 8;
/// General purpose flag: the entry is encrypted.
const ENCRYPTED: u16 = 1;
/// A size or offset of `0xFFFFFFFF`, or a count of `0xFFFF`, means the
/// value is in a ZIP64 record.
const ZIP64_U32: u32 = u32::MAX;
/// See [`ZIP64_U32`].
const ZIP64_U16: u16 = u16::MAX;

/// The contents of the file `name` in the zip archive `archive`, or `None`
/// if the archive has no such file. Names are compared as stored, with `/`
/// separators.
///
/// # Errors
///
/// Returns a description of the problem if the archive is not a zip this
/// reader supports, the entry is damaged, or its contents don't match its
/// CRC-32 or size.
pub(crate) fn extract(archive: &[u8], name: &str) -> Result<Option<Vec<u8>>, String> {
    let end = find_end(archive)?;
    let entries = u16_at(archive, end + 10)?;
    let directory_len = u32_at(archive, end + 12)?;
    let directory_offset = u32_at(archive, end + 16)?;
    if u16_at(archive, end + 4)? != 0 || u16_at(archive, end + 6)? != 0 {
        return Err("multi-disk zip archives are not supported".to_owned());
    }
    if entries == ZIP64_U16 || directory_len == ZIP64_U32 || directory_offset == ZIP64_U32 {
        return Err("ZIP64 archives are not supported".to_owned());
    }
    let mut at = directory_offset as usize;
    for _ in 0..entries {
        if u32_at(archive, at)? != CENTRAL_SIGNATURE {
            return Err("the central directory is damaged".to_owned());
        }
        let flags = u16_at(archive, at + 8)?;
        let method = u16_at(archive, at + 10)?;
        let crc = u32_at(archive, at + 16)?;
        let compressed_len = u32_at(archive, at + 20)?;
        let len = u32_at(archive, at + 24)?;
        let name_len = u16_at(archive, at + 28)? as usize;
        let extra_len = u16_at(archive, at + 30)? as usize;
        let comment_len = u16_at(archive, at + 32)? as usize;
        let local_offset = u32_at(archive, at + 42)?;
        let entry_name = bytes_at(archive, at + 46, name_len)?;
        at += 46 + name_len + extra_len + comment_len;
        if entry_name != name.as_bytes() {
            continue;
        }
        if flags & ENCRYPTED != 0 {
            return Err(format!("`{name}` is encrypted"));
        }
        if compressed_len == ZIP64_U32 || len == ZIP64_U32 || local_offset == ZIP64_U32 {
            return Err("ZIP64 archives are not supported".to_owned());
        }
        let data = local_data(archive, local_offset as usize, compressed_len as usize)?;
        let contents = match method {
            STORED => data.to_vec(),
            DEFLATE => {
                let mut contents = Vec::with_capacity(len as usize);
                DeflateDecoder::new(data)
                    .take(u64::from(len) + 1)
                    .read_to_end(&mut contents)
                    .map_err(|error| format!("`{name}` can't be decompressed: {error}"))?;
                contents
            }
            other => return Err(format!("`{name}` uses compression method {other}")),
        };
        let mut actual = Crc::new();
        actual.update(&contents);
        if contents.len() != len as usize || actual.sum() != crc {
            return Err(format!("`{name}` doesn't match its size or CRC-32"));
        }
        return Ok(Some(contents));
    }
    Ok(None)
}

/// The offset of the end of the central directory record, found from the
/// end of `archive`, as its comment may follow it.
fn find_end(archive: &[u8]) -> Result<usize, String> {
    let not_a_zip = || "not a zip archive".to_owned();
    let last = archive.len().checked_sub(END_LEN).ok_or_else(not_a_zip)?;
    let first = last.saturating_sub(MAX_COMMENT_LEN);
    (first..=last)
        .rev()
        .find(|&at| u32_at(archive, at) == Ok(END_SIGNATURE))
        .ok_or_else(not_a_zip)
}

/// The `len` bytes of an entry's data, after its local file header at
/// `offset`, whose own name and extra field lengths can differ from the
/// central directory's.
fn local_data(archive: &[u8], offset: usize, len: usize) -> Result<&[u8], String> {
    if u32_at(archive, offset)? != LOCAL_SIGNATURE {
        return Err("a local file header is damaged".to_owned());
    }
    let name_len = u16_at(archive, offset + 26)? as usize;
    let extra_len = u16_at(archive, offset + 28)? as usize;
    bytes_at(archive, offset + 30 + name_len + extra_len, len)
}

/// The `len` bytes of `archive` at `at`.
fn bytes_at(archive: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    at.checked_add(len)
        .and_then(|end| archive.get(at..end))
        .ok_or_else(|| "the zip archive is truncated".to_owned())
}

/// The little-endian `u16` of `archive` at `at`.
fn u16_at(archive: &[u8], at: usize) -> Result<u16, String> {
    let bytes = bytes_at(archive, at, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

/// The little-endian `u32` of `archive` at `at`.
fn u32_at(archive: &[u8], at: usize) -> Result<u32, String> {
    let bytes = bytes_at(archive, at, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::DeflateEncoder;

    use super::*;

    /// An entry for [`zip`]: its name, contents, whether to deflate it, and
    /// its general purpose flags.
    pub(crate) struct Entry<'a> {
        pub(crate) name: &'a str,
        pub(crate) contents: &'a [u8],
        pub(crate) deflate: bool,
        pub(crate) flags: u16,
    }

    /// A zip archive of `entries`, as a zip writer lays one out, with
    /// `comment` after the end record.
    pub(crate) fn zip(entries: &[Entry], comment: &[u8]) -> Vec<u8> {
        let mut archive = Vec::new();
        let mut directory = Vec::new();
        for entry in entries {
            let data = if entry.deflate {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(entry.contents).unwrap();
                encoder.finish().unwrap()
            } else {
                entry.contents.to_vec()
            };
            let mut crc = Crc::new();
            crc.update(entry.contents);
            let method = if entry.deflate { DEFLATE } else { STORED };
            let offset = archive.len() as u32;
            // The local header, with an extra field the central directory
            // doesn't have.
            archive.extend(LOCAL_SIGNATURE.to_le_bytes());
            archive.extend(20u16.to_le_bytes());
            archive.extend(entry.flags.to_le_bytes());
            archive.extend(method.to_le_bytes());
            archive.extend([0; 4]);
            archive.extend(crc.sum().to_le_bytes());
            archive.extend((data.len() as u32).to_le_bytes());
            archive.extend((entry.contents.len() as u32).to_le_bytes());
            archive.extend((entry.name.len() as u16).to_le_bytes());
            archive.extend(4u16.to_le_bytes());
            archive.extend(entry.name.as_bytes());
            archive.extend([0xfe, 0xca, 0, 0]);
            archive.extend(&data);

            directory.extend(CENTRAL_SIGNATURE.to_le_bytes());
            directory.extend([20, 0, 20, 0]);
            directory.extend(entry.flags.to_le_bytes());
            directory.extend(method.to_le_bytes());
            directory.extend([0; 4]);
            directory.extend(crc.sum().to_le_bytes());
            directory.extend((data.len() as u32).to_le_bytes());
            directory.extend((entry.contents.len() as u32).to_le_bytes());
            directory.extend((entry.name.len() as u16).to_le_bytes());
            directory.extend([0; 12]);
            directory.extend(offset.to_le_bytes());
            directory.extend(entry.name.as_bytes());
        }
        let directory_offset = archive.len() as u32;
        archive.extend(&directory);
        archive.extend(END_SIGNATURE.to_le_bytes());
        archive.extend([0; 4]);
        archive.extend((entries.len() as u16).to_le_bytes());
        archive.extend((entries.len() as u16).to_le_bytes());
        archive.extend((directory.len() as u32).to_le_bytes());
        archive.extend(directory_offset.to_le_bytes());
        archive.extend((comment.len() as u16).to_le_bytes());
        archive.extend(comment);
        archive
    }

    fn entry<'a>(name: &'a str, contents: &'a [u8], deflate: bool) -> Entry<'a> {
        Entry {
            name,
            contents,
            deflate,
            flags: 0,
        }
    }

    #[test]
    fn extracts_stored_and_deflated_files() {
        let big = b"uv ".repeat(1000);
        let archive = zip(
            &[
                entry("uvx.exe", b"uvx", false),
                entry("uv.exe", &big, true),
                entry("dir/uvw.exe", b"uvw", false),
            ],
            b"a comment",
        );
        assert_eq!(extract(&archive, "uv.exe").unwrap().unwrap(), big);
        assert_eq!(extract(&archive, "uvx.exe").unwrap().unwrap(), b"uvx");
        assert_eq!(extract(&archive, "dir/uvw.exe").unwrap().unwrap(), b"uvw");
        assert_eq!(extract(&archive, "uvw.exe").unwrap(), None);
        let empty = zip(&[], b"");
        assert_eq!(extract(&empty, "uv.exe").unwrap(), None);
    }

    #[test]
    fn rejects_damaged_and_unsupported_archives() {
        let archive = zip(&[entry("uv.exe", b"uv", false)], b"");
        let error = |archive: &[u8]| extract(archive, "uv.exe").unwrap_err();

        assert_eq!(error(b"not a zip"), "not a zip archive");
        assert_eq!(error(b""), "not a zip archive");
        assert!(error(&archive[..archive.len() - 1]).contains("not a zip"));

        // The stored contents, changed: the CRC-32 no longer matches.
        let mut corrupt = archive.clone();
        let at = corrupt.windows(2).position(|w| w == b"uv").unwrap();
        let data = corrupt[at + 2..]
            .windows(2)
            .position(|w| w == b"uv")
            .unwrap();
        corrupt[at + 2 + data] = b'X';
        assert!(error(&corrupt).contains("CRC-32"), "{}", error(&corrupt));

        let encrypted = zip(
            &[Entry {
                flags: ENCRYPTED,
                ..entry("uv.exe", b"uv", false)
            }],
            b"",
        );
        assert!(error(&encrypted).contains("encrypted"));

        // A ZIP64 marker in the end record.
        let mut zip64 = archive.clone();
        let end = find_end(&zip64).unwrap();
        zip64[end + 16..end + 20].copy_from_slice(&ZIP64_U32.to_le_bytes());
        assert!(error(&zip64).contains("ZIP64"));

        let mut truncated = archive.clone();
        let end = find_end(&truncated).unwrap();
        truncated[end + 16..end + 20].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(error(&truncated).contains("truncated"));
    }
}
