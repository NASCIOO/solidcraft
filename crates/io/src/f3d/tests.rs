use super::*;
use std::io::Write;

fn string(out: &mut Vec<u8>, text: &str) {
    out.extend((text.len() as u32).to_le_bytes());
    out.extend(text.as_bytes());
}

/// Original synthetic index, not copied from a Fusion-produced file.
fn streams(layout: u32) -> (Vec<u8>, Vec<u8>) {
    let mut meta = Vec::new();
    let segment = if layout == 8 { "FusionDesignSegmentType" } else { "Design" };
    string(&mut meta, segment);
    string(&mut meta, "");
    meta.extend(0u32.to_le_bytes()); // empty UTF-16 identity
    meta.extend(1u32.to_le_bytes()); // build
    meta.extend(layout.to_le_bytes());
    if layout == 8 {
        meta.extend(30u32.to_le_bytes());
        meta.extend(0u32.to_le_bytes());
    }
    string(&mut meta, segment);
    string(&mut meta, if layout == 8 { "Fusion" } else { "" });
    for n in [1u32, 0, 1] {
        meta.extend(n.to_le_bytes());
    }
    string(&mut meta, "test-class");
    string(&mut meta, "");
    meta.extend(1u32.to_le_bytes());
    string(&mut meta, "Test");
    meta.extend(1u32.to_le_bytes());
    meta.extend(7u64.to_le_bytes());
    meta.extend(1u32.to_le_bytes()); // roots
    meta.extend(7u64.to_le_bytes());
    meta.extend(1u32.to_le_bytes()); // index
    meta.extend(7u64.to_le_bytes());
    meta.extend(0u64.to_le_bytes());
    let mut bulk = Vec::new();
    string(&mut bulk, "101");
    bulk.extend(7u64.to_le_bytes());
    bulk.extend([11, 22, 33]); // unknown payload retained exactly
    meta.extend(1u32.to_le_bytes()); // secondary index
    meta.extend(7u64.to_le_bytes());
    meta.extend((bulk.len() as u64).to_le_bytes());
    string(&mut bulk, "102");
    bulk.extend(7u64.to_le_bytes());
    bulk.extend([44, 55]);
    meta.extend(8u64.to_le_bytes()); // unknown tail
    if layout >= 5 {
        meta.extend(0u32.to_le_bytes());
    }
    if layout == 8 {
        meta.extend(1u32.to_le_bytes());
        string(&mut meta, "Application");
        meta.extend(6u32.to_le_bytes());
    }
    (meta, bulk)
}

