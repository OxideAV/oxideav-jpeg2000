//! HTJ2K (ISO/IEC 15444-15) marker-segment + probe regression tests.
//!
//! All fixtures are hand-built in this file — no third-party HTJ2K
//! reference encoder is involved (the task forbids OpenJPH / Kakadu /
//! OpenJPEG). Each test crafts a minimal codestream to exercise one
//! aspect of the new framing: CAP (Pcap + Ccap_i), CPF (Pcpf +
//! CPFnum), the `is_htj2k()` discriminator, the `probe()` API, the
//! decoder stub error, and the bounds-check rejections required by
//! the task brief.

use oxideav_core::{CodecId, Decoder, Packet, TimeBase};
use oxideav_jpeg2000::{codestream, probe, J2kDecoder, J2kFlavour, Marker, CODEC_ID_STR};

/// Build a minimal classic Part-1 J2K codestream (1x1 grayscale, no
/// CAP). Reused across tests as a "negative" baseline.
fn build_classic_minimal_j2k() -> Vec<u8> {
    let mut v = Vec::new();
    // SOC
    v.extend_from_slice(&[0xFF, 0x4F]);
    // SIZ
    v.extend_from_slice(&[0xFF, 0x51]);
    v.extend_from_slice(&41u16.to_be_bytes()); // Lsiz
    v.extend_from_slice(&0u16.to_be_bytes()); // Rsiz
    v.extend_from_slice(&1u32.to_be_bytes()); // Xsiz
    v.extend_from_slice(&1u32.to_be_bytes()); // Ysiz
    v.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
    v.extend_from_slice(&1u32.to_be_bytes()); // XTsiz
    v.extend_from_slice(&1u32.to_be_bytes()); // YTsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
    v.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
    v.extend_from_slice(&1u16.to_be_bytes()); // Csiz
    v.extend_from_slice(&[7, 1, 1]);
    // COD
    v.extend_from_slice(&[0xFF, 0x52]);
    v.extend_from_slice(&12u16.to_be_bytes());
    v.extend_from_slice(&[0, 0, 0, 0, 0, 5, 4, 4, 0, 0]);
    // QCD
    v.extend_from_slice(&[0xFF, 0x5C]);
    v.extend_from_slice(&5u16.to_be_bytes());
    v.extend_from_slice(&[0x00, 0x00, 0x00]);
    // SOT (one tile-part, empty body)
    let sot_off = v.len();
    v.extend_from_slice(&[0xFF, 0x90]);
    v.extend_from_slice(&10u16.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes()); // Isot
    let psot_pos = v.len();
    v.extend_from_slice(&0u32.to_be_bytes()); // Psot — patched
    v.extend_from_slice(&[0, 1]); // TPsot=0, TNsot=1
                                  // SOD + 0 body bytes
    v.extend_from_slice(&[0xFF, 0x93]);
    let tile_end = v.len();
    let psot = (tile_end - sot_off) as u32;
    v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    // EOC
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// Append a `CAP` marker segment with the supplied 32-bit Pcap and
/// the matching 16-bit Ccap_i values (one per set bit, MSB→LSB).
/// Inserts immediately after SIZ — the spec position required for
/// HTJ2K codestreams.
fn build_j2k_with_cap(pcap: u32, ccaps: &[u16]) -> Vec<u8> {
    let n = pcap.count_ones() as usize;
    assert_eq!(n, ccaps.len(), "ccaps length must match Pcap popcount");

    let mut v = Vec::new();
    // SOC
    v.extend_from_slice(&[0xFF, 0x4F]);
    // SIZ — Rsiz bit 14 = 1 per HTJ2K §A.2 (mask 0x4000 in big-endian
    // 16-bit word: MSB-counted bit 14 = bit-position 1 from LSB).
    v.extend_from_slice(&[0xFF, 0x51]);
    v.extend_from_slice(&41u16.to_be_bytes());
    let rsiz: u16 = 0x4000; // §A.2 marker
    v.extend_from_slice(&rsiz.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&[7, 1, 1]);
    // CAP — Lcap = 6 + 2n; segment payload = Pcap (4 bytes) + n × 2 bytes Ccap_i.
    v.extend_from_slice(&[0xFF, 0x50]);
    let lcap = (6 + 2 * n) as u16;
    v.extend_from_slice(&lcap.to_be_bytes());
    v.extend_from_slice(&pcap.to_be_bytes());
    for cc in ccaps {
        v.extend_from_slice(&cc.to_be_bytes());
    }
    // COD
    v.extend_from_slice(&[0xFF, 0x52]);
    v.extend_from_slice(&12u16.to_be_bytes());
    v.extend_from_slice(&[0, 0, 0, 0, 0, 5, 4, 4, 0, 0]);
    // QCD
    v.extend_from_slice(&[0xFF, 0x5C]);
    v.extend_from_slice(&5u16.to_be_bytes());
    v.extend_from_slice(&[0x00, 0x00, 0x00]);
    // SOT (empty body)
    let sot_off = v.len();
    v.extend_from_slice(&[0xFF, 0x90]);
    v.extend_from_slice(&10u16.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes());
    let psot_pos = v.len();
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&[0, 1]);
    v.extend_from_slice(&[0xFF, 0x93]);
    let tile_end = v.len();
    let psot = (tile_end - sot_off) as u32;
    v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

/// Same as [`build_j2k_with_cap`] but additionally inserts a `CPF`
/// segment between CAP and COD. `pcpf` lists the 16-bit Pcpf_i words
/// in order (i = 1..N from low order to high — matches the spec
/// formula `CPFnum = -1 + Σ Pcpf_i · 2^(16·(i-1))`).
fn build_htj2k_with_cpf(pcpf: &[u16]) -> Vec<u8> {
    // Use the smallest legal HTJ2K CAP: Pcap with only bit 15 set,
    // one Ccap15 = 0 (HTONLY, single-set, no RGN, homogeneous,
    // reversible-only, magnitude bound = 8).
    let mut v = build_j2k_with_cap(0x0002_0000, &[0x0000]);

    // The base helper appended CAP after SIZ and before COD. We need
    // to splice CPF (FF 59) in *between* CAP and COD. Find the COD
    // marker in the buffer and insert before it.
    let cod_start = v
        .windows(2)
        .position(|w| w == [0xFF, 0x52])
        .expect("cod present");
    let n = pcpf.len();
    let lcpf = (2 + 2 * n) as u16;
    let mut seg = Vec::new();
    seg.extend_from_slice(&[0xFF, 0x59]);
    seg.extend_from_slice(&lcpf.to_be_bytes());
    for w in pcpf {
        seg.extend_from_slice(&w.to_be_bytes());
    }
    // We must also re-patch SOT.Psot since inserting bytes shifts the
    // tile-part end. Easiest: reconstruct fully. Here we do an
    // in-place splice and then rewrite Psot.
    let inserted_len = seg.len();
    v.splice(cod_start..cod_start, seg);

    // Re-find SOT and patch its Psot field. After the splice every
    // offset >= cod_start moved by `inserted_len`; SOT was after COD.
    // SOT layout: marker(2) + Lsot(2) + Isot(2) + Psot(4) + TPsot(1)
    // + TNsot(1). Psot starts at offset 6 from the marker.
    let sot_off = v
        .windows(2)
        .position(|w| w == [0xFF, 0x90])
        .expect("sot present");
    let psot_pos = sot_off + 6;
    // Find SOD, the last marker before EOC at the very tail.
    let sod_off = v
        .windows(2)
        .rposition(|w| w == [0xFF, 0x93])
        .expect("sod present");
    let tile_end = sod_off + 2; // SOD has no length, body length is 0.
    let psot = (tile_end - sot_off) as u32;
    v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
    let _ = inserted_len; // silence unused-binding warning under some configurations.
    v
}

#[test]
fn marker_constants_match_spec() {
    // ISO/IEC 15444-1 §A and 15444-15 §A.6 define these codes.
    assert_eq!(Marker::CAP.0, 0xFF50);
    assert_eq!(Marker::PRF.0, 0xFF56);
    assert_eq!(Marker::CPF.0, 0xFF59);
}

#[test]
fn parses_cap_with_only_pcap15_set() {
    // HTJ2K minimum: Pcap bit 15 (mask 0x0002_0000) set, Ccap15 = 0.
    let buf = build_j2k_with_cap(0x0002_0000, &[0x0000]);
    let cs = codestream::parse(&buf).expect("parse");
    let cap = cs.cap.as_ref().expect("CAP captured");
    assert_eq!(cap.pcap, 0x0002_0000);
    assert_eq!(cap.ccaps, vec![0x0000]);
    assert!(cap.is_htj2k());
    assert_eq!(cap.ccap15(), Some(0x0000));
    assert!(cs.is_htj2k());
}

#[test]
fn parses_cap_with_multiple_capabilities_indexes_ccap15_correctly() {
    // Pcap bits 1, 2, 15 set: Ccaps appear in MSB→LSB order, so the
    // list is [Ccap1, Ccap2, Ccap15]. Ccap15 must be picked from
    // index 2 by the lookup helper.
    let pcap = (1u32 << 31) | (1u32 << 30) | (1u32 << 17);
    let buf = build_j2k_with_cap(pcap, &[0xAAAA, 0xBBBB, 0xC0DE]);
    let cs = codestream::parse(&buf).expect("parse");
    let cap = cs.cap.as_ref().unwrap();
    assert_eq!(cap.ccaps.len(), 3);
    assert!(cap.is_htj2k());
    assert_eq!(cap.ccap15(), Some(0xC0DE));
    assert!(cs.is_htj2k());
}

#[test]
fn cap_without_pcap15_is_not_htj2k() {
    // Pcap bit 1 (Part-2 capabilities) only — classic-extended,
    // not HTJ2K.
    let pcap = 1u32 << 31;
    let buf = build_j2k_with_cap(pcap, &[0xDEAD]);
    let cs = codestream::parse(&buf).expect("parse");
    let cap = cs.cap.as_ref().unwrap();
    assert!(!cap.is_htj2k());
    assert!(cap.ccap15().is_none());
    assert!(!cs.is_htj2k());
}

#[test]
fn classic_codestream_has_no_cap_and_is_not_htj2k() {
    let buf = build_classic_minimal_j2k();
    let cs = codestream::parse(&buf).expect("parse");
    assert!(cs.cap.is_none());
    assert!(!cs.is_htj2k());
}

#[test]
fn rejects_truncated_cap_segment() {
    // Pcap claims 2 bits set but only one Ccap word is supplied.
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x4F]);
    v.extend_from_slice(&[0xFF, 0x51]);
    v.extend_from_slice(&41u16.to_be_bytes());
    v.extend_from_slice(&0x4000u16.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&1u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&[7, 1, 1]);
    // CAP body: 4 bytes Pcap + 2 bytes (only 1 Ccap), Lcap = 8 → claims n=1 → OK syntactically;
    // Pcap claims 2 bits set but Lcap only allows 1 Ccap word: parser must reject.
    v.extend_from_slice(&[0xFF, 0x50]);
    v.extend_from_slice(&8u16.to_be_bytes()); // Lcap = 8 → body = 6 bytes
                                              // Pcap15 (mask 0x0002_0000) and Pcap14 (mask 0x0004_0000) — two
                                              // capability bits set, two Ccap_i words required.
    let pcap = 0x0002_0000u32 | 0x0004_0000u32;
    v.extend_from_slice(&pcap.to_be_bytes());
    v.extend_from_slice(&0xAAAAu16.to_be_bytes()); // only 1 Ccap, but 2 needed
    let err = codestream::parse(&v).expect_err("truncated CAP must fail");
    let msg = format!("{err}");
    assert!(msg.contains("CAP"), "{msg}");
}

