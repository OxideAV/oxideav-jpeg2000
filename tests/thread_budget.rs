//! The worker budget never changes the output: every thread count
//! emits the same codestream bytes and decodes to the same samples as
//! the serial path, on the contract API and through the framework
//! adapters' `ExecutionContext`.

use oxideav_jpeg2000::encode::RoiRegion;
use oxideav_jpeg2000::{
    Container, DecodeOptions, EncodeOptions, Jpeg2000Image, PixelFormat, ProgressionOrder,
    decode_with, encode,
};

fn synthetic(width: u32, height: u32, format: PixelFormat, seed: u32) -> Jpeg2000Image {
    let wide = matches!(format, PixelFormat::Rgb48Le | PixelFormat::Gray16Le);
    let max = if wide { 65_535i64 } else { 255 };
    let mut random = seed;
    let mut data = Vec::new();
    for y in 0..i64::from(height) {
        for x in 0..i64::from(width) {
            for c in 0..format.components() as i64 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = i64::from((random >> 9) % 19) - 9;
                let wave = ((x * (2 + c) + y * 3) % 83 - 41).abs() * max / 50;
                let edge = if (x / 23 + y / 17 + c) % 3 == 0 {
                    max / 4
                } else {
                    0
                };
                let value = (wave / 2 + edge + noise * max / 255).clamp(0, max) as u16;
                if wide {
                    data.extend_from_slice(&value.to_le_bytes());
                } else {
                    data.push(value as u8);
                }
            }
        }
    }
    Jpeg2000Image::packed(width, height, format, data).unwrap()
}

fn variants() -> Vec<(&'static str, EncodeOptions)> {
    let base = || EncodeOptions::new().with_container(Container::J2k);
    let mut styled = base().with_layers(3).with_target_bytes(7000);
    styled.bypass = true;
    styled.terminate_all = true;
    styled.segmentation_symbols = true;
    let mut roi = base();
    roi.roi = Some(RoiRegion {
        x0: 30,
        y0: 20,
        x1: 100,
        y1: 70,
    });
    let mut mixed = base();
    mixed.ht_mixed = true;
    vec![
        ("lossless", base().with_decomposition_levels(4)),
        ("lossless-jp2", EncodeOptions::new()),
        (
            "lossless-tiles-rpcl",
            base()
                .with_tile_size((64, 48))
                .with_progression(ProgressionOrder::Rpcl),
        ),
        (
            "lossless-small-blocks-layers",
            base().with_code_block_exp((3, 4)).with_layers(4),
        ),
        ("lossless-rate", base().with_target_bytes(9000)),
        ("lossy", base().with_lossy(3)),
        (
            "lossy-rate-layers",
            base().with_lossy(2).with_layers(3).with_target_bytes(6000),
        ),
        ("lossy-psnr", base().with_lossy(2).with_target_psnr(36.0)),
        ("styled-rate", styled),
        ("roi", roi),
        ("ht", base().with_high_throughput(true)),
        (
            "ht-layers-tiles",
            base()
                .with_high_throughput(true)
                .with_layers(3)
                .with_tile_size((80, 80)),
        ),
        ("ht-mixed", mixed),
    ]
}

