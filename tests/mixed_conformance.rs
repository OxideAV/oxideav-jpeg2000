//! T.814 §8.2 **MIXED**-set conformance codestreams — the three `hm`
//! (HT-mixed) streams of the ISO/IEC 15444-4 Ed. 4 electronic insert
//! (the only MIXED codestreams in that corpus), decoded end-to-end.
//!
//! In a MIXED tile-component every code-block is *individually* an HT
//! code-block or a T.800 code-block, with no per-block signalling —
//! the packet reader resolves the lanes by the staged derivation
//! (K(T.800) = 1, the §A.4 / §B.3 refutations, and depth-first
//! hypothesis search over the genuine set-`T` straddles), and tier-1
//! arbitrates the remaining blocks by trial decoding per the §A.4
//! NOTE.
//!
//! No available opaque decoder accepts these streams (the black-box
//! HT validators are SINGLEHT/HTONLY-only and the Part-1 validators
//! reject the MIXED style byte), so validation leans on the corpus's
//! own controlled redundancy: `ds0_hm_06_b11` and `ds0_hm_06_b18` are
//! **independent transcodes of the same base image** at different HT
//! magnitude bounds, and their losslessly carried components must —
//! and do — reconstruct **byte-identically** across the two streams.
//! The remaining component pair sits at the ≈40 dB the corpus's own
//! per-bundle statistics record. Structural facts (progression,
//! layers, sub-sampling, capability signalling) are pinned against
//! the archive's marker-syntax logs, and content hashes freeze the
//! decode for regression.
//!
//! The fixture directory carries the corpus `COPYRIGHT.txt` verbatim,
//! as its notice requires; use for JPEG 2000 conformance testing is
//! the granted use.

