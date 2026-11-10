//! Metadata in the JP2 / JPH file: Exif and XMP in `uuid` boxes
//! (T.800 §I.7.2 with the XMP Part 3 Table 8 identifier and the
//! `JpgTiffExif->JP2` identifier), XML boxes (§I.7.1) and UUID Info
//! superboxes (§I.7.3) — read from files written end to end by
//! independent producers, written back byte for byte, round-tripped
//! through the contract API and the registry path, and checked on the
//! box layouts no staged file exercises (a box after the codestream, a
//! big-endian Exif block, `XLBox`, `LBox = 0`).

use oxideav_jpeg2000::jp2::{
    Jp2WriteOptions, MAX_VENDOR_BOXES, UUID_EXIF, UUID_XMP, UuidInfo, parse_jp2, write_jp2,
};
use oxideav_jpeg2000::{
    Container, EncodeOptions, Jpeg2000Error, Jpeg2000Image, Metadata, decode, encode, info,
};

const XMP_JP2: &[u8] = include_bytes!("fixtures/jp2-uuid/xmp.jp2");
const EXIF_JP2: &[u8] = include_bytes!("fixtures/jp2-uuid/exif.jp2");
const EXIF_XMP_JP2: &[u8] = include_bytes!("fixtures/jp2-uuid/exif-xmp.jp2");
const EXPECTED_XMP: &[u8] = include_bytes!("fixtures/jp2-uuid/expected.xmp");
const EXPECTED_EXIF: &[u8] = include_bytes!("fixtures/jp2-uuid/expected.exif.tif");
const EXPECTED_PPM: &[u8] = include_bytes!("fixtures/jp2-uuid/expected.ppm");

/// The 192 sample bytes of the 8×8 binary PPM (`P6\n8 8\n255\n`).
fn expected_rgb() -> &'static [u8] {
    &EXPECTED_PPM[EXPECTED_PPM.len() - 192..]
}

/// `(file, Exif box [offset of DATA, length], XMP box [..])` — the
/// box walks staged with the files.
#[allow(clippy::type_complexity)]
const FIXTURES: &[(&str, &[u8], Option<(usize, usize)>, Option<(usize, usize)>)] = &[
    ("xmp", XMP_JP2, None, Some((101, 2597))),
    ("exif", EXIF_JP2, Some((101, 122)), None),
    (
        "exif-xmp",
        EXIF_XMP_JP2,
        Some((101, 122)),
        Some((247, 2597)),
    ),
];

