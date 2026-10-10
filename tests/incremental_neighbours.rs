//! First-use cache initialization for blocks supplied by another decoder.
use oxideav_jpeg2000::{
    geometry::SubBandOrientation,
    mq::MqDecoder,
    mqenc::MqEncoder,
    t1::{reset_contexts, CodeBlock, Coefficient, REFINEMENT_CTX_OFFSET},
};

#[test]
fn preassembled_refinement_uses_existing_significant_neighbours() {
    for (width, height, positions) in [(2, 1, [0, 1]), (1, 2, [0, 1]), (1, 5, [3, 4])] {
        for causal in [false, true] {
            for orientation in [
                SubBandOrientation::LL,
                SubBandOrientation::LH,
                SubBandOrientation::HL,
                SubBandOrientation::HH,
            ] {
                let mut initial = vec![Coefficient::default(); width * height];
                for (i, &position) in positions.iter().enumerate() {
                    initial[position] = Coefficient {
                        magnitude: 2,
                        sigma: true,
                        sign: i != 0,
                        already_refined: false,
                    };
                }
                let mut targets = initial.clone();
                targets[positions[0]].magnitude |= 1;

                // Both samples have a significant neighbour. VSC hides the
                // lower neighbour only at row 3, at the first stripe's end.
                let first_label = usize::from(!(causal && height == 5));
                let mut expected_ctx = reset_contexts();
                let mut expected = MqEncoder::new();
                expected.encode(&mut expected_ctx[REFINEMENT_CTX_OFFSET + first_label], 1);
                expected.encode(&mut expected_ctx[REFINEMENT_CTX_OFFSET + 1], 0);
                let expected_bytes = expected.flush();

                let make_block = || {
                    CodeBlock::from_coefficients(orientation, width, height, initial.clone())
                        .with_vertically_causal_context(causal)
                };
                let mut encoded = make_block();
                let mut encoder_ctx = reset_contexts();
                let mut encoder = MqEncoder::new();
                encoded.magnitude_refinement_encode(0, &targets, &mut encoder, &mut encoder_ctx);
                assert_eq!(encoder_ctx, expected_ctx);
                assert_eq!(encoder.flush(), expected_bytes);

                let mut decoded = make_block();
                let mut decoder_ctx = reset_contexts();
                let mut decoder = MqDecoder::new(&expected_bytes);
                assert_eq!(
                    decoded
                        .magnitude_refinement_pass(0, &mut decoder, &mut decoder_ctx)
                        .unwrap(),
                    2
                );
                assert_eq!(decoder_ctx, expected_ctx);
                for &position in &positions {
                    let (u, v) = (position % width, position / width);
                    let coefficient = decoded.coefficient(u, v);
                    assert_eq!(coefficient, encoded.coefficient(u, v));
                    assert_eq!(coefficient.magnitude, targets[position].magnitude);
                    assert!(coefficient.already_refined);
                    assert_eq!(decoded.decoded_bits(u, v), 1);
                }
            }
        }
    }
}
