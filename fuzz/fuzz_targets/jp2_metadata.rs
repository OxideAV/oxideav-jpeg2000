#![no_main]

//! Fuzz target for the vendor-information boxes of a JP2 file (T.800
//! §I.7): `uuid` boxes with their Exif / XMP dispatch, `xml ` boxes and
//! `uinf` superboxes (`ulst` + `url `).
//!
//! The input is appended, as a run of top-level boxes, to a small valid
//! JP2 file (so the walk always reaches it) and the result is parsed.
//! Every retained box must describe bytes inside the file, the Exif
//! block must open with a TIFF header, `info` / `decode` must agree
//! with the parse on what metadata is present, and writing the parsed
//! vendor content back around the same codestream must parse to the
//! same content. The raw input is also handed to the parser on its own.
//!
//! ## Bounds
//!
//! Inputs are capped at 64 KiB; the base picture is 4 × 4 samples.

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::jp2::{parse_jp2, write_jp2, Jp2WriteOptions, MAX_VENDOR_BOXES};
use oxideav_jpeg2000::{decode, encode, info, EncodeOptions, Jpeg2000Image};

const MAX_INPUT_BYTES: usize = 64 * 1024;

fn base() -> &'static [u8] {
    static BASE: OnceLock<Vec<u8>> = OnceLock::new();
    BASE.get_or_init(|| {
        let img = Jpeg2000Image::from_rgb8(4, 4, (0..48).collect()).expect("image");
        encode(&img, &EncodeOptions::default()).expect("encode")
    })
}

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = parse_jp2(data);

    let mut file = base().to_vec();
    file.extend_from_slice(data);
    let Ok(c) = parse_jp2(&file) else {
        // A malformed trailing box: the contract entry points must
        // reach the same verdict without panicking.
        let _ = info(&file);
        return;
    };
    assert!(c.uuid_boxes.len() <= MAX_VENDOR_BOXES);
    assert!(c.xml_boxes.len() <= MAX_VENDOR_BOXES);
    assert!(c.uuid_info.len() <= MAX_VENDOR_BOXES);
    for b in &c.uuid_boxes {
        assert!(b.data(&file).is_some());
    }
    for b in &c.xml_boxes {
        assert!(b.data(&file).is_some());
    }
    let exif = c.exif(&file);
    let xmp = c.xmp(&file);
    if let Some(e) = exif {
        assert!(e.len() >= 8 && (e.starts_with(b"II*\0") || e.starts_with(b"MM\0*")));
    }
    let i = info(&file).expect("info of a parsed file");
    assert_eq!((i.has_exif, i.has_xmp), (exif.is_some(), xmp.is_some()));
    let img = decode(&file).expect("decode of a parsed file");
    assert_eq!(img.metadata.exif.as_deref(), exif);
    assert_eq!(img.metadata.xmp.as_deref(), xmp);

    // Write what was read around the same codestream and read it back.
    let cs = &file[c.codestream_offset..c.codestream_offset + c.codestream_len];
    let uuid_boxes: Vec<([u8; 16], Vec<u8>)> = c
        .uuid_boxes
        .iter()
        .map(|b| (b.id, b.data(&file).expect("in bounds").to_vec()))
        .collect();
    let xml_boxes: Vec<Vec<u8>> = c
        .xml_boxes
        .iter()
        .map(|b| b.data(&file).expect("in bounds").to_vec())
        .collect();
    // A URL with an interior NUL cannot come out of the reader (the
    // string ends at the first NUL), so every parsed box is writable.
    let again = write_jp2(
        cs,
        &Jp2WriteOptions::for_components(3)
            .with_uuid_boxes(uuid_boxes.clone())
            .with_xml_boxes(xml_boxes.clone())
            .with_uuid_info(c.uuid_info.clone()),
    )
    .expect("write parsed vendor boxes");
    let c2 = parse_jp2(&again).expect("parse own output");
    let uuid2: Vec<([u8; 16], Vec<u8>)> = c2
        .uuid_boxes
        .iter()
        .map(|b| (b.id, b.data(&again).expect("in bounds").to_vec()))
        .collect();
    let xml2: Vec<Vec<u8>> = c2
        .xml_boxes
        .iter()
        .map(|b| b.data(&again).expect("in bounds").to_vec())
        .collect();
    assert_eq!(uuid2, uuid_boxes);
    assert_eq!(xml2, xml_boxes);
    assert_eq!(c2.uuid_info, c.uuid_info);
    assert_eq!(c2.exif(&again), exif);
    assert_eq!(c2.xmp(&again), xmp);

    // The typed inputs: an Exif block and an XMP packet the reader
    // surfaced are accepted by the writer unless the packet is not
    // UTF-8, which is a verdict.
    let typed = write_jp2(
        cs,
        &Jp2WriteOptions::for_components(3)
            .with_exif(exif.map(<[u8]>::to_vec))
            .with_xmp(xmp.map(<[u8]>::to_vec)),
    );
    match typed {
        Ok(f) => {
            let c3 = parse_jp2(&f).expect("parse own output");
            assert_eq!(c3.exif(&f), exif);
            assert_eq!(c3.xmp(&f), xmp);
        }
        Err(_) => assert!(xmp.is_some_and(|x| std::str::from_utf8(x).is_err())),
    }
});
