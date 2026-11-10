//! JP2 Header semantics on the contract API: the Resolution box
//! (T.800 §I.5.3.7) read by `info` and written by `encode`, and
//! premultiplied opacity (§I.5.3.6, Table I.16 `Typ = 2`, Equation I-5)
//! returned as the straight opacity the contract layouts carry.

use oxideav_jpeg2000::encode::encode_jp2_with;
use oxideav_jpeg2000::jp2::{
    ChannelDef, CmapEntry, CmapMapping, GridResolution, Jp2WriteOptions, Pclr, PclrColumn,
    Resolution, decode_jp2, parse_jp2, write_jp2,
};
use oxideav_jpeg2000::{
    Container, EncodeOptions, Jpeg2000Image, PixelFormat, decode, encode, info,
};

fn gradient(w: u32, h: u32) -> Jpeg2000Image {
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 5 % 253) as u8).collect();
    Jpeg2000Image::from_rgb8(w, h, rgb).unwrap()
}

// ---- Resolution box ---------------------------------------------------------

#[test]
fn grid_resolution_is_the_closest_representable_value() {
    // 300 dpi = 300 / 0.0254 = 1 500 000 / 127 points per metre: exact.
    let g = GridResolution::from_dpi(300.0, 72.0).unwrap();
    assert_eq!(
        (
            g.vertical_numerator,
            g.vertical_denominator,
            g.vertical_exponent
        ),
        (15_000, 127, 2)
    );
    assert!((g.vertical_dpi() - 300.0).abs() < 1e-9);
    assert!((g.horizontal_dpi() - 72.0).abs() < 1e-9);
    // Integers and decimal fractions are exact too.
    for v in [1.0, 2835.0, 11_811.0, 65_535.0, 0.5, 1e-3, 123_400_000.0] {
        let g = GridResolution::from_points_per_metre(v, v).unwrap();
        assert!(
            ((g.vertical_points_per_metre() - v) / v).abs() < 1e-12,
            "{v}: {g:?}"
        );
        assert_eq!(
            g.vertical_points_per_metre(),
            g.horizontal_points_per_metre()
        );
        assert!(g.vertical_numerator >= 1 && g.vertical_denominator >= 1);
    }
    // Anything else lands within a part in a million.
    for v in [std::f64::consts::PI * 1000.0, 2_834.645_669, 1e-90, 7.7e80] {
        let g = GridResolution::from_points_per_metre(v, 2.0 * v).unwrap();
        assert!(
            ((g.vertical_points_per_metre() - v) / v).abs() < 1e-6,
            "{v}"
        );
        assert!(((g.horizontal_points_per_metre() - 2.0 * v) / v).abs() < 1e-6);
    }
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-200, 1e200] {
        assert!(
            GridResolution::from_points_per_metre(bad, 1.0).is_none(),
            "{bad}"
        );
        assert!(
            GridResolution::from_points_per_metre(1.0, bad).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn resolution_box_travels_through_encode_and_info() {
    let img = gradient(12, 9);
    let capture = GridResolution::from_dpi(600.0, 300.0).unwrap();
    let display = GridResolution::from_dpi(96.0, 96.0).unwrap();
    for (c, d) in [
        (Some(capture), None),
        (None, Some(display)),
        (Some(capture), Some(display)),
    ] {
        for ht in [false, true] {
            let mut res = Resolution {
                capture: c,
                display: d,
            };
            let opts = EncodeOptions::default()
                .with_high_throughput(ht)
                .with_resolution(Some(res));
            let file = encode(&img, &opts).unwrap();
            let i = info(&file).unwrap();
            assert_eq!(i.resolution, Some(res));
            assert_eq!(parse_jp2(&file).unwrap().header.resolution, Some(res));
            assert_eq!(decode(&file).unwrap().as_bytes(), img.as_bytes());
            // The picture bytes do not depend on the box.
            let plain = encode(&img, &EncodeOptions::default().with_high_throughput(ht)).unwrap();
            assert!(info(&plain).unwrap().resolution.is_none());
            assert_eq!(
                file.len(),
                plain.len() + 8 + 18 * (c.iter().count() + d.iter().count())
            );
            // A zero numerator is not a resolution (Table I.20).
            if let Some(g) = res.capture.as_mut() {
                g.vertical_numerator = 0;
                assert!(encode(&img, &opts.clone().with_resolution(Some(res))).is_err());
            }
        }
    }
    // An empty Resolution box is not writable (§I.5.3.7), and a bare
    // codestream carries none.
    let empty = Resolution {
        capture: None,
        display: None,
    };
    assert!(encode(&img, &EncodeOptions::default().with_resolution(Some(empty))).is_err());
    let cs = encode(
        &img,
        &EncodeOptions::default()
            .with_container(Container::J2k)
            .with_resolution(Some(Resolution {
                capture: Some(capture),
                display: None,
            })),
    )
    .unwrap();
    assert!(info(&cs).unwrap().resolution.is_none());
}

// ---- premultiplied opacity --------------------------------------------------

/// Four 8-bit planes of a 4 × 2 picture: straight colour, opacity, and
/// the colour premultiplied per Equation I-5 (`Sp = S × α / max`,
/// rounded).
fn premultiplied_planes() -> ([Vec<u8>; 3], Vec<u8>, [Vec<u8>; 3]) {
    let alpha = vec![255u8, 128, 64, 0, 1, 200, 255, 17];
    let straight = [
        vec![255u8, 200, 100, 9, 255, 30, 0, 240],
        vec![0u8, 255, 50, 99, 0, 60, 128, 15],
        vec![17u8, 34, 255, 250, 255, 90, 255, 30],
    ];
    let pre = straight.clone().map(|p| {
        p.iter()
            .zip(&alpha)
            .map(|(&s, &a)| ((u32::from(s) * u32::from(a) + 127) / 255) as u8)
            .collect::<Vec<u8>>()
    });
    (straight, alpha, pre)
}

/// `round(Sp × 255 / α)` clipped — what a straight-opacity reader gets.
fn straight_of(pre: u8, a: u8) -> u8 {
    if a == 0 {
        0
    } else {
        ((u32::from(pre) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
    }
}

fn opacity_defs(typ: u16, asoc: &[u16]) -> Vec<ChannelDef> {
    let mut defs: Vec<ChannelDef> = (0..3u16)
        .map(|c| ChannelDef {
            channel: c,
            channel_type: ChannelDef::TYPE_COLOUR,
            association: c + 1,
        })
        .collect();
    defs.extend(asoc.iter().map(|&a| ChannelDef {
        channel: 3,
        channel_type: typ,
        association: a,
    }));
    defs
}

#[test]
fn premultiplied_opacity_decodes_to_straight_samples() {
    let (_, alpha, pre) = premultiplied_planes();
    let planes: Vec<&[u8]> = pre.iter().map(Vec::as_slice).chain([&alpha[..]]).collect();
    let params = EncodeOptions::default().with_decomposition_levels(1);
    let file_with = |defs: Vec<ChannelDef>| {
        let opts = Jp2WriteOptions::for_components(4).with_channel_definitions(Some(defs));
        encode_jp2_with(&planes, 4, 2, &params, &opts).unwrap()
    };

    // Whole-image premultiplied opacity: every colour is divided back.
    let file = file_with(opacity_defs(ChannelDef::TYPE_PREMULTIPLIED_OPACITY, &[0]));
    let img = decode(&file).unwrap();
    assert_eq!(img.format, PixelFormat::Rgba);
    let px = img.as_bytes().unwrap();
    for i in 0..8 {
        for (c, plane) in pre.iter().enumerate() {
            assert_eq!(
                px[i * 4 + c],
                straight_of(plane[i], alpha[i]),
                "pixel {i} c{c}"
            );
        }
        assert_eq!(px[i * 4 + 3], alpha[i]);
    }
    // Fully opaque pixels are exactly the stored colour.
    assert_eq!(&px[0..3], &[pre[0][0], pre[1][0], pre[2][0]]);
    // The depth API hands out the stored (premultiplied) samples.
    let stored = decode_jp2(&file).unwrap();
    for (component, plane) in stored.components.iter().zip(&pre) {
        let got: Vec<u8> = component.samples.iter().map(|&v| v as u8).collect();
        assert_eq!(&got, plane);
    }

    // Straight opacity (Typ = 1) is left alone.
    let img = decode(&file_with(opacity_defs(ChannelDef::TYPE_OPACITY, &[0]))).unwrap();
    let px = img.as_bytes().unwrap();
    for i in 0..8 {
        assert_eq!(&px[i * 4..i * 4 + 3], &[pre[0][i], pre[1][i], pre[2][i]]);
    }

    // Premultiplied into the colours it names only (here R and B).
    let file = file_with(opacity_defs(
        ChannelDef::TYPE_PREMULTIPLIED_OPACITY,
        &[1, 3],
    ));
    let px = decode(&file).unwrap().as_bytes().unwrap().to_vec();
    for i in 0..8 {
        assert_eq!(px[i * 4], straight_of(pre[0][i], alpha[i]));
        assert_eq!(px[i * 4 + 1], pre[1][i]);
        assert_eq!(px[i * 4 + 2], straight_of(pre[2][i], alpha[i]));
    }
}

#[test]
fn premultiplied_palette_expands_to_straight_rgba() {
    // A 4-column palette whose fourth column is premultiplied opacity
    // cannot ride the Pal8 table as stored: the file decodes to RGBA
    // with the division applied per pixel.
    let entries: [[u8; 4]; 3] = [[200, 100, 50, 255], [100, 50, 25, 128], [9, 9, 9, 0]];
    let index: Vec<u8> = vec![0, 1, 2, 1, 0, 2];
    let cs = encode(
        &Jpeg2000Image::new(
            3,
            2,
            PixelFormat::Gray8,
            vec![oxideav_jpeg2000::Plane::new(3, index.clone())],
        )
        .unwrap()
        .with_bit_depth(2)
        .unwrap(),
        &EncodeOptions::default()
            .with_container(Container::J2k)
            .with_decomposition_levels(0),
    )
    .unwrap();
    let column = |k: usize| PclrColumn {
        bit_depth: 8,
        signed: false,
        values: entries.iter().map(|e| i32::from(e[k])).collect(),
    };
    let build = |typ: u16| {
        let mut opts = Jp2WriteOptions::for_components(4)
            .with_palette(Some(Pclr {
                columns: (0..4).map(column).collect(),
            }))
            .with_component_mapping(Some(
                (0..4u8)
                    .map(|k| CmapEntry {
                        component: 0,
                        mapping: CmapMapping::Palette { column: k },
                    })
                    .collect(),
            ));
        opts.channel_definitions = Some(opacity_defs(typ, &[0]));
        write_jp2(&cs, &opts).unwrap()
    };
    let straight = decode(&build(ChannelDef::TYPE_OPACITY)).unwrap();
    assert_eq!(straight.format, PixelFormat::Pal8);
    let img = decode(&build(ChannelDef::TYPE_PREMULTIPLIED_OPACITY)).unwrap();
    assert_eq!(img.format, PixelFormat::Rgba);
    assert_eq!(
        info(&build(ChannelDef::TYPE_PREMULTIPLIED_OPACITY))
            .unwrap()
            .format,
        PixelFormat::Rgba
    );
    let px = img.as_bytes().unwrap();
    for (i, &ix) in index.iter().enumerate() {
        let e = entries[usize::from(ix)];
        let want = [
            straight_of(e[0], e[3]),
            straight_of(e[1], e[3]),
            straight_of(e[2], e[3]),
            e[3],
        ];
        assert_eq!(&px[i * 4..i * 4 + 4], &want, "pixel {i}");
    }
}
