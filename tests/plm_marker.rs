//! T.800 §A.7.2 `PLM` (packet lengths, main header): the encoder emits
//! the per-tile-part `(Nplm, Iplm…)` series, the decoder cross-checks
//! it against the packets it walks, and any disagreement is rejected.

use oxideav_jpeg2000::encode::{PackedHeaders, TilePartSplit};
use oxideav_jpeg2000::{
    Container, EncodeOptions, Error, Jpeg2000Image, PixelFormat, ProgressionOrder, decode, encode,
};

const SOT: u16 = 0xFF90;
const PLM: u16 = 0xFF57;

fn image(width: u32, height: u32, format: PixelFormat) -> Jpeg2000Image {
    let mut state = 0x1234_5678u32;
    let data = (0..width * height * format.components() as u32)
        .map(|k| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((k % 97) * 2 + (state >> 28)) as u8
        })
        .collect();
    Jpeg2000Image::packed(width, height, format, data).unwrap()
}

fn base() -> EncodeOptions {
    EncodeOptions::new().with_container(Container::J2k)
}

/// `(offset of the marker, payload range)` of every main-header
/// segment carrying `marker`.
fn main_header_segments(stream: &[u8], marker: u16) -> Vec<(usize, std::ops::Range<usize>)> {
    let mut found = Vec::new();
    let mut pos = 2;
    while pos + 4 <= stream.len() {
        let code = u16::from_be_bytes([stream[pos], stream[pos + 1]]);
        if code == SOT {
            break;
        }
        let len = usize::from(u16::from_be_bytes([stream[pos + 2], stream[pos + 3]]));
        if code == marker {
            found.push((pos, pos + 4..pos + 2 + len));
        }
        pos += 2 + len;
    }
    found
}

/// The concatenated `(Nplm, Iplm…)` series of a stream, per tile-part.
fn plm_entries(stream: &[u8]) -> Vec<Vec<u64>> {
    let mut series = Vec::new();
    for (index, (_, payload)) in main_header_segments(stream, PLM).into_iter().enumerate() {
        assert_eq!(usize::from(stream[payload.start]), index, "Zplm counts up");
        series.extend_from_slice(&stream[payload.start + 1..payload.end]);
    }
    let mut entries = Vec::new();
    let mut at = 0;
    while at < series.len() {
        let run = usize::from(series[at]);
        at += 1;
        let mut lengths = Vec::new();
        let mut value = 0u64;
        for &byte in &series[at..at + run] {
            value = (value << 7) | u64::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                lengths.push(value);
                value = 0;
            }
        }
        at += run;
        entries.push(lengths);
    }
    entries
}

#[test]
fn plm_round_trips_across_layouts() {
    let rgb = image(96, 80, PixelFormat::Rgb24);
    let mut resolution_parts = base().with_tile_size((48, 48));
    resolution_parts.tile_parts = TilePartSplit::ByResolution;
    let mut ppm = base().with_tile_size((64, 40));
    ppm.packed_headers = PackedHeaders::Ppm;
    let mut ppt = base().with_layers(2);
    ppt.packed_headers = PackedHeaders::Ppt;
    let variants = [
        ("single tile", base()),
        ("tiles", base().with_tile_size((40, 32))),
        ("tile-parts by resolution", resolution_parts),
        (
            "layers + RPCL",
            base()
                .with_layers(3)
                .with_progression(ProgressionOrder::Rpcl),
        ),
        (
            "all pointer markers + SOP/EPH",
            base()
                .with_tile_size((48, 80))
                .with_plt(true)
                .with_tlm(true)
                .with_sop(true)
                .with_eph(true),
        ),
        ("PPM", ppm),
        ("PPT", ppt),
        ("lossy", base().with_lossy(3)),
        ("HT", base().with_high_throughput(true)),
    ];
    for (name, options) in variants {
        let plain = encode(&rgb, &options).unwrap();
        let with_plm = encode(&rgb, &options.clone().with_plm(true)).unwrap();
        assert!(main_header_segments(&plain, PLM).is_empty(), "{name}");
        assert!(!main_header_segments(&with_plm, PLM).is_empty(), "{name}");
        // The marker only adds main-header bytes; the image is the same.
        assert_eq!(
            decode(&with_plm).unwrap().planes,
            decode(&plain).unwrap().planes,
            "{name}"
        );
        // One entry per tile-part, and every listed length is real:
        // the tile-part bodies add up to the listed packet lengths
        // (with relocated headers the lengths leave the headers out,
        // and so do the bodies).
        let entries = plm_entries(&with_plm);
        let mut bodies = Vec::new();
        let mut pos = 2;
        while pos + 4 <= with_plm.len() {
            let code = u16::from_be_bytes([with_plm[pos], with_plm[pos + 1]]);
            let len = usize::from(u16::from_be_bytes([with_plm[pos + 2], with_plm[pos + 3]]));
            if code == SOT {
                let psot = u32::from_be_bytes(with_plm[pos + 6..pos + 10].try_into().unwrap());
                // Skip the tile-part header up to SOD.
                let mut inner = pos + 2 + len;
                while with_plm[inner..inner + 2] != [0xFF, 0x93] {
                    let l = usize::from(u16::from_be_bytes([
                        with_plm[inner + 2],
                        with_plm[inner + 3],
                    ]));
                    inner += 2 + l;
                }
                bodies.push((pos + psot as usize - (inner + 2)) as u64);
                pos += psot as usize;
            } else {
                pos += 2 + len;
            }
        }
        assert_eq!(
            entries.len(),
            bodies.len(),
            "{name}: one entry per tile-part"
        );
        for (lengths, body) in entries.iter().zip(&bodies) {
            assert_eq!(lengths.iter().sum::<u64>(), *body, "{name}");
        }
    }
}