#[test]
fn every_budget_emits_the_serial_codestream_and_samples() {
    let images = [
        synthetic(150, 110, PixelFormat::Rgb24, 3),
        synthetic(97, 131, PixelFormat::Gray8, 5),
        synthetic(120, 90, PixelFormat::Rgb48Le, 9),
    ];
    for (image_index, image) in images.iter().enumerate() {
        for (name, options) in variants() {
            let serial = match encode(image, &options) {
                Ok(bytes) => bytes,
                // A combination this layout cannot take (e.g. the ROI
                // magnitude lane at 16 bits) must fail at every budget.
                Err(error) => {
                    for threads in [2, 7] {
                        let again = encode(image, &options.clone().with_threads(threads));
                        assert_eq!(
                            again.unwrap_err().to_string(),
                            error.to_string(),
                            "image {image_index} {name} threads {threads}"
                        );
                    }
                    continue;
                }
            };
            let reference = decode_with(&serial, &DecodeOptions::new()).unwrap();
            for threads in [0, 2, 3, 7, 32] {
                let bytes = encode(image, &options.clone().with_threads(threads)).unwrap();
                assert_eq!(
                    bytes, serial,
                    "image {image_index} {name}: codestream differs at {threads} threads"
                );
                for (reduce, layers) in [(0, None), (1, None), (0, Some(1))] {
                    let serial_options =
                        DecodeOptions::new().with_reduce(reduce).with_layers(layers);
                    let expected = decode_with(&serial, &serial_options).unwrap();
                    let decoded =
                        decode_with(&serial, &serial_options.clone().with_threads(threads))
                            .unwrap();
                    assert_eq!(
                        decoded.planes, expected.planes,
                        "image {image_index} {name}: samples differ at {threads} threads \
                         (reduce {reduce}, layers {layers:?})"
                    );
                }
            }
            assert_eq!(reference.width, image.width);
        }
    }
}

#[test]
fn corrupt_streams_fail_identically_at_every_budget() {
    let image = synthetic(128, 96, PixelFormat::Rgb24, 21);
    let options = EncodeOptions::new()
        .with_container(Container::J2k)
        .with_code_block_exp((4, 4));
    let clean = encode(&image, &options).unwrap();
    let mut state = 0x9e37_79b9_u32;
    for _ in 0..40 {
        let mut bytes = clean.clone();
        for _ in 0..3 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let at = 120 + (state >> 8) as usize % (bytes.len() - 120);
            bytes[at] ^= (state >> 3) as u8 | 1;
        }
        let describe =
            |threads: usize| match decode_with(&bytes, &DecodeOptions::new().with_threads(threads))
            {
                Ok(image) => Ok(image.planes),
                Err(error) => Err(error.to_string()),
            };
        let serial = describe(1);
        for threads in [2, 6] {
            assert_eq!(describe(threads), serial, "threads {threads}");
        }
    }
}

#[cfg(feature = "registry")]
#[test]
fn execution_context_budget_is_output_neutral() {
    use oxideav_core::{
        CodecId, CodecParameters, ExecutionContext, Frame, Packet, TimeBase, VideoFrame,
    };
    use oxideav_jpeg2000::{CODEC_ID_STR, make_decoder, make_encoder};

    let image = synthetic(140, 100, PixelFormat::Rgb24, 33);
    let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    params.width = Some(image.width);
    params.height = Some(image.height);
    params.pixel_format = Some(oxideav_core::PixelFormat::Rgb24);
    let frame = Frame::Video(VideoFrame::from(&image));
    let run = |context: Option<ExecutionContext>| {
        let mut encoder = make_encoder(&params).unwrap();
        if let Some(context) = &context {
            encoder.set_execution_context(context);
        }
        encoder.send_frame(&frame).unwrap();
        let packet = encoder.receive_packet().unwrap();
        let mut decoder = make_decoder(&params).unwrap();
        if let Some(context) = &context {
            decoder.set_execution_context(context);
        }
        decoder
            .send_packet(&Packet::new(0, TimeBase::new(1, 25), packet.data.clone()))
            .unwrap();
        let Frame::Video(video) = decoder.receive_frame().unwrap() else {
            panic!("video frame expected");
        };
        let planes: Vec<Vec<u8>> = video.planes.iter().map(|p| p.data.clone()).collect();
        (packet.data, planes)
    };
    let serial = run(None);
    assert_eq!(run(Some(ExecutionContext::serial())), serial);
    assert_eq!(run(Some(ExecutionContext::with_threads(4))), serial);
    assert_eq!(run(Some(ExecutionContext::with_threads(64))), serial);
}