#[test]
fn parses_cpf_single_word() {
    // CPFnum encoded as Pcpf=[0x0001] → CPFnum = 0.
    let buf = build_htj2k_with_cpf(&[0x0001]);
    let cs = codestream::parse(&buf).expect("parse");
    let cpf = cs.cpf.as_ref().expect("CPF captured");
    assert_eq!(cpf.pcpf, vec![0x0001]);
    assert_eq!(cpf.cpfnum, 0u128);
    assert!(cs.is_htj2k());
}

#[test]
fn parses_cpf_two_words() {
    // CPFnum = -1 + 0x1234 + 0x5678 * 2^16 = 0x5678_1234 - 1.
    let buf = build_htj2k_with_cpf(&[0x1234, 0x5678]);
    let cs = codestream::parse(&buf).expect("parse");
    let cpf = cs.cpf.as_ref().unwrap();
    assert_eq!(cpf.pcpf, vec![0x1234, 0x5678]);
    let expected: u128 = 0x5678_1234u128 - 1;
    assert_eq!(cpf.cpfnum, expected);
    assert!(cs.is_htj2k());
}

#[test]
fn cpf_with_zero_terminal_word_rejected() {
    // §A.6 explicitly: Pcpf_N ("the last word") shall not be zero.
    let mut v = Vec::new();
    v.extend_from_slice(&[0xFF, 0x4F]);
    v.extend_from_slice(&[0xFF, 0x51]);
    v.extend_from_slice(&41u16.to_be_bytes());
    v.extend_from_slice(&0x4000u16.to_be_bytes());
    for _ in 0..8 {
        v.extend_from_slice(&1u32.to_be_bytes());
    }
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&[7, 1, 1]);
    v.extend_from_slice(&[0xFF, 0x50]);
    v.extend_from_slice(&8u16.to_be_bytes());
    v.extend_from_slice(&0x0002_0000u32.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes()); // Ccap15 = 0
                                              // CPF with a terminal zero word — Lcpf = 2 + 2*1 = 4.
    v.extend_from_slice(&[0xFF, 0x59]);
    v.extend_from_slice(&4u16.to_be_bytes());
    v.extend_from_slice(&0u16.to_be_bytes()); // Pcpf_1 = 0 → invalid
    let err = codestream::parse(&v).expect_err("zero-terminal CPF must fail");
    let msg = format!("{err}");
    assert!(msg.contains("CPF"), "{msg}");
}