#[test]
fn a_wrong_plm_length_is_rejected() {
    let stream = encode(
        &image(64, 64, PixelFormat::Gray8),
        &base().with_tile_size((32, 32)).with_plm(true),
    )
    .unwrap();
    assert!(decode(&stream).is_ok());
    let (_, payload) = main_header_segments(&stream, PLM)[0].clone();
    // Payload: Zplm, then per tile-part Nplm + lengths. Nudge the last
    // byte of the series — the low seven bits of a packet length.
    let mut wrong = stream.clone();
    wrong[payload.end - 1] ^= 0x01;
    assert!(matches!(decode(&wrong), Err(Error::PlmMismatch)));

    // A repeated Zplm makes the concatenation ambiguous.
    let (marker_at, _) = main_header_segments(&stream, PLM)[0].clone();
    let segment = stream[marker_at..payload.end].to_vec();
    let mut doubled = stream[..payload.end].to_vec();
    doubled.extend_from_slice(&segment);
    doubled.extend_from_slice(&stream[payload.end..]);
    assert!(matches!(decode(&doubled), Err(Error::PlmMismatch)));

    // A series that stops one tile-part short does not describe "every
    // tile-part": drop the final (Nplm, Iplm…) entry and fix Lplm.
    let entries = plm_entries(&stream);
    assert_eq!(entries.len(), 4);
    let series = &stream[payload.start + 1..payload.end];
    let mut at = 0;
    for _ in 0..3 {
        at += 1 + usize::from(series[at]);
    }
    let dropped = series.len() - at;
    let mut short = stream[..payload.end - dropped].to_vec();
    short.extend_from_slice(&stream[payload.end..]);
    let lplm = (payload.len() + 2 - dropped) as u16;
    short[marker_at + 2..marker_at + 4].copy_from_slice(&lplm.to_be_bytes());
    assert!(matches!(decode(&short), Err(Error::PlmMismatch)));
}

#[test]
fn a_tile_part_past_the_nplm_range_refuses_plm() {
    // 30 layers × 6 resolutions × 3 components = 540 packets in one
    // tile-part: more Iplm bytes than the 8-bit Nplm can count.
    let rgb = image(64, 64, PixelFormat::Rgb24);
    let options = base().with_decomposition_levels(5).with_layers(30);
    assert!(encode(&rgb, &options).is_ok());
    assert!(encode(&rgb, &options.clone().with_plm(true)).is_err());
    // Splitting the tile into parts brings each one back in range.
    let mut split = options.with_plm(true);
    split.tile_parts = TilePartSplit::ByLayer;
    let stream = encode(&rgb, &split).unwrap();
    assert_eq!(plm_entries(&stream).len(), 30);
    assert!(decode(&stream).is_ok());
}