fn box_bytes(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut b = ((content.len() + 8) as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(content);
    b
}

fn uuid_box(id: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut c = id.to_vec();
    c.extend_from_slice(data);
    box_bytes(b"uuid", &c)
}

/// A small lossless RGB picture and its JP2 file without metadata.
fn plain_jp2() -> (Jpeg2000Image, Vec<u8>) {
    let rgb: Vec<u8> = (0..16 * 12 * 3).map(|i| (i * 7 % 251) as u8).collect();
    let img = Jpeg2000Image::from_rgb8(16, 12, rgb).unwrap();
    let file = encode(&img, &EncodeOptions::default()).unwrap();
    (img, file)
}

/// A big-endian TIFF structure with an empty 0th IFD.
const EXIF_MM: &[u8] = b"MM\0\x2A\0\0\0\x08\0\0\0\0\0\0";

// ---- staged fixtures --------------------------------------------------------

#[test]
fn staged_fixtures_yield_their_payloads_exactly() {
    for &(name, file, exif_at, xmp_at) in FIXTURES {
        let c = parse_jp2(file).unwrap_or_else(|e| panic!("{name}: {e}"));
        // The box walk: one `uuid` box per payload, DATA where the
        // staged trace puts it.
        assert_eq!(
            c.uuid_boxes.len(),
            usize::from(exif_at.is_some()) + usize::from(xmp_at.is_some()),
            "{name}"
        );
        for b in &c.uuid_boxes {
            let at = Some((b.data_offset, b.data_len));
            if b.is_exif() {
                assert_eq!(at, exif_at, "{name}");
            } else {
                assert!(b.is_xmp(), "{name}");
                assert_eq!(at, xmp_at, "{name}");
            }
        }
        assert!(c.xml_boxes.is_empty() && c.uuid_info.is_empty(), "{name}");
        assert_eq!(c.exif(file), exif_at.map(|_| EXPECTED_EXIF), "{name}");
        assert_eq!(c.xmp(file), xmp_at.map(|_| EXPECTED_XMP), "{name}");

        let i = info(file).unwrap();
        assert_eq!(i.has_exif, exif_at.is_some(), "{name}");
        assert_eq!(i.has_xmp, xmp_at.is_some(), "{name}");
        assert!(!i.has_icc, "{name}");

        let img = decode(file).unwrap();
        assert_eq!((img.width, img.height), (8, 8), "{name}");
        assert_eq!(img.as_bytes().unwrap(), expected_rgb(), "{name}");
        assert_eq!(
            img.metadata.exif.as_deref(),
            exif_at.map(|_| EXPECTED_EXIF),
            "{name}"
        );
        assert_eq!(
            img.metadata.xmp.as_deref(),
            xmp_at.map(|_| EXPECTED_XMP),
            "{name}"
        );
        assert!(img.metadata.icc.is_none(), "{name}");
    }
}

#[test]
fn writer_reproduces_the_staged_files_byte_for_byte() {
    // Take each staged file apart (codestream + payloads) and wrap it
    // again: the writer's box layout — Signature, File Type, JP2
    // Header, Exif `uuid`, XMP `uuid`, codestream — and every header
    // byte are the independent producers'.
    for &(name, file, exif_at, xmp_at) in FIXTURES {
        let c = parse_jp2(file).unwrap();
        let cs = &file[c.codestream_offset..c.codestream_offset + c.codestream_len];
        let opts = Jp2WriteOptions::for_components(3)
            .with_exif(exif_at.map(|_| EXPECTED_EXIF.to_vec()))
            .with_xmp(xmp_at.map(|_| EXPECTED_XMP.to_vec()));
        assert_eq!(write_jp2(cs, &opts).unwrap(), file, "{name}");
    }
}

// ---- contract API round trip ------------------------------------------------

#[test]
fn contract_api_round_trips_icc_exif_and_xmp() {
    let icc = vec![0u8, 0, 0, 0x80, b'I', b'C', b'C', 0xAA, 1, 2, 3, 4];
    let (img, _) = plain_jp2();
    for ht in [false, true] {
        for (exif, xmp) in [
            (Some(EXPECTED_EXIF.to_vec()), Some(EXPECTED_XMP.to_vec())),
            (Some(EXIF_MM.to_vec()), None),
            (
                None,
                Some(b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>".to_vec()),
            ),
            (None, None),
        ] {
            let meta = Metadata::new()
                .with_icc(icc.clone())
                .with_exif(exif.clone())
                .with_xmp(xmp.clone());
            let src = img.clone().with_metadata(meta.clone());
            let opts = EncodeOptions::default().with_high_throughput(ht);
            let file = encode(&src, &opts).unwrap();
            let i = info(&file).unwrap();
            assert!(i.has_icc && i.jp2);
            assert_eq!(i.high_throughput, ht);
            assert_eq!((i.has_exif, i.has_xmp), (exif.is_some(), xmp.is_some()));
            let back = decode(&file).unwrap();
            assert_eq!(back.metadata, meta);
            assert_eq!(back.as_bytes(), img.as_bytes());
            // A second generation is the same file.
            assert_eq!(encode(&back, &opts).unwrap(), file);
        }
    }
}

#[test]
fn bare_codestream_carries_no_metadata() {
    let (img, _) = plain_jp2();
    let src = img.clone().with_metadata(
        Metadata::new()
            .with_exif(EXPECTED_EXIF.to_vec())
            .with_xmp(EXPECTED_XMP.to_vec()),
    );
    let opts = EncodeOptions::default().with_container(Container::J2k);
    let cs = encode(&src, &opts).unwrap();
    assert_eq!(cs, encode(&img, &opts).unwrap());
    let i = info(&cs).unwrap();
    assert!(!i.has_exif && !i.has_xmp);
    assert!(decode(&cs).unwrap().metadata.is_empty());
}

#[test]
fn encode_normalises_and_validates_its_metadata_inputs() {
    let (img, _) = plain_jp2();
    // The six-byte APP1-form code in front of the TIFF header is
    // dropped: the box holds the block from the byte-order mark.
    let mut prefixed = b"Exif\0\0".to_vec();
    prefixed.extend_from_slice(EXPECTED_EXIF);
    let file = encode(
        &img.clone()
            .with_metadata(Metadata::new().with_exif(prefixed)),
        &EncodeOptions::default(),
    )
    .unwrap();
    assert_eq!(
        decode(&file).unwrap().metadata.exif.as_deref(),
        Some(EXPECTED_EXIF)
    );

    for bad in [&b"not a tiff header"[..], b"II*\0", b"", b"Exif\0\0"] {
        let e = encode(
            &img.clone()
                .with_metadata(Metadata::new().with_exif(bad.to_vec())),
            &EncodeOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(e, Jpeg2000Error::InvalidData(_)), "{bad:?}: {e}");
    }
    let e = encode(
        &img.clone()
            .with_metadata(Metadata::new().with_xmp(vec![b'<', 0xFF, 0xFE, b'>'])),
        &EncodeOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(e, Jpeg2000Error::InvalidData(_)), "{e}");
}

// ---- layouts no staged file exercises --------------------------------------

#[test]
fn metadata_boxes_after_the_codestream_are_read() {
    let (img, plain) = plain_jp2();
    let mut file = plain.clone();
    file.extend(box_bytes(b"xml ", b"<?xml version=\"1.0\"?><a/>"));
    file.extend(uuid_box(&UUID_XMP, EXPECTED_XMP));
    file.extend(uuid_box(&UUID_EXIF, EXIF_MM));
    let c = parse_jp2(&file).unwrap();
    assert!(
        c.uuid_boxes
            .iter()
            .all(|b| b.data_offset > c.codestream_offset)
    );
    let back = decode(&file).unwrap();
    assert_eq!(back.metadata.xmp.as_deref(), Some(EXPECTED_XMP));
    assert_eq!(back.metadata.exif.as_deref(), Some(EXIF_MM));
    assert_eq!(back.as_bytes(), img.as_bytes());
    assert_eq!(
        c.xml_boxes[0].data(&file),
        Some(&b"<?xml version=\"1.0\"?><a/>"[..])
    );
}

#[test]
fn extended_and_open_ended_box_lengths_frame_the_payload() {
    let (_, plain) = plain_jp2();
    // XLBox: LBox = 1, the 64-bit length after TBox counts all 16
    // header bytes.
    let mut xl = plain.clone();
    xl.extend_from_slice(&1u32.to_be_bytes());
    xl.extend_from_slice(b"uuid");
    xl.extend_from_slice(&((16 + 16 + EXPECTED_XMP.len()) as u64).to_be_bytes());
    xl.extend_from_slice(&UUID_XMP);
    xl.extend_from_slice(EXPECTED_XMP);
    xl.extend(uuid_box(&UUID_EXIF, EXPECTED_EXIF));
    let m = decode(&xl).unwrap().metadata;
    assert_eq!(m.xmp.as_deref(), Some(EXPECTED_XMP));
    assert_eq!(m.exif.as_deref(), Some(EXPECTED_EXIF));

    // LBox = 0: the last box runs to the end of the file.
    let mut open = plain.clone();
    open.extend_from_slice(&0u32.to_be_bytes());
    open.extend_from_slice(b"uuid");
    open.extend_from_slice(&UUID_EXIF);
    open.extend_from_slice(EXPECTED_EXIF);
    assert_eq!(
        decode(&open).unwrap().metadata.exif.as_deref(),
        Some(EXPECTED_EXIF)
    );
}

#[test]
fn first_box_of_an_identifier_wins_and_mislabelled_payloads_are_unknown() {
    let (_, plain) = plain_jp2();
    let mut file = plain.clone();
    // An Exif identifier over bytes that are no TIFF structure is an
    // unknown box; the next one with the identifier is the Exif block.
    file.extend(uuid_box(&UUID_EXIF, b"garbage!garbage!"));
    file.extend(uuid_box(&UUID_EXIF, EXIF_MM));
    file.extend(uuid_box(&UUID_EXIF, EXPECTED_EXIF));
    file.extend(uuid_box(&UUID_XMP, b"<first/>"));
    file.extend(uuid_box(&UUID_XMP, b"<second/>"));
    // An unknown identifier, an empty DATA field, a box too short for
    // an ID: all skipped without complaint.
    file.extend(uuid_box(&[0x42; 16], b"vendor"));
    file.extend(uuid_box(&UUID_XMP, b""));
    file.extend(box_bytes(b"uuid", &[1, 2, 3]));
    let c = parse_jp2(&file).unwrap();
    assert_eq!(c.uuid_boxes.len(), 7);
    assert_eq!(c.exif(&file), Some(EXIF_MM));
    assert_eq!(c.xmp(&file), Some(&b"<first/>"[..]));

    // The APP1-form code in front of the TIFF header is tolerated.
    let mut prefixed = plain.clone();
    let mut data = b"Exif\0\0".to_vec();
    data.extend_from_slice(EXPECTED_EXIF);
    prefixed.extend(uuid_box(&UUID_EXIF, &data));
    assert_eq!(
        decode(&prefixed).unwrap().metadata.exif.as_deref(),
        Some(EXPECTED_EXIF)
    );
}

// ---- XML boxes, UUID Info, further UUID boxes ------------------------------

#[test]
fn xml_uuid_info_and_vendor_uuid_boxes_round_trip() {
    let (_, plain) = plain_jp2();
    let c = parse_jp2(&plain).unwrap();
    let cs = &plain[c.codestream_offset..c.codestream_offset + c.codestream_len];
    let xml = vec![
        b"<?xml version=\"1.0\"?><note>one</note>".to_vec(),
        b"<two/>".to_vec(),
    ];
    let vendor = vec![([0x42u8; 16], b"vendor bytes".to_vec()), ([7; 16], vec![])];
    let infos = vec![
        UuidInfo::new(vec![UUID_XMP, [0x42; 16]], "https://example.org/uuids"),
        UuidInfo::new(Vec::new(), "relative/path.html"),
    ];
    let file = write_jp2(
        cs,
        &Jp2WriteOptions::for_components(3)
            .with_exif(Some(EXPECTED_EXIF.to_vec()))
            .with_xmp(Some(EXPECTED_XMP.to_vec()))
            .with_uuid_boxes(vendor.clone())
            .with_xml_boxes(xml.clone())
            .with_uuid_info(infos.clone()),
    )
    .unwrap();
    let c = parse_jp2(&file).unwrap();
    assert_eq!(c.exif(&file), Some(EXPECTED_EXIF));
    assert_eq!(c.xmp(&file), Some(EXPECTED_XMP));
    let got_xml: Vec<Vec<u8>> = c
        .xml_boxes
        .iter()
        .map(|b| b.data(&file).unwrap().to_vec())
        .collect();
    assert_eq!(got_xml, xml);
    let got_vendor: Vec<([u8; 16], Vec<u8>)> = c
        .uuid_boxes
        .iter()
        .filter(|b| !b.is_exif() && !b.is_xmp())
        .map(|b| (b.id, b.data(&file).unwrap().to_vec()))
        .collect();
    assert_eq!(got_vendor, vendor);
    assert_eq!(c.uuid_info, infos);
    // The picture is untouched by any of it.
    assert_eq!(
        decode(&file).unwrap().as_bytes(),
        decode(&plain).unwrap().as_bytes()
    );
}

#[test]
fn malformed_uuid_info_is_ignored_not_fatal() {
    let (_, plain) = plain_jp2();
    let ulst = |nu: u16, ids: usize| {
        let mut c = nu.to_be_bytes().to_vec();
        c.extend(std::iter::repeat_n(0x11u8, ids * 16));
        box_bytes(b"ulst", &c)
    };
    let url = |vers_flag: [u8; 4], loc: &[u8]| {
        let mut c = vers_flag.to_vec();
        c.extend_from_slice(loc);
        box_bytes(b"url ", &c)
    };
    let good = [ulst(2, 2), url([0; 4], b"http://a/b\0")].concat();
    let cases: Vec<(Vec<u8>, bool)> = vec![
        (good.clone(), true),
        // No terminating NUL: the string ends with the box.
        ([ulst(1, 1), url([0; 4], b"x")].concat(), true),
        // Count larger than the IDs present.
        ([ulst(0xFFFF, 2), url([0; 4], b"x\0")].concat(), false),
        // Missing either sub-box.
        (ulst(1, 1), false),
        (url([0; 4], b"x\0"), false),
        // Non-zero VERS / FLAG, a URL that is not UTF-8.
        ([ulst(0, 0), url([1, 0, 0, 0], b"x\0")].concat(), false),
        ([ulst(0, 0), url([0; 4], &[0xFF, 0xFE, 0])].concat(), false),
        // A sub-box overrunning the superbox, a truncated list header.
        (
            [ulst(0, 0), vec![0, 0, 1, 0, b'u', b'r', b'l', b' ']].concat(),
            false,
        ),
        (
            [box_bytes(b"ulst", &[0]), url([0; 4], b"x\0")].concat(),
            false,
        ),
        (Vec::new(), false),
    ];
    for (content, kept) in cases {
        let mut file = plain.clone();
        file.extend(box_bytes(b"uinf", &content));
        let c = parse_jp2(&file).unwrap();
        assert_eq!(c.uuid_info.len(), usize::from(kept), "{content:?}");
        decode(&file).unwrap();
    }
    let mut file = plain.clone();
    file.extend(box_bytes(b"uinf", &good));
    let c = parse_jp2(&file).unwrap();
    assert_eq!(c.uuid_info[0].uuids, vec![[0x11; 16]; 2]);
    assert_eq!(c.uuid_info[0].location, "http://a/b");
}

#[test]
fn writer_limits_are_typed() {
    let (_, plain) = plain_jp2();
    let c = parse_jp2(&plain).unwrap();
    let cs = &plain[c.codestream_offset..c.codestream_offset + c.codestream_len];
    let base = Jp2WriteOptions::for_components(3);
    // NU is a 16-bit field.
    let e = write_jp2(
        cs,
        &base
            .clone()
            .with_uuid_info(vec![UuidInfo::new(vec![[0; 16]; 65_536], "u")]),
    )
    .unwrap_err();
    assert!(matches!(e, Jpeg2000Error::LimitExceeded(_)), "{e}");
    write_jp2(
        cs,
        &base
            .clone()
            .with_uuid_info(vec![UuidInfo::new(vec![[0; 16]; 65_535], "u")]),
    )
    .unwrap();
    // LOC is NUL-terminated.
    let e = write_jp2(
        cs,
        &base
            .clone()
            .with_uuid_info(vec![UuidInfo::new(Vec::new(), "a\0b")]),
    )
    .unwrap_err();
    assert!(matches!(e, Jpeg2000Error::InvalidData(_)), "{e}");
}

// ---- hostile input ----------------------------------------------------------

#[test]
fn vendor_boxes_are_capped_and_truncation_never_panics() {
    let (_, plain) = plain_jp2();
    let mut flood = plain.clone();
    for _ in 0..MAX_VENDOR_BOXES + 50 {
        flood.extend(uuid_box(&[9; 16], b""));
        flood.extend(box_bytes(b"xml ", b""));
        flood.extend(box_bytes(
            b"uinf",
            &[
                box_bytes(b"ulst", &[0, 0]),
                box_bytes(b"url ", &[0, 0, 0, 0, 0]),
            ]
            .concat(),
        ));
    }
    // The boxes past the cap are skipped; a payload behind them is
    // simply not seen.
    flood.extend(uuid_box(&UUID_XMP, EXPECTED_XMP));
    let c = parse_jp2(&flood).unwrap();
    assert_eq!(c.uuid_boxes.len(), MAX_VENDOR_BOXES);
    assert_eq!(c.xml_boxes.len(), MAX_VENDOR_BOXES);
    assert_eq!(c.uuid_info.len(), MAX_VENDOR_BOXES);
    assert!(c.xmp(&flood).is_none());

    // Every prefix of a metadata-bearing file, and every single-byte
    // corruption of its box headers, is a verdict.
    for cut in 0..EXIF_XMP_JP2.len() {
        let _ = parse_jp2(&EXIF_XMP_JP2[..cut]);
        let _ = info(&EXIF_XMP_JP2[..cut]);
    }
    for at in 0..260 {
        for v in [0u8, 1, 7, 0x7F, 0xFF] {
            let mut f = EXIF_XMP_JP2.to_vec();
            f[at] = v;
            let _ = decode(&f);
        }
    }
    // A stale box description never reads outside a shorter slice.
    let c = parse_jp2(EXIF_XMP_JP2).unwrap();
    assert!(c.xmp(&EXIF_XMP_JP2[..300]).is_none());
    assert!(c.exif(&EXIF_XMP_JP2[..100]).is_none());
}

// ---- black-box readers -------------------------------------------------------

fn tool(name: &str) -> bool {
    std::process::Command::new(name).arg("-h").output().is_ok()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("oxideav-jpeg2000-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A file of ours carrying Exif + XMP + XML + UUID Info, for the
/// black-box readers.
fn rich_file(ht: bool) -> (Jpeg2000Image, Vec<u8>) {
    let (img, _) = plain_jp2();
    let cs = encode(
        &img,
        &EncodeOptions::default()
            .with_container(Container::J2k)
            .with_high_throughput(ht),
    )
    .unwrap();
    let file = write_jp2(
        &cs,
        &Jp2WriteOptions::for_components(3)
            .with_exif(Some(EXPECTED_EXIF.to_vec()))
            .with_xmp(Some(EXPECTED_XMP.to_vec()))
            .with_xml_boxes(vec![b"<?xml version=\"1.0\"?><a/>".to_vec()])
            .with_uuid_info(vec![UuidInfo::new(vec![UUID_XMP], "https://example.org/")]),
    )
    .unwrap();
    (img, file)
}

#[test]
fn a_black_box_decoder_reads_the_picture_past_our_metadata_boxes() {
    if !tool("opj_decompress") {
        eprintln!("opj_decompress not installed: skipped");
        return;
    }
    let (img, file) = rich_file(false);
    let (src, out) = (scratch("rich.jp2"), scratch("rich.ppm"));
    std::fs::write(&src, &file).unwrap();
    let run = std::process::Command::new("opj_decompress")
        .arg("-i")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ppm = std::fs::read(&out).unwrap();
    let rgb = img.as_bytes().unwrap();
    assert_eq!(&ppm[ppm.len() - rgb.len()..], rgb);
}

#[test]
fn a_black_box_metadata_reader_sees_what_we_write() {
    if !tool("exiftool") {
        eprintln!("exiftool not installed: skipped");
        return;
    }
    let (_, file) = rich_file(false);
    let src = scratch("rich-meta.jp2");
    std::fs::write(&src, &file).unwrap();
    let read = |args: &[&str]| {
        let run = std::process::Command::new("exiftool")
            .args(args)
            .arg(&src)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        run.stdout
    };
    assert_eq!(read(&["-b", "-XMP"]), EXPECTED_XMP);
    assert_eq!(
        String::from_utf8_lossy(&read(&["-s3", "-Make"])).trim(),
        "OxideAV"
    );
    assert_eq!(
        String::from_utf8_lossy(&read(&["-s3", "-DateTimeOriginal"])).trim(),
        "2026:01:02 03:04:05"
    );
}

// ---- registry path ----------------------------------------------------------

#[cfg(feature = "registry")]
mod registry {
    use super::*;
    use oxideav_core::{
        BlobKind, CodecId, CodecOptions, CodecParameters, Frame, MetadataBlob, NullCodecResolver,
        Packet, ReadSeek, StreamInfo, TimeBase, VideoFrame, WriteSeek,
    };
    use oxideav_jpeg2000::container::{open_demuxer, open_muxer_j2k, open_muxer_jp2};
    use oxideav_jpeg2000::registry::{BLOB_KIND_UUID, BLOB_KIND_XML};
    use oxideav_jpeg2000::{CODEC_ID_STR, make_decoder, make_encoder};
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Cursor<Vec<u8>>>>);

    impl std::io::Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl std::io::Seek for Sink {
        fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
            self.0.lock().unwrap().seek(pos)
        }
    }

    fn reader(bytes: &[u8]) -> Box<dyn ReadSeek> {
        Box::new(Cursor::new(bytes.to_vec()))
    }

    fn demux(bytes: &[u8]) -> (StreamInfo, Packet) {
        let mut d = open_demuxer(reader(bytes), &NullCodecResolver).unwrap();
        // Opaque payloads are blobs, not text metadata.
        assert!(d.metadata().is_empty());
        (d.streams()[0].clone(), d.next_packet().unwrap())
    }

    fn decode_packet(params: &CodecParameters, pkt: &Packet) -> (CodecParameters, VideoFrame) {
        let mut dec = make_decoder(params).unwrap();
        dec.send_packet(pkt).unwrap();
        let Frame::Video(v) = dec.receive_frame().unwrap() else {
            panic!("video frame expected");
        };
        (params.clone(), v)
    }

    #[allow(clippy::type_complexity)]
    fn mux(
        open: fn(
            Box<dyn WriteSeek>,
            &[StreamInfo],
        ) -> oxideav_core::Result<Box<dyn oxideav_core::Muxer>>,
        stream: &StreamInfo,
        pkt: &Packet,
    ) -> oxideav_core::Result<Vec<u8>> {
        let sink = Sink::default();
        let mut m = open(Box::new(sink.clone()), std::slice::from_ref(stream))?;
        m.write_header()?;
        m.write_packet(pkt)?;
        m.write_trailer()?;
        Ok(sink.0.lock().unwrap().get_ref().clone())
    }

    fn data<'a>(blobs: &'a [MetadataBlob], kind: &BlobKind) -> Option<&'a [u8]> {
        blobs.iter().find(|b| b.is(kind)).map(|b| b.data.as_slice())
    }

    #[test]
    fn staged_fixtures_surface_as_stream_and_frame_blobs() {
        for &(name, file, exif_at, xmp_at) in FIXTURES {
            let (stream, pkt) = demux(file);
            let blobs = stream.params.blobs();
            assert_eq!(
                data(blobs, &BlobKind::EXIF),
                exif_at.map(|_| EXPECTED_EXIF),
                "{name}"
            );
            assert_eq!(
                data(blobs, &BlobKind::XMP),
                xmp_at.map(|_| EXPECTED_XMP),
                "{name}"
            );
            assert!(data(blobs, &BlobKind::ICC).is_none(), "{name}");
            assert_eq!(
                blobs.len(),
                usize::from(exif_at.is_some()) + usize::from(xmp_at.is_some()),
                "{name}"
            );
            let (_, frame) = decode_packet(&stream.params, &pkt);
            assert_eq!(frame.blobs(), blobs, "{name}");
            assert_eq!(frame.image_planes()[0].data, expected_rgb(), "{name}");
        }
    }

    #[test]
    fn every_vendor_box_becomes_a_blob_and_remuxes() {
        let icc = vec![0u8, 0, 0, 0x80, b'I', b'C', b'C', 0xAA, 1, 2, 3, 4];
        let (img, plain) = plain_jp2();
        let c = parse_jp2(&plain).unwrap();
        let cs = &plain[c.codestream_offset..c.codestream_offset + c.codestream_len];
        let mut opts = oxideav_jpeg2000::jp2::Jp2WriteOptions::for_components(3)
            .with_exif(Some(EXPECTED_EXIF.to_vec()))
            .with_xmp(Some(EXPECTED_XMP.to_vec()))
            .with_uuid_boxes(vec![
                ([0x42; 16], b"vendor".to_vec()),
                (UUID_XMP, b"<again/>".to_vec()),
            ])
            .with_xml_boxes(vec![b"<a/>".to_vec(), b"<b/>".to_vec()]);
        opts.colour[0] = oxideav_jpeg2000::jp2::Colr {
            method: oxideav_jpeg2000::jp2::ColrMethod::RestrictedIccProfile,
            precedence: 0,
            approximation: 0,
            enumerated: None,
            icc_profile: Some(icc.clone()),
            parameterized: None,
        };
        let file = write_jp2(cs, &opts).unwrap();

        let (stream, pkt) = demux(&file);
        let blobs = stream.params.blobs();
        let kinds: Vec<&str> = blobs.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "icc",
                "exif",
                "xmp",
                BLOB_KIND_UUID,
                BLOB_KIND_UUID,
                BLOB_KIND_XML,
                BLOB_KIND_XML
            ]
        );
        assert!(blobs.iter().all(|b| b.kind.follows_naming_rule()));
        assert_eq!(data(blobs, &BlobKind::ICC), Some(&icc[..]));
        assert_eq!(blobs[3].data, [&[0x42u8; 16][..], b"vendor"].concat());
        assert_eq!(blobs[4].data, [&UUID_XMP[..], b"<again/>"].concat());
        assert_eq!(blobs[5].data, b"<a/>");

        // A JP2 packet passes through the jp2 muxer untouched; the
        // bare-codestream muxer drops the wrapper and its metadata.
        assert_eq!(mux(open_muxer_jp2, &stream, &pkt).unwrap(), file);
        assert_eq!(mux(open_muxer_j2k, &stream, &pkt).unwrap(), cs);

        // The stream's blobs around a bare codestream give the same
        // file back: the jp2 muxer rebuilds every box from them.
        let bare = Packet::new(0, TimeBase::new(1, 1), cs.to_vec());
        assert_eq!(mux(open_muxer_jp2, &stream, &bare).unwrap(), file);

        // Decoder: the well-known three ride the frame.
        let (_, frame) = decode_packet(&stream.params, &pkt);
        assert_eq!(frame.blobs(), &blobs[..3]);
        assert_eq!(
            Some(frame.image_planes()[0].data.as_slice()),
            img.as_bytes()
        );

        // A malformed custom blob is refused with a verdict.
        let mut bad = stream.clone();
        bad.params.blobs = vec![MetadataBlob::new(BLOB_KIND_UUID, vec![1, 2, 3])];
        assert!(mux(open_muxer_jp2, &bad, &bare).is_err());
    }

    #[test]
    fn encoder_embeds_frame_and_stream_blobs() {
        let (img, _) = plain_jp2();
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(img.width);
        params.height = Some(img.height);
        params.pixel_format = Some(img.format.into());
        params.options = CodecOptions::new().set("container", "jp2");
        // Stream level: an XMP packet and an Exif block; the frame
        // refines the Exif block.
        params.push_blob(MetadataBlob::new(BlobKind::XMP, EXPECTED_XMP.to_vec()));
        params.push_blob(MetadataBlob::new(BlobKind::EXIF, EXPECTED_EXIF.to_vec()));
        let frame = VideoFrame::from(img.clone())
            .with_blob(MetadataBlob::new(BlobKind::EXIF, EXIF_MM.to_vec()));
        let mut enc = make_encoder(&params).unwrap();
        enc.send_frame(&Frame::Video(frame.clone())).unwrap();
        let pkt = enc.receive_packet().unwrap();
        let back = decode(&pkt.data).unwrap();
        assert_eq!(back.metadata.exif.as_deref(), Some(EXIF_MM));
        assert_eq!(back.metadata.xmp.as_deref(), Some(EXPECTED_XMP));
        assert_eq!(back.as_bytes(), img.as_bytes());
        // What was embedded is what the encoder reports downstream.
        let out = enc.output_params();
        assert_eq!(data(out.blobs(), &BlobKind::EXIF), Some(EXIF_MM));
        assert_eq!(data(out.blobs(), &BlobKind::XMP), Some(EXPECTED_XMP));

        // The default bare-codestream output leaves the embedding to
        // the jp2 muxer, which takes it from the output parameters.
        params.options = CodecOptions::new();
        let mut enc = make_encoder(&params).unwrap();
        enc.send_frame(&Frame::Video(frame)).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(decode(&pkt.data).unwrap().metadata.is_empty());
        let stream = StreamInfo {
            index: 0,
            params: enc.output_params().clone(),
            time_base: TimeBase::new(1, 1),
            start_time: Some(0),
            duration: None,
        };
        let file = mux(open_muxer_jp2, &stream, &pkt).unwrap();
        let back = decode(&file).unwrap();
        assert_eq!(back.metadata.exif.as_deref(), Some(EXIF_MM));
        assert_eq!(back.metadata.xmp.as_deref(), Some(EXPECTED_XMP));
        assert_eq!(back.as_bytes(), img.as_bytes());

        // Frame ↔ image bridge keeps the three blobs.
        let meta = Metadata::new()
            .with_icc(vec![1, 2, 3])
            .with_exif(EXIF_MM.to_vec())
            .with_xmp(b"<x/>".to_vec());
        let vf = VideoFrame::from(img.clone().with_metadata(meta.clone()));
        let mut p = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        p.width = Some(img.width);
        p.height = Some(img.height);
        p.pixel_format = Some(img.format.into());
        assert_eq!(
            Jpeg2000Image::from_video_frame(&vf, &p).unwrap().metadata,
            meta
        );
    }
}