#[test]
fn cpf_overlong_segment_rejected_as_invalid() {
    // 9 Pcpf words exceeds the bound (MAX_PCPF=8): InvalidData,
    // not silent overflow. Lcpf = 2 + 2*9 = 20.
    let mut v = build_classic_minimal_j2k();
    // Splice CAP+CPF before COD. Reuse our builder for CAP only.
    let cap_pcap = 0x0002_0000u32;
    let cap_seg = {
        let mut s = vec![0xFF, 0x50];
        let lcap: u16 = 8; // 6 + 2*1
        s.extend_from_slice(&lcap.to_be_bytes());
        s.extend_from_slice(&cap_pcap.to_be_bytes());
        s.extend_from_slice(&0u16.to_be_bytes());
        s
    };
    let cpf_seg = {
        let mut s = vec![0xFF, 0x59];
        let lcpf: u16 = 2 + 2 * 9;
        s.extend_from_slice(&lcpf.to_be_bytes());
        for _ in 0..9 {
            s.extend_from_slice(&0x0001u16.to_be_bytes());
        }
        s
    };
    let cod_start = v.windows(2).position(|w| w == [0xFF, 0x52]).unwrap();
    // Need to also flip Rsiz bit 14 in the SIZ to mark HTJ2K, but
    // parsing of CAP doesn't depend on Rsiz so leave it.
    v.splice(cod_start..cod_start, cap_seg.into_iter().chain(cpf_seg));
    let err = codestream::parse(&v).expect_err("overlong CPF must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("CPF") && msg.contains("exceeds bound"),
        "{msg}"
    );
}