fn archive(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in parts {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn native_indexes_keep_opaque_streams_and_secondary_records() {
    for layout in [4, 5, 8] {
        let (meta, bulk) = streams(layout);
        let bytes = archive(&[("Manifest.dat", b"synthetic"), ("asset/Design1/MetaStream.dat", &meta), ("asset/Design1/BulkStream.dat", &bulk)]);
        let designs = read_f3d(&bytes).unwrap();
        assert_eq!(designs.len(), 1);
        let d = &designs[0];
        assert_eq!(d.layout, layout);
        assert_eq!(d.path, "asset/Design1");
        assert_eq!(d.classes[0].objects, [7]);
        assert_eq!(d.roots, [7]);
        assert_eq!(d.records[0].id, 7);
        assert_eq!(d.records[0].offset, 0);
        assert_eq!(d.records[0].secondary_offset, Some(18));
        assert_eq!(d.metadata, meta);
        assert_eq!(d.bulk, bulk);
    }
    assert!(is_f3d_path("part.F3D"));
    assert!(!is_f3d_path("part.f3d.json"));
}

#[test]
fn resaved_legacy_design_header_uses_modern_descriptor() {
    let (mut meta, bulk) = streams(8);
    let mut r = Reader { bytes: &meta, at: 0 };
    r.string().unwrap();
    let end = r.at;
    let mut prefix = Vec::new();
    string(&mut prefix, "Design");
    meta.splice(..end, prefix);
    let d = index("resaved".into(), meta.clone(), bulk.clone()).unwrap();
    assert_eq!(d.layout, 8);
    assert_eq!(d.classes[0].objects, [7]);
    // An unrelated stream identity must still fail.
    meta[4..10].copy_from_slice(b"Other!");
    assert!(index("wrong identity".into(), meta, bulk).is_err());
}

#[test]
fn design_header_flag_admits_zero_and_one_only() {
    for layout in [4, 5, 8] {
        let (mut meta, bulk) = streams(layout);
        let mut r = Reader { bytes: &meta, at: 0 };
        r.string().unwrap();
        let at = r.at;
        for (flag, admitted) in [(0u32, true), (1, true), (2, false), (u32::MAX, false)] {
            meta[at..at + 4].copy_from_slice(&flag.to_le_bytes());
            assert_eq!(index("flag".into(), meta.clone(), bulk.clone()).is_ok(), admitted, "layout {layout}, flag {flag}");
        }
    }
}

#[test]
fn selection_flag_admits_zero_and_one_only() {
    for (flag, admitted) in [(0u32, true), (1, true), (2, false), (u32::MAX, false)] {
        let bytes = flag.to_le_bytes();
        let mut r = Reader { bytes: &bytes, at: 0 };
        assert_eq!(history::selection_flag(&mut r).is_ok(), admitted, "flag {flag}");
    }
    assert!(history::selection_flag(&mut Reader { bytes: &[1, 0], at: 0 }).is_err());
}

#[test]
fn truncated_and_corrupt_indexes_fail_without_panicking() {
    let (meta, bulk) = streams(5);
    for end in 0..meta.len() {
        assert!(index("test".into(), meta[..end].to_vec(), bulk.clone()).is_err(), "metadata prefix {end}");
    }
    // A record header must fit and carry the matching object identity.
    for end in 0..33 {
        assert!(index("test".into(), meta.clone(), bulk[..end].to_vec()).is_err(), "bulk prefix {end}");
    }
    let mut corrupt = bulk.clone();
    corrupt[7] = 9;
    assert!(index("test".into(), meta.clone(), corrupt).is_err());
    let mut corrupt = meta.clone();
    let offset = corrupt.len() - 12 - 8;
    corrupt[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(index("test".into(), corrupt, bulk.clone()).is_err());
    let mut corrupt = meta;
    corrupt[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(index("test".into(), corrupt, bulk).is_err());
}

#[test]
fn unsafe_packages_and_missing_streams_are_rejected() {
    let (meta, bulk) = streams(5);
    for parts in [
        vec![("Manifest.dat", &b"test"[..])],
        vec![("asset/Design1/MetaStream.dat", meta.as_slice()), ("asset/Design1/BulkStream.dat", bulk.as_slice())],
        vec![("Manifest.dat", &b"test"[..]), ("asset/Design1/MetaStream.dat", meta.as_slice())],
        vec![("Manifest.dat", &b"test"[..]), ("../outside", &b"bad"[..])],
    ] {
        assert!(read_f3d(&archive(&parts)).is_err());
    }
    assert!(read_f3d(b"not a zip").is_err());
    let (mut meta, bulk) = streams(5);
    // Unsupported layout, not best-effort decoding of a newer schema.
    meta[22..26].copy_from_slice(&99u32.to_le_bytes());
    assert!(index("test".into(), meta, bulk).is_err());
}

#[test]
fn declared_zip_expansion_is_checked_before_decompression() {
    let mut bytes = archive(&[("Manifest.dat", b"synthetic")]);
    let start = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    // Central directory uncompressed size. No large allocation or compressed
    // bomb is needed to verify the package-wide expansion budget.
    bytes[start + 24..start + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    let error = read_f3d(&bytes).unwrap_err().to_string();
    assert!(error.contains("expanded archive"), "{error}");
}

fn zstd_archive(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in parts {
        // RFC 8878 single-segment frame containing one raw block.
        let mut frame = 0xfd2fb528u32.to_le_bytes().to_vec();
        frame.push(0xa0);
        frame.extend((data.len() as u32).to_le_bytes());
        frame.extend(&((data.len() as u32 * 8) | 1).to_le_bytes()[..3]);
        frame.extend(*data);
        zip.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        zip.write_all(&frame).unwrap();
    }
    let mut bytes = zip.finish().unwrap().into_inner();
    let offsets: Vec<_> = {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        (0..parts.len())
            .map(|i| {
                let f = zip.by_index_raw(i).unwrap();
                (f.header_start() as usize, f.central_header_start() as usize)
            })
            .collect()
    };
    for ((local, central), (_, data)) in offsets.into_iter().zip(parts) {
        for (method, crc, size) in [(local + 8, local + 14, local + 22), (central + 10, central + 16, central + 24)] {
            bytes[method..method + 2].copy_from_slice(&93u16.to_le_bytes());
            bytes[crc..crc + 4].copy_from_slice(&crc32fast::hash(data).to_le_bytes());
            bytes[size..size + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
        }
    }
    bytes
}

#[test]
fn zstandard_segmented_archives_validate_checksums_and_ignore_browser_streams() {
    let (meta, bulk) = streams(8);
    let bytes = zstd_archive(&[
        ("Manifest.dat", b"synthetic"),
        ("asset/FusionDesignSegmentType1/MetaStream.dat", &meta),
        ("asset/FusionDesignSegmentType1/BulkStream.dat", &bulk),
        ("asset/FusionBrowserSegmentType1/MetaStream.dat", b"different browser schema"),
    ]);
    let designs = read_f3d(&bytes).unwrap();
    assert_eq!(designs.len(), 1);
    assert_eq!(designs[0].metadata, meta);
    assert_eq!(designs[0].bulk, bulk);
    let mut bad = bytes.clone();
    let (data, central) = {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        let f = zip.by_index_raw(2).unwrap();
        (f.data_start() as usize, f.central_header_start() as usize)
    };
    bad[central + 16] ^= 1;
    assert!(read_f3d(&bad).unwrap_err().to_string().contains("checksum"));
    // A forged single-segment content size is rejected before decoder allocation.
    let mut bad = bytes;
    bad[data + 5..data + 9].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(read_f3d(&bad).unwrap_err().to_string().contains("Zstandard window"));
}
