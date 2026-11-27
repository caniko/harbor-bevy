use super::*;
use sha2::{Digest, Sha256};

const PAYLOAD: &[u8] = b"asset contents";
const NAME: &str = "page.bin";

fn row(name: &str, bytes: &[u8]) -> String {
    let hash: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{hash}  {name}\n")
}

fn policy(sums: &str) -> AssetPackPolicy {
    AssetPackPolicy {
        sha256sums_digest: Sha256::digest(sums.as_bytes()).into(),
        max_file_bytes: 256,
        max_total_bytes: 512,
    }
}

#[test]
fn embedded_pack_binds_manifest_and_every_member() {
    let sums = row(NAME, PAYLOAD);
    let p = policy(&sums);
    let pack = load_and_verify_embedded(sums.as_bytes(), &[(NAME, PAYLOAD)], &p).unwrap();
    assert_eq!(pack.get(NAME), Some(PAYLOAD));
    assert_eq!(pack.manifest_digest(), p.sha256sums_digest);
    assert!(matches!(
        load_and_verify_embedded(sums.as_bytes(), &[(NAME, b"changed")], &p),
        Err(AssetPackError::DigestMismatch(_))
    ));
    let rewritten = row(NAME, b"changed");
    assert!(matches!(
        load_and_verify_embedded(rewritten.as_bytes(), &[(NAME, b"changed")], &p),
        Err(AssetPackError::DigestMismatch(_))
    ));
}

#[test]
fn pack_rejects_malformed_or_noncanonical_index() {
    let good = row(NAME, PAYLOAD);
    for bad in [
        row("../page.bin", PAYLOAD),
        row("a..b", PAYLOAD),
        row("a\\b", PAYLOAD),
        row(".", PAYLOAD),
        row("", PAYLOAD),
        row(" x", PAYLOAD),
        row("x\0", PAYLOAD),
        row("x\t", PAYLOAD),
        row("SHA256SUMS", PAYLOAD),
        good.repeat(2),
        format!("{good}{}", row("a.bin", PAYLOAD)),
        good.trim_end().to_owned(),
        good.replace('\n', "\r\n"),
        good.replace("  ", " "),
        good.to_uppercase(),
        format!("{good}\n"),
    ] {
        assert!(matches!(
            load_and_verify_embedded(bad.as_bytes(), &[(NAME, PAYLOAD)], &policy(&bad)),
            Err(AssetPackError::InvalidManifest { .. })
        ));
    }
}

#[test]
fn embedded_members_are_exact_and_bounded() {
    let sums = row(NAME, PAYLOAD);
    let p = policy(&sums);
    for members in [
        vec![],
        vec![(NAME, PAYLOAD), ("extra.bin", PAYLOAD)],
        vec![(NAME, PAYLOAD), (NAME, PAYLOAD)],
    ] {
        assert!(load_and_verify_embedded(sums.as_bytes(), &members, &p).is_err());
    }
    let p = AssetPackPolicy {
        max_total_bytes: sums.len() + PAYLOAD.len() - 1,
        ..p
    };
    assert!(matches!(
        load_and_verify_embedded(sums.as_bytes(), &[(NAME, PAYLOAD)], &p),
        Err(AssetPackError::ByteBudget(_))
    ));
}

#[test]
fn oversized_embedded_name_has_bounded_diagnostic() {
    let sums = row(NAME, PAYLOAD);
    let name = "é".repeat(5_000);
    let err =
        load_and_verify_embedded(sums.as_bytes(), &[(&name, PAYLOAD)], &policy(&sums)).unwrap_err();
    assert!(matches!(
        err,
        AssetPackError::UnexpectedMember(ref shown)
            if shown.len() <= 128 && shown.ends_with("...")
    ));
    assert!(err.to_string().len() < 180);
}