#[test]
fn a_long_series_continues_across_plm_segments() {
    // 1024 tile-parts, each listing 25 layers of every non-empty
    // resolution level: more Iplm bytes than one marker segment
    // holds, so the series continues in a second segment.
    let gray = image(128, 128, PixelFormat::Gray8);
    let options = base()
        .with_tile_size((4, 4))
        .with_decomposition_levels(4)
        .with_layers(25)
        .with_plm(true);
    let stream = encode(&gray, &options).unwrap();
    let segments = main_header_segments(&stream, PLM);
    assert!(
        segments.len() >= 2,
        "{} segment(s), {:?} bytes",
        segments.len(),
        segments.iter().map(|(_, p)| p.len()).collect::<Vec<_>>()
    );
    let entries = plm_entries(&stream);
    assert_eq!(entries.len(), 1024);
    assert!(
        entries
            .iter()
            .all(|lengths| lengths.len() >= 75 && lengths.len() % 25 == 0)
    );
    assert_eq!(decode(&stream).unwrap().planes, gray.planes);

    // Cut the series inside a tile-part's run: a truncated series is
    // rejected, not read as far as it goes.
    let (marker_at, payload) = segments.last().unwrap().clone();
    let mut cut = stream[..payload.end - 5].to_vec();
    cut.extend_from_slice(&stream[payload.end..]);
    let lplm = (payload.len() + 2 - 5) as u16;
    cut[marker_at + 2..marker_at + 4].copy_from_slice(&lplm.to_be_bytes());
    assert!(matches!(decode(&cut), Err(Error::PlmMismatch)));
}

/// With relocated packet headers a packet without code-block data
/// occupies no body bytes, so a rate-controlled stream ends its
/// tile-parts on zero-length packets and has them sitting on tile-part
/// boundaries. The lists still describe every packet, and the decoder
/// accepts them (fuzz find: such an own-encoded stream was rejected
/// with a `PLT` or `PLM` mismatch).
#[test]
fn empty_packets_on_tile_part_boundaries_satisfy_plt_and_plm() {
    let rgb = image(39, 27, PixelFormat::Rgb24);
    let mut streams_with_empty_tail = 0;
    for packed in [PackedHeaders::Ppt, PackedHeaders::Ppm] {
        for split in [
            TilePartSplit::Single,
            TilePartSplit::ByResolution,
            TilePartSplit::ByLayer,
            TilePartSplit::ByComponent,
        ] {
            for target in [1816, 600, 100] {
                for (plt, plm) in [(true, false), (false, true), (true, true)] {
                    let mut options = base()
                        .with_decomposition_levels(5)
                        .with_code_block_exp((3, 4))
                        .with_lossy(5)
                        .with_progression(ProgressionOrder::Pcrl)
                        .with_layers(3)
                        .with_target_bytes(target)
                        .with_plt(plt)
                        .with_plm(plm);
                    options.packed_headers = packed;
                    options.tile_parts = split;
                    let stream = encode(&rgb, &options).unwrap();
                    let context = format!("{packed:?} {split:?} {target} plt={plt} plm={plm}");
                    // (The markers count against the byte budget, so the
                    // image is not the unmarked stream's; it must decode.)
                    let decoded = decode(&stream).unwrap_or_else(|e| panic!("{context}: {e}"));
                    assert_eq!((decoded.width, decoded.height), (39, 27), "{context}");

                    if !plm {
                        continue;
                    }
                    let entries = plm_entries(&stream);
                    if entries.last().and_then(|list| list.last()) != Some(&0) {
                        continue;
                    }
                    streams_with_empty_tail += 1;
                    // The final empty packet is still a packet of the
                    // last tile-part: a list that leaves it out does
                    // not describe the tile-part. Drop its `Iplm` byte
                    // (and count one byte fewer in `Nplm` and `Lplm`).
                    let segments = main_header_segments(&stream, PLM);
                    if segments.len() != 1 {
                        continue;
                    }
                    let (marker_at, payload) = segments[0].clone();
                    let series = &stream[payload.start + 1..payload.end];
                    let mut last_nplm = 0;
                    let mut at = 0;
                    while at < series.len() {
                        last_nplm = at;
                        at += 1 + usize::from(series[at]);
                    }
                    let mut shorter = stream[..payload.end - 1].to_vec();
                    shorter.extend_from_slice(&stream[payload.end..]);
                    shorter[payload.start + 1 + last_nplm] -= 1;
                    let lplm = (payload.len() + 2 - 1) as u16;
                    shorter[marker_at + 2..marker_at + 4].copy_from_slice(&lplm.to_be_bytes());
                    assert!(
                        matches!(decode(&shorter), Err(Error::PlmMismatch)),
                        "{context}"
                    );
                }
            }
        }
    }
    assert!(
        streams_with_empty_tail > 0,
        "the sweep reaches the empty-tail case"
    );
}
