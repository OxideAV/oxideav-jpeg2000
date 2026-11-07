//! Component scheduling regressions with interleaved packets and empty blocks.

use oxideav_jpeg2000::encode::encode_j2k;
use oxideav_jpeg2000::{
    EncodeOptions, ProgressionOrder, decode_j2k, decode_j2k_layers, decode_j2k_reduced,
};

const WIDTH: u32 = 17;
const HEIGHT: u32 = 13;
const ORDERS: [ProgressionOrder; 5] = [
    ProgressionOrder::Lrcp,
    ProgressionOrder::Rlcp,
    ProgressionOrder::Rpcl,
    ProgressionOrder::Pcrl,
    ProgressionOrder::Cprl,
];

fn planes() -> Vec<Vec<u8>> {
    (0..5)
        .map(|c| {
            (0..HEIGHT)
                .flat_map(|y| {
                    (0..WIDTH).map(move |x| {
                        if c % 2 == 0 {
                            // DC shifting produces no coded coefficients. Empty
                            // components before, between and after active ones
                            // must not consume the next component's blocks.
                            128
                        } else {
                            (((x * 19 + y * 31) ^ (c * 53 + x * y)) & 255) as u8
                        }
                    })
                })
                .collect()
        })
        .collect()
}

fn options(order: ProgressionOrder) -> EncodeOptions {
    EncodeOptions::default()
        .with_decomposition_levels(2)
        .with_code_block_exp((3, 3))
        .with_precincts(vec![0x44; 3])
        .with_layers(3)
        .with_tile_size((9, 7))
        .with_mct(false)
        .with_progression(order)
}

#[test]
fn empty_components_preserve_later_blocks_in_all_progression_orders() {
    let planes = planes();
    let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
    for order in ORDERS {
        let bytes = encode_j2k(&refs, WIDTH, HEIGHT, &options(order)).unwrap();
        let decoded = decode_j2k(&bytes).unwrap();
        assert_eq!(decoded.components.len(), planes.len());
        for (c, input) in planes.iter().enumerate() {
            let expected: Vec<i32> = input.iter().map(|&v| i32::from(v)).collect();
            assert_eq!(
                decoded.components[c].samples, expected,
                "{order:?}, component {c}"
            );
        }
    }
}

#[test]
fn component_layer_and_resolution_limits_match_independent_planes() {
    let planes = planes();
    let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
    for order in ORDERS {
        let params = options(order);
        let multi = encode_j2k(&refs, WIDTH, HEIGHT, &params).unwrap();
        let singles: Vec<Vec<u8>> = refs
            .iter()
            .map(|plane| encode_j2k(&[*plane], WIDTH, HEIGHT, &params).unwrap())
            .collect();
        // Packet contributions from every selected layer must be complete
        // before a component's tier-1 work begins, including a partial pass.
        for limit in 1..=4 {
            let decoded = decode_j2k_layers(&multi, limit).unwrap();
            for (c, single) in singles.iter().enumerate() {
                let expected = decode_j2k_layers(single, limit).unwrap();
                assert_eq!(
                    decoded.components[c], expected.components[0],
                    "{order:?}, component {c}, layer limit {limit}"
                );
            }
        }
        // Kept levels are component-local; skipped high-resolution blocks
        // must not advance the iterator into another component's data.
        for discard in 0..=2 {
            let decoded = decode_j2k_reduced(&multi, discard).unwrap();
            for (c, single) in singles.iter().enumerate() {
                let expected = decode_j2k_reduced(single, discard).unwrap();
                assert_eq!(
                    decoded.components[c], expected.components[0],
                    "{order:?}, component {c}, discard {discard}"
                );
            }
        }
    }
}