fn read_fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/conformance-mixed/{name}.j2k",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// FNV-1a over the sample stream — a dependency-free content pin.
fn fnv1a(samples: &[i32]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &s in samples {
        for b in s.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// All three MIXED conformance streams decode, with the component
/// geometry the SIZ declares (including the 2×1 / 1×2 / 2×2
/// sub-sampled planes of the four-component streams) and samples
/// inside the declared depth.
#[test]
fn mixed_conformance_streams_decode() {
    for (name, dims, depth_max) in [
        (
            "ds0_hm_06_b11",
            vec![(513, 129), (257, 129), (513, 65), (257, 65)],
            4095,
        ),
        (
            "ds0_hm_06_b18",
            vec![(513, 129), (257, 129), (513, 65), (257, 65)],
            4095,
        ),
        ("ds0_hm_15_b8", vec![(256, 256)], 7),
    ] {
        let bytes = read_fixture(name);
        let img = oxideav_jpeg2000::decode_j2k(&bytes)
            .unwrap_or_else(|e| panic!("{name}: decode failed: {e:?}"));
        assert_eq!(img.components.len(), dims.len(), "{name}: component count");
        for (c, (comp, &(w, h))) in img.components.iter().zip(dims.iter()).enumerate() {
            assert_eq!((comp.width, comp.height), (w, h), "{name} comp {c} extent");
            let lo = *comp.samples.iter().min().unwrap();
            let hi = *comp.samples.iter().max().unwrap();
            // Signed 4-bit for hm_15 (−8..=7), unsigned 12-bit for
            // the hm_06 pair (0..=4095) — Table A.11 ranges.
            let floor = if depth_max == 7 { -8 } else { 0 };
            assert!(
                lo >= floor && hi <= depth_max,
                "{name} comp {c} range [{lo}, {hi}]"
            );
        }
    }
}

/// The stream-level structure the archive's marker-syntax logs record:
/// `Rsiz` flags Part 15, the CAP `Ccap15` top bits are `11` (MIXED
/// permitted), and the COD carries the logged progression / layers.
#[test]
fn mixed_conformance_structure_matches_syntax_logs() {
    use oxideav_jpeg2000::ProgressionOrder;
    // (name, layers, progression per Table A.16: RPCL / PCRL)
    for (name, layers, prog) in [
        ("ds0_hm_06_b11", 4u16, ProgressionOrder::Rpcl),
        ("ds0_hm_06_b18", 4, ProgressionOrder::Rpcl),
        ("ds0_hm_15_b8", 8, ProgressionOrder::Pcrl),
    ] {
        let bytes = read_fixture(name);
        let header = oxideav_jpeg2000::parse_j2k_header(&bytes).expect(name);
        assert_eq!(header.siz.rsiz & (1 << 14), 1 << 14, "{name}: Rsiz bit 14");
        assert_eq!(header.cod.layers, layers, "{name}: layers");
        assert_eq!(header.cod.progression, prog, "{name}: progression");
        // CAP: marker(2) + Lcap(2) + Pcap(4) + Ccap15(2); the logs
        // record 0xd823 / 0xd823 / 0xd800.
        let mut pos = 2usize;
        let mut ccap15 = None;
        while pos + 4 <= header.bytes_consumed {
            let m = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
            let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
            if m == 0xFF50 {
                ccap15 = Some(u16::from_be_bytes([bytes[pos + 8], bytes[pos + 9]]));
            }
            pos += 2 + len;
        }
        let ccap15 = ccap15.expect("CAP present");
        assert_eq!(ccap15 >> 14, 0b11, "{name}: MIXED-permitted Ccap15");
    }
}

/// The corpus's controlled pair: `b11` and `b18` are independent
/// MIXED transcodes of the same base image at different HT cleanup
/// magnitude bounds. Their losslessly carried components (0, 1, 2 —
/// the per-bundle statistics record ∞ PSNR for 1 and 2, and the
/// shared lossy component-0 approximation) reconstruct
/// **byte-identically** across the two streams; component 3 (the one
/// whose COC restates the wavelet) differs at the ≈40 dB the
/// statistics record.
#[test]
fn mixed_cross_stream_components_agree() {
    let i11 = oxideav_jpeg2000::decode_j2k(&read_fixture("ds0_hm_06_b11")).unwrap();
    let i18 = oxideav_jpeg2000::decode_j2k(&read_fixture("ds0_hm_06_b18")).unwrap();
    for c in 0..3 {
        assert_eq!(
            i11.components[c].samples, i18.components[c].samples,
            "component {c} must match byte-exact across the two transcodes"
        );
    }
    let a = &i11.components[3].samples;
    let b = &i18.components[3].samples;
    assert_eq!(a.len(), b.len());
    let mse = a
        .iter()
        .zip(b.iter())
        .map(|(&x, &y)| f64::from(x - y) * f64::from(x - y))
        .sum::<f64>()
        / a.len() as f64;
    let psnr = 10.0 * (4095.0f64 * 4095.0 / mse).log10();
    assert!(
        (35.0..60.0).contains(&psnr),
        "component 3 cross-stream PSNR {psnr:.2} dB outside the expected band"
    );
}

/// Content pins: the decoded sample streams are frozen so any future
/// change to the MIXED lane resolution, the HT trial dispatch or the
/// Annex D fallback surfaces as a diff here.
#[test]
fn mixed_conformance_content_pinned() {
    for (name, want) in [
        ("ds0_hm_06_b11", 0x3cafab4f70777916u64),
        ("ds0_hm_06_b18", 0xea19193cc64f4bcb),
        ("ds0_hm_15_b8", 0x9c827a8750f12254),
    ] {
        let img = oxideav_jpeg2000::decode_j2k(&read_fixture(name)).unwrap();
        let mut h = 0xcbf29ce484222325u64;
        for comp in &img.components {
            h ^= fnv1a(&comp.samples);
            h = h.wrapping_mul(0x100000001b3);
        }
        assert_eq!(h, want, "{name}: content hash drifted");
    }
}

/// Layer-progressive decode of the MIXED streams: every layer-prefix
/// decode succeeds and its MSE against the full decode is monotone
/// non-increasing (the T.814 placeholder passes exist precisely to
/// preserve quality-layer boundaries through transcoding — §B.1
/// NOTE), with the final prefix equal to the full decode.
#[test]
fn mixed_layer_progressive_monotone() {
    for (name, layers) in [("ds0_hm_06_b11", 4u16), ("ds0_hm_15_b8", 8)] {
        let bytes = read_fixture(name);
        let full = oxideav_jpeg2000::decode_j2k(&bytes).unwrap();
        let mut prev = f64::INFINITY;
        for l in 1..=layers {
            let part = oxideav_jpeg2000::decode_j2k_layers(&bytes, l)
                .unwrap_or_else(|e| panic!("{name} layers {l}: {e:?}"));
            let mut se = 0.0f64;
            let mut n = 0usize;
            for (pc, fc) in part.components.iter().zip(full.components.iter()) {
                se += pc
                    .samples
                    .iter()
                    .zip(fc.samples.iter())
                    .map(|(&x, &y)| f64::from(x - y) * f64::from(x - y))
                    .sum::<f64>();
                n += pc.samples.len();
            }
            let mse = se / n as f64;
            assert!(
                mse <= prev + 1e-9,
                "{name}: MSE rose from {prev} to {mse} at layer prefix {l}"
            );
            prev = mse;
        }
        assert_eq!(
            prev, 0.0,
            "{name}: full layer prefix must equal full decode"
        );
    }
}

/// Reduced-resolution decode (ISO/IEC 15444-4 §B.2.3) of the MIXED
/// streams: the r1 surface decodes with the Equation B-14 ceiling
/// extents.
#[test]
fn mixed_reduced_resolution_decodes() {
    let img = oxideav_jpeg2000::decode_j2k_reduced(&read_fixture("ds0_hm_15_b8"), 1).unwrap();
    assert_eq!(
        (img.components[0].width, img.components[0].height),
        (128, 128)
    );
    let img = oxideav_jpeg2000::decode_j2k_reduced(&read_fixture("ds0_hm_06_b11"), 2).unwrap();
    assert_eq!(
        (img.components[0].width, img.components[0].height),
        (129, 33)
    );
}

/// The MIXED codestream decodes through the JP2 container route and
/// the historical byte-vector entry point too: `jp2::decode_jp2` on a
/// minimal JP2 wrapping of the 4-bit signed conformance stream
/// reconstructs the same samples as the raw-codestream decode, and
/// the registry-facing `decode_jpeg2000` sniffs both framings.
#[test]
fn mixed_decodes_through_jp2_container() {
    let codestream = read_fixture("ds0_hm_15_b8");
    let raw = oxideav_jpeg2000::decode_j2k(&codestream).unwrap();

    // Minimal Annex I file: signature, ftyp (brand 'jp2 '), jp2h with
    // ihdr (256×256 × 1 component, 4-bit signed → BPC = 0x83) and a
    // greyscale colr, then the jp2c codestream box.
    let mut file = Vec::new();
    file.extend_from_slice(&12u32.to_be_bytes());
    file.extend_from_slice(b"jP  ");
    file.extend_from_slice(&[0x0D, 0x0A, 0x87, 0x0A]);
    file.extend_from_slice(&20u32.to_be_bytes());
    file.extend_from_slice(b"ftypjp2 ");
    file.extend_from_slice(&0u32.to_be_bytes());
    file.extend_from_slice(b"jp2 ");
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&22u32.to_be_bytes());
    ihdr.extend_from_slice(b"ihdr");
    ihdr.extend_from_slice(&256u32.to_be_bytes()); // HEIGHT
    ihdr.extend_from_slice(&256u32.to_be_bytes()); // WIDTH
    ihdr.extend_from_slice(&1u16.to_be_bytes()); // NC
    ihdr.push(0x83); // BPC: 4-bit signed
    ihdr.extend_from_slice(&[7, 0, 0]); // C = jpeg2000, UnkC = 0, IPR = 0
    let mut colr = Vec::new();
    colr.extend_from_slice(&15u32.to_be_bytes());
    colr.extend_from_slice(b"colr");
    colr.extend_from_slice(&[1, 0, 0]); // METH = 1 (enumerated)
    colr.extend_from_slice(&17u32.to_be_bytes()); // greyscale
    file.extend_from_slice(&((8 + ihdr.len() + colr.len()) as u32).to_be_bytes());
    file.extend_from_slice(b"jp2h");
    file.extend_from_slice(&ihdr);
    file.extend_from_slice(&colr);
    file.extend_from_slice(&((8 + codestream.len()) as u32).to_be_bytes());
    file.extend_from_slice(b"jp2c");
    file.extend_from_slice(&codestream);

    let boxed = oxideav_jpeg2000::jp2::decode_jp2(&file).expect("JP2-wrapped MIXED decode");
    assert_eq!(boxed.components.len(), 1);
    assert_eq!(
        boxed.components[0].samples, raw.components[0].samples,
        "container route must match the raw-codestream decode"
    );

    // The historical interleaved-bytes entry point sniffs both
    // framings and routes through the same decode — and per its
    // documented contract rejects this stream's **signed** channel
    // cleanly (callers use `decode_j2k` / `decode_jp2` for the
    // planar surface) rather than mis-converting it.
    assert!(matches!(
        oxideav_jpeg2000::decode_jpeg2000(&file),
        Err(oxideav_jpeg2000::Error::NotImplemented)
    ));
    assert!(matches!(
        oxideav_jpeg2000::decode_jpeg2000(&codestream),
        Err(oxideav_jpeg2000::Error::NotImplemented)
    ));
}
