//! Integration test: decode a baseline (5/3 lossless) OpenJPEG fixture.
//!
//! `tests/fixtures/baseline.j2k` is a 64×64 8-bit grayscale image built
//! with `opj_compress` from an ffmpeg-generated `testsrc` pattern (see
//! the README for the exact command). The fixture exercises the
//! complete pipeline: marker parse → tier-2 packets → EBCOT bit-planes
//! → IDWT → level shift.

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

const BASELINE_J2K: &[u8] = include_bytes!("fixtures/baseline.j2k");

#[test]
fn baseline_decodes_to_sensible_pixels() {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), BASELINE_J2K.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    let frame = dec.receive_frame().expect("receive_frame");
    let vf = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    // Inspect luma statistics on the single plane.
    assert_eq!(vf.planes.len(), 1);
    let plane = &vf.planes[0];
    assert_eq!(plane.stride, 64);
    assert_eq!(plane.data.len(), 64 * 64);
    let sum: u64 = plane.data.iter().map(|&v| v as u64).sum();
    let mean = (sum / plane.data.len() as u64) as u32;
    assert!(
        (32..=224).contains(&mean),
        "luma mean {mean} out of sensible range"
    );
    let distinct = plane
        .data
        .iter()
        .copied()
        .collect::<std::collections::HashSet<u8>>();
    assert!(
        distinct.len() > 20,
        "too few distinct luma samples ({})",
        distinct.len()
    );
}