#[cfg(unix)]
#[test]
fn directory_rejects_extras_nonregular_files_and_oversize() {
    use std::{fs, os::unix::fs::symlink};

    let dir = tempfile::tempdir().unwrap();
    let sums = row(NAME, PAYLOAD);
    let p = policy(&sums);
    let asset = dir.path().join(NAME);
    let extra = dir.path().join("extra");
    fs::write(dir.path().join("SHA256SUMS"), sums.as_bytes()).unwrap();
    fs::write(&asset, PAYLOAD).unwrap();
    assert_eq!(
        load_and_verify_dir(dir.path(), &p).unwrap().get(NAME),
        Some(PAYLOAD)
    );
    fs::write(&asset, b"changed").unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &p),
        Err(AssetPackError::DigestMismatch(_))
    ));
    fs::write(&asset, PAYLOAD).unwrap();
    fs::write(&extra, b"extra").unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &p),
        Err(AssetPackError::UnexpectedMember(_))
    ));
    fs::remove_file(&extra).unwrap();
    fs::create_dir(&extra).unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &p),
        Err(AssetPackError::UnexpectedMember(_))
    ));
    fs::remove_dir(&extra).unwrap();
    fs::write(&asset, [0; 257]).unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &p),
        Err(AssetPackError::ByteBudget(_))
    ));
    fs::remove_file(&asset).unwrap();
    symlink(dir.path().join("SHA256SUMS"), &asset).unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &p),
        Err(AssetPackError::NonRegular(_))
    ));
    let container = tempfile::tempdir().unwrap();
    let alias = container.path().join("alias");
    symlink(dir.path(), &alias).unwrap();
    assert!(matches!(
        load_and_verify_dir(&alias.join(""), &p),
        Err(AssetPackError::NonRegular(_))
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn directory_rejects_fifo_with_no_writer() {
    use rustix::fs::{CWD, Mode, mkfifoat};

    let dir = tempfile::tempdir().unwrap();
    let sums = row(NAME, PAYLOAD);
    std::fs::write(dir.path().join("SHA256SUMS"), &sums).unwrap();
    mkfifoat(CWD, dir.path().join(NAME), Mode::RUSR).unwrap();
    assert!(matches!(
        load_and_verify_dir(dir.path(), &policy(&sums)),
        Err(AssetPackError::NonRegular(name)) if name == NAME
    ));
}

fn raw_frame(data: &[u8]) -> Vec<u8> {
    let mut frame = vec![
        0x28,
        0xb5,
        0x2f,
        0xfd,
        0x20,
        u8::try_from(data.len()).unwrap(),
    ];
    frame.extend_from_slice(&((u32::try_from(data.len()).unwrap() << 3) | 1).to_le_bytes()[..3]);
    frame.extend_from_slice(data);
    frame
}

fn texture(format: u32, compression: u32) -> Vec<u8> {
    let block_bytes = if format == 0 { 16 } else { 4 };
    let levels = [if format == 0 { 16 } else { 64 }, 16];
    let channels: &[u8] = match format {
        0 => &[3],
        43 => &[0, 1, 2, 31],
        _ => &[0, 1, 2, 15],
    };
    let dfd_offset = 80 + levels.len() * 24;
    let dfd_length = 28 + channels.len() * 16;
    let mut bytes = vec![0; dfd_offset + dfd_length];
    bytes[..12].copy_from_slice(b"\xabKTX 20\xbb\r\n\x1a\n");
    for (i, field) in [
        format,
        1,
        4,
        4,
        0,
        0,
        1,
        2,
        compression,
        dfd_offset as u32,
        dfd_length as u32,
    ]
    .into_iter()
    .enumerate()
    {
        bytes[12 + i * 4..16 + i * 4].copy_from_slice(&field.to_le_bytes());
    }
    let dfd = &mut bytes[dfd_offset..];
    dfd[..4].copy_from_slice(&(dfd_length as u32).to_le_bytes());
    dfd[8] = 2;
    dfd[10] = (dfd_length - 4) as u8;
    dfd[12] = if format == 0 { 166 } else { 1 };
    dfd[13] = 1;
    dfd[14] = if format == 37 { 1 } else { 2 };
    if format == 0 {
        dfd[16..18].fill(3);
    }
    dfd[20] = block_bytes as u8;
    for (i, &channel) in channels.iter().enumerate() {
        let sample = &mut dfd[28 + i * 16..44 + i * 16];
        sample[0] = i as u8 * 8;
        sample[2] = if format == 0 { 127 } else { 7 };
        sample[3] = channel;
        sample[12..16].copy_from_slice(&(if format == 0 { u32::MAX } else { 255 }).to_le_bytes());
    }
    for (i, size) in levels.into_iter().enumerate().rev() {
        if compression == 0 {
            bytes.resize(bytes.len().next_multiple_of(block_bytes), 0);
        }
        let pixels = vec![0; size];
        let mip = if compression == 0 {
            pixels
        } else {
            raw_frame(&pixels)
        };
        let offset = bytes.len();
        for (j, value) in [offset, mip.len(), size].into_iter().enumerate() {
            let index = 80 + i * 24 + j * 8;
            bytes[index..index + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        bytes.extend(mip);
    }
    bytes
}

#[test]
fn accepts_supported_formats_in_raw_and_zstd() {
    for (vk, expected) in [
        (0, Ktx2PixelFormat::Uastc4x4 { srgb: true }),
        (37, Ktx2PixelFormat::Rgba8Unorm),
        (43, Ktx2PixelFormat::Rgba8Srgb),
    ] {
        for compression in [0, 2] {
            let bytes = texture(vk, compression);
            let info = Ktx2HeaderInfo::parse(&bytes).unwrap();
            assert_eq!(info.pixel_format, expected);
            assert_eq!(info.dimensions, [4, 4]);
            assert_eq!(info.levels.len(), 2);
        }
    }
}

#[test]
fn rejects_bad_dfd_and_level_index() {
    let original = texture(43, 0);
    let dfd = 128;
    for i in [
        dfd,
        dfd + 4,
        dfd + 8,
        dfd + 10,
        dfd + 12,
        dfd + 13,
        dfd + 14,
        dfd + 16,
        dfd + 20,
        dfd + 24,
        dfd + 30,
        dfd + 31,
        dfd + 32,
        dfd + 36,
        dfd + 40,
        dfd + 79,
    ] {
        let mut bad = original.clone();
        bad[i] ^= 0xff;
        assert!(matches!(
            Ktx2HeaderInfo::parse(&bad),
            Err(Ktx2ValidationError::Dfd)
        ));
    }
    let mut overlap = original.clone();
    let second = overlap[104..112].to_vec();
    overlap[80..88].copy_from_slice(&second);
    assert!(matches!(
        Ktx2HeaderInfo::parse(&overlap),
        Err(Ktx2ValidationError::OverlappingLevels)
    ));
    for (index, value) in [(80, u64::MAX), (80, 225), (88, 63), (96, 63)] {
        let mut bad = original.clone();
        bad[index..index + 8].copy_from_slice(&value.to_le_bytes());
        assert!(matches!(
            Ktx2HeaderInfo::parse(&bad),
            Err(Ktx2ValidationError::Level(_))
        ));
    }
    for cut in [0, 11, 79, 100, original.len() - 1] {
        assert!(Ktx2HeaderInfo::parse(&original[..cut]).is_err());
    }
}

#[test]
fn rejects_mips_stored_largest_first() {
    // Both UASTC levels occupy one block, so swapping offsets preserves sizes.
    let mut bytes = texture(0, 0);
    let largest_offset = *bytes[80..].first_chunk::<8>().unwrap();
    bytes.copy_within(104..112, 80);
    bytes[104..112].copy_from_slice(&largest_offset);
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::Level(1))
    ));
}

#[test]
fn rejects_excess_mip_gaps_and_nonzero_alignment_padding() {
    let mut bytes = texture(0, 0);
    bytes[172] = 1; // First byte after the DFD, before the first 16-byte boundary.
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::Level(1))
    ));

    let mut bytes = texture(0, 0);
    let start = bytes.len() - 16;
    bytes.splice(start..start, [0; 16]);
    bytes[80..88].copy_from_slice(&((start + 16) as u64).to_le_bytes());
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::Level(0))
    ));

    let mut bytes = texture(0, 2);
    let info = Ktx2HeaderInfo::parse(&bytes).unwrap();
    let start = info.levels[0].byte_range.start;
    bytes.insert(start, 0);
    bytes[80..88].copy_from_slice(&((start + 1) as u64).to_le_bytes());
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::Level(0))
    ));
}

