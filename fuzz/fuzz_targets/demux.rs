#![no_main]

//! Fuzz target for the framework containers: `open_demuxer` on
//! arbitrary bytes (a JP2 / JPH file or a bare codestream, the sniff
//! decides), the packet through the registry decoder under tight
//! limits, and both muxers over the demuxed packet. Every step must
//! return a `Result`; nothing may panic, overflow or allocate an
//! attacker-scaled buffer.
//!
//! ## Bounds
//!
//! The decoder runs under `DecoderLimits` of 1 Mi pixels and a 64 MiB
//! working set.

use libfuzzer_sys::fuzz_target;
use oxideav_core::{DecoderLimits, Error, NullCodecResolver, ReadSeek, WriteSeek};
use oxideav_jpeg2000::container::{open_demuxer, open_muxer_j2k, open_muxer_jp2};

fuzz_target!(|data: &[u8]| {
    let input: Box<dyn ReadSeek> = Box::new(std::io::Cursor::new(data.to_vec()));
    let Ok(mut demux) = open_demuxer(input, &NullCodecResolver) else {
        return;
    };
    let stream = demux.streams()[0].clone();
    assert!(stream.params.width.is_some_and(|w| w > 0));
    assert!(stream.params.height.is_some_and(|h| h > 0));
    let Ok(pkt) = demux.next_packet() else {
        return;
    };
    assert_eq!(pkt.data.len(), data.len());
    assert!(matches!(demux.next_packet(), Err(Error::Eof)));

    let mut params = stream.params.clone();
    params.limits = DecoderLimits::default()
        .with_max_pixels_per_frame(1 << 20)
        .with_max_alloc_bytes_per_frame(64 << 20);
    if let Ok(mut dec) = oxideav_jpeg2000::make_decoder(&params) {
        if dec.send_packet(&pkt).is_ok() {
            let _ = dec.receive_frame();
        }
    }

    for open in [open_muxer_j2k, open_muxer_jp2] {
        let out: Box<dyn WriteSeek> = Box::new(std::io::Cursor::new(Vec::new()));
        if let Ok(mut mux) = open(out, std::slice::from_ref(&stream)) {
            let _ = mux.write_header();
            let _ = mux.write_packet(&pkt);
            let _ = mux.write_trailer();
        }
    }
});
