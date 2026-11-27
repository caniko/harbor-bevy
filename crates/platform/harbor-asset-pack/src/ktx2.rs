use std::{
    io::{self, Read},
    ops::Range,
};

use ruzstd::decoding::StreamingDecoder;
use thiserror::Error;

const HEADER_SIZE: usize = 80;
const INDEX_STRIDE: usize = 24;
const MAX_DIMENSION: u32 = 8192;
const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENCODED_LEVEL_BYTES: u64 = MAX_DECODED_BYTES;
const MAX_ZSTD_WINDOW: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ktx2PixelFormat {
    Uastc4x4 { srgb: bool },
    Rgba8Unorm,
    Rgba8Srgb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ktx2Supercompression {
    None,
    Zstd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ktx2LevelInfo {
    pub byte_range: Range<usize>,
    pub uncompressed_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ktx2HeaderInfo {
    pub dimensions: [u32; 2],
    pub pixel_format: Ktx2PixelFormat,
    pub supercompression: Ktx2Supercompression,
    /// In logical mip order, largest first.
    pub levels: Vec<Ktx2LevelInfo>,
}

#[derive(Debug, Error)]
pub enum Ktx2ValidationError {
    #[error("invalid KTX2 header or unsupported texture shape")]
    Header,
    #[error("unsupported KTX2 format or supercompression")]
    UnsupportedFormat,
    #[error("invalid or unsupported Khronos Basic DFD v2")]
    Dfd,
    #[error("invalid KTX2 metadata range")]
    Metadata,
    #[error("invalid KTX2 level {0} range, alignment, or size")]
    Level(usize),
    #[error("overlapping KTX2 level ranges")]
    OverlappingLevels,
    #[error("trailing bytes after the final KTX2 level")]
    TrailingBytes,
    #[error("KTX2 encoded or decoded byte budget exceeded")]
    ByteBudget,
    #[error("invalid Zstd frame for KTX2 level {0}")]
    Zstd(usize),
}

impl Ktx2HeaderInfo {
    /// Validates the flat 2D profile: one face, no layers/depth, explicit mips,
    /// dimensions up to 8192, and at most 64 MiB decoded. Zstd levels must
    /// contain one dictionary-free frame with a window no larger than 8 MiB.
    /// Each encoded level is limited to 64 MiB or twice its decoded size plus
    /// 4 KiB, whichever is smaller.
    /// KVD contents and the texture bitstream itself are not interpreted.
    pub fn parse(bytes: &[u8]) -> Result<Self, Ktx2ValidationError> {
        use Ktx2ValidationError as Error;

        let header = bytes.first_chunk::<HEADER_SIZE>().ok_or(Error::Header)?;
        if &header[..12] != b"\xabKTX 20\xbb\r\n\x1a\n" {
            return Err(Error::Header);
        }
        let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().expect("header word"));
        let (vk, width, height, count) = (word(12), word(20), word(24), word(40));
        if word(16) != 1
            || word(28) != 0
            || word(32) != 0
            || word(36) != 1
            || !(1..=MAX_DIMENSION).contains(&width)
            || !(1..=MAX_DIMENSION).contains(&height)
            || !(1..=width.max(height).ilog2() + 1).contains(&count)
        {
            return Err(Error::Header);
        }
        let supercompression = match word(44) {
            0 => Ktx2Supercompression::None,
            2 => Ktx2Supercompression::Zstd,
            _ => return Err(Error::UnsupportedFormat),
        };
        let block_bytes = match vk {
            0 => 16,
            37 | 43 => 4,
            _ => return Err(Error::UnsupportedFormat),
        };
        let index_end = HEADER_SIZE + count as usize * INDEX_STRIDE;
        let index = bytes.get(HEADER_SIZE..index_end).ok_or(Error::Header)?;
        let (dfd_offset, dfd_len) = (word(48) as usize, word(52) as usize);
        if dfd_offset != index_end {
            return Err(Error::Dfd);
        }
        let dfd_end = dfd_offset.checked_add(dfd_len).ok_or(Error::Dfd)?;
        let dfd = bytes.get(dfd_offset..dfd_end).ok_or(Error::Dfd)?;
        let pixel_format = parse_dfd(dfd, vk).ok_or(Error::Dfd)?;
        let (kvd_offset, kvd_len) = (word(56) as usize, word(60) as usize);
        if header[64..80] != [0; 16] {
            return Err(Error::Metadata);
        }
        let data_start = match (kvd_offset, kvd_len) {
            (0, 0) => dfd_end,
            (offset, len) if offset == dfd_end && len != 0 && len.is_multiple_of(4) => offset
                .checked_add(len)
                .filter(|&end| end <= bytes.len())
                .ok_or(Error::Metadata)?,
            _ => return Err(Error::Metadata),
        };

        // KTX 2.0 Level Index: raw alignment is lcm(texel block size, 4);
        // supercompressed levels have byte alignment.
        let raw = supercompression == Ktx2Supercompression::None;
        let mut budget = MAX_DECODED_BYTES;
        let mut levels = Vec::with_capacity(count as usize);
        for (mip, entry) in index.as_chunks::<INDEX_STRIDE>().0.iter().enumerate() {
            let [offset, len, uncompressed_bytes] = [0, 8, 16]
                .map(|at| u64::from_le_bytes(entry[at..at + 8].try_into().expect("index word")));
            let [mut w, mut h] = [width, height].map(|v| u64::from((v >> mip).max(1)));
            if vk == 0 {
                (w, h) = (w.div_ceil(4), h.div_ceil(4));
            }
            let end = offset.checked_add(len).ok_or(Error::Level(mip))?;
            if offset < data_start as u64
                || end > bytes.len() as u64
                || len == 0
                || uncompressed_bytes != w * h * block_bytes
                || (raw && (!offset.is_multiple_of(block_bytes) || len != uncompressed_bytes))
            {
                return Err(Error::Level(mip));
            }
            budget = budget
                .checked_sub(uncompressed_bytes)
                .ok_or(Error::ByteBudget)?;
            if !raw && len > MAX_ENCODED_LEVEL_BYTES.min(uncompressed_bytes * 2 + 4096) {
                return Err(Error::ByteBudget);
            }
            levels.push(Ktx2LevelInfo {
                byte_range: offset as usize..end as usize,
                uncompressed_bytes,
            });
        }
        let alignment = if raw { block_bytes as usize } else { 1 };
        let mut cursor = data_start;
        for (mip, level) in levels.iter().enumerate().rev() {
            let range = &level.byte_range;
            if range.start < cursor {
                return Err(Error::OverlappingLevels);
            }
            if cursor.checked_next_multiple_of(alignment) != Some(range.start)
                || bytes[cursor..range.start].iter().any(|&byte| byte != 0)
            {
                return Err(Error::Level(mip));
            }
            cursor = range.end;
        }
        if cursor != bytes.len() {
            return Err(Error::TrailingBytes);
        }
        if !raw {
            for (mip, level) in levels.iter().enumerate() {
                if !valid_zstd_mip(&bytes[level.byte_range.clone()], level.uncompressed_bytes) {
                    return Err(Error::Zstd(mip));
                }
            }
        }
        Ok(Self {
            dimensions: [width, height],
            pixel_format,
            supercompression,
            levels,
        })
    }
}

fn parse_dfd(dfd: &[u8], vk: u32) -> Option<Ktx2PixelFormat> {
    let header = dfd.first_chunk::<28>()?;
    let len = if vk == 0 { 44 } else { 92 };
    if dfd.len() != len
        || header[..4] != (len as u32).to_le_bytes()
        || header[4..10] != [0, 0, 0, 0, 2, 0]
        || header[10..12] != ((len - 4) as u16).to_le_bytes()
        || !matches!(header[13], 0 | 1) // unspecified or BT.709/sRGB primaries
        || !matches!(header[14], 1 | 2) // linear or sRGB transfer
        || !matches!(header[15], 0 | 1)
    // straight or premultiplied alpha
    {
        return None;
    }
    let samples = &dfd[28..];
    let srgb = header[14] == 2;
    if vk == 0 {
        let channel = samples[3];
        // Khronos UASTC 4x4: one combined block sample, not separate alpha.
        return (header[12] == 166
            && header[16..28] == [3, 3, 0, 0, 16, 0, 0, 0, 0, 0, 0, 0]
            && matches!(channel, 0 | 3 | 4 | 5 | 6)
            && (header[15] == 0 || channel == 3)
            && valid_sample(samples, 0, 127, channel, u32::MAX))
        .then_some(Ktx2PixelFormat::Uastc4x4 { srgb });
    }
    (header[12] == 1
        && srgb == (vk == 43)
        && header[16..28] == [0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]
        && samples
            .as_chunks::<16>()
            .0
            .iter()
            .zip([0, 1, 2, if srgb { 31 } else { 15 }])
            .enumerate()
            .all(|(i, (sample, channel))| valid_sample(sample, i as u8 * 8, 7, channel, 255)))
    .then_some(if srgb {
        Ktx2PixelFormat::Rgba8Srgb
    } else {
        Ktx2PixelFormat::Rgba8Unorm
    })
}

fn valid_sample(sample: &[u8], offset: u8, bits_minus_one: u8, channel: u8, upper: u32) -> bool {
    sample[..4] == [offset, 0, bits_minus_one, channel]
        && sample[4..12] == [0; 8] // sample positions and lower bound
        && sample[12..16] == upper.to_le_bytes()
}

fn valid_zstd_mip(bytes: &[u8], expected: u64) -> bool {
    let Some(&descriptor) = bytes.get(4) else {
        return false;
    };
    // Reject a reserved descriptor bit; the decoder checks window size before
    // allocating its frame scratch space.
    if descriptor & 0x08 != 0 {
        return false;
    }
    let Ok(mut decoder) = StreamingDecoder::new_with_max_window_size(bytes, MAX_ZSTD_WINDOW) else {
        return false;
    };
    if !io::copy(&mut decoder.by_ref().take(expected + 1), &mut io::sink())
        .is_ok_and(|n| n == expected)
    {
        return false;
    }
    let (remaining, frame) = decoder.into_parts();
    remaining.is_empty() && frame.is_finished()
        // Content size is mandatory with a size flag or single-segment bit.
        && (descriptor & 0xe0 == 0 || frame.content_size() == expected)
        && frame.get_checksum_from_data()
            .is_none_or(|checksum| frame.get_calculated_checksum() == Some(checksum))
}

#[cfg(test)]
mod tests {
    use super::valid_zstd_mip;

    #[test]
    fn accepts_only_complete_single_frames_at_exact_size() {
        // Ordinary single segment containing the bytes "abc" and their
        // xxHash64 low 32 bits.
        let good = [
            0x28, 0xb5, 0x2f, 0xfd, 0x24, 3, 25, 0, 0, b'a', b'b', b'c', 0x99, 0x09, 0x77, 0xad,
        ];
        assert!(valid_zstd_mip(&good, 3));
        assert!(!valid_zstd_mip(&good, 4));
        assert!(!valid_zstd_mip(&good[..good.len() - 1], 3));
        let mut bad = good;
        bad[15] ^= 1;
        assert!(!valid_zstd_mip(&bad, 3));
        assert!(!valid_zstd_mip(&[good, good].concat(), 3));
        assert!(!valid_zstd_mip(&[good.as_slice(), &[0]].concat(), 3));
    }

    #[test]
    fn rejects_output_overrun_and_window_over_budget() {
        // Small no-content-size RLE frame: decodes to 32 bytes, budget 16.
        let rle = [0x28, 0xb5, 0x2f, 0xfd, 0, 0, 3, 1, 0, 0];
        assert!(valid_zstd_mip(&rle, 32));
        assert!(!valid_zstd_mip(&rle, 16));
        // The same ordinary frame is accepted at the cap, rejected just above it.
        let mut window = rle;
        window[5] = 13 << 3;
        assert!(valid_zstd_mip(&window, 32));
        window[5] += 1;
        assert!(!valid_zstd_mip(&window, 32));
    }
}