#[test]
fn rejects_trailing_bytes_after_last_mip() {
    for compression in [0, 2] {
        let mut bytes = texture(37, compression);
        bytes.push(0);
        assert!(matches!(
            Ktx2HeaderInfo::parse(&bytes),
            Err(Ktx2ValidationError::TrailingBytes)
        ));
    }
}

#[test]
fn rejects_oversized_encoded_mip_before_decoding() {
    let mut bytes = texture(37, 2);
    let info = Ktx2HeaderInfo::parse(&bytes).unwrap();
    let start = info.levels[0].byte_range.start;
    let encoded = 64 * 2 + 4096;
    // Not a Zstd frame: at the cap it reaches frame validation, above it does not.
    bytes.truncate(start);
    bytes.resize(start + encoded, 0);
    bytes[88..96].copy_from_slice(&(encoded as u64).to_le_bytes());
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::Zstd(0))
    ));
    bytes.push(0);
    bytes[88..96].copy_from_slice(&((encoded + 1) as u64).to_le_bytes());
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bytes),
        Err(Ktx2ValidationError::ByteBudget)
    ));
}

#[test]
fn rejects_invalid_zstd_mips_even_after_first_mip() {
    let original = texture(43, 2);
    let info = Ktx2HeaderInfo::parse(&original).unwrap();
    let range = info.levels[1].byte_range.clone();
    let mut bad = original;
    bad[range.start] = 0;
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bad),
        Err(Ktx2ValidationError::Zstd(1))
    ));
}

#[test]
fn rejects_unsupported_shape_metadata_and_decoded_budget() {
    let original = texture(37, 2);
    for (offset, value) in [
        (16, 2u32),
        (20, 0),
        (24, 8193),
        (28, 1),
        (32, 1),
        (36, 6),
        (40, 0),
        (40, 4),
    ] {
        let mut bad = original.clone();
        bad[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(matches!(
            Ktx2HeaderInfo::parse(&bad),
            Err(Ktx2ValidationError::Header)
        ));
    }
    let mut bad = original.clone();
    bad[64] = 1;
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bad),
        Err(Ktx2ValidationError::Metadata)
    ));
    let mut bad = original;
    bad[20..24].copy_from_slice(&8192u32.to_le_bytes());
    bad[24..28].copy_from_slice(&8192u32.to_le_bytes());
    bad[96..104].copy_from_slice(&(8192u64 * 8192 * 4).to_le_bytes());
    assert!(matches!(
        Ktx2HeaderInfo::parse(&bad),
        Err(Ktx2ValidationError::ByteBudget)
    ));
}