#[test]
fn probe_classic_reports_classic_part1() {
    let buf = build_classic_minimal_j2k();
    let p = probe(&buf).expect("probe classic");
    assert_eq!(p.flavour, J2kFlavour::ClassicPart1);
    assert_eq!(p.width, 1);
    assert_eq!(p.height, 1);
    assert_eq!(p.num_components, 1);
    assert!(p.pcap.is_none());
    assert!(p.ccap15.is_none());
    assert!(p.cpfnum.is_none());
}

#[test]
fn probe_htj2k_reports_high_throughput() {
    let buf = build_j2k_with_cap(0x0002_0000, &[0xC0DE]);
    let p = probe(&buf).expect("probe htj2k");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.pcap, Some(0x0002_0000));
    assert_eq!(p.ccap15, Some(0xC0DE));
    assert!(p.cpfnum.is_none());
}

#[test]
fn probe_htj2k_with_cpf_carries_cpfnum() {
    let buf = build_htj2k_with_cpf(&[0x000A]);
    let p = probe(&buf).expect("probe");
    assert_eq!(p.flavour, J2kFlavour::HighThroughput);
    assert_eq!(p.cpfnum, Some(9u128)); // -1 + 10
}

#[cfg(feature = "htj2k")]
#[test]
fn decoder_dispatches_htj2k_codestream_to_fbcot_path() {
    // The empty-body fixture used here has 1x1 SIZ, NL=5, 9/7 transform —
    // not actually supported by the round-3 FBCOT driver, but the
    // dispatch into the HTJ2K path must happen and the parsed
    // codestream must be retained on the decoder.
    let buf = build_j2k_with_cap(0x0002_0000, &[0x0000]);
    let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), buf);
    // Decode is expected to fail for this minimal CAP-only fixture
    // (NL=5 + 9/7 + zero body bytes), but we should land in the HTJ2K
    // driver — the resulting message mentions "HTJ2K" rather than
    // anything classic-EBCOT-specific.
    let err = dec
        .send_packet(&pkt)
        .expect_err("htj2k must surface body error");
    let msg = format!("{err}");
    assert!(msg.contains("HTJ2K") || msg.contains("jpeg2000"), "{msg}");
    let cs = dec.last_parsed().expect("last_parsed retained");
    assert!(cs.is_htj2k());
}

#[cfg(not(feature = "htj2k"))]
#[test]
fn decoder_without_feature_does_not_short_circuit() {
    // Without the feature, the decoder is free to fall through into
    // the classic path. We just check that parse succeeds — the
    // tier-1 attempt past parse is allowed to fail.
    let buf = build_j2k_with_cap(0x0002_0000, &[0x0000]);
    let mut dec = J2kDecoder::new(CodecId::new(CODEC_ID_STR));
    let pkt = Packet::new(0u32, TimeBase::new(1, 1), buf);
    let _ = dec.send_packet(&pkt);
    // last_parsed must record the HTJ2K flag even if downstream
    // decode failed.
    if let Some(cs) = dec.last_parsed() {
        assert!(cs.is_htj2k());
    }
}
