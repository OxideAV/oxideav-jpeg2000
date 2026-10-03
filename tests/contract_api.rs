//! The image-crate API contract surface (`IMAGE_CRATE_API`): `probe`,
//! `info`, `decode` / `decode_with` / `decode_rgb8` / `decode_rgba8` /
//! `decode_from`, `encode` / `encode_rgb8` / `encode_rgba8` /
//! `encode_to`, the `Jpeg2000Image` type and its layout derivation —
//! pinned against the committed black-box fixtures (samples must equal
//! the depth API's planes byte for byte) and lossless round trips over
//! every native layout.

use std::io::Cursor;

use oxideav_jpeg2000::{
    decode, decode_from, decode_j2k, decode_j2k_layers, decode_j2k_reduced, decode_rgb8,
    decode_rgba8, decode_with, encode, encode_rgb8, encode_rgba8, encode_to, info, probe,
    ColorInfo, ColorRange, Container, DecodeOptions, EncodeOptions, Error, Jpeg2000Image, Metadata,
    Palette, PixelFormat, Plane,
};

const GRAY_17X13_53: &[u8] = include_bytes!("data/gray-17x13-53.j2k");
const GRAY_U16_53: &[u8] = include_bytes!("data/gray-32x32-u16-53.j2k");
const GRAY_S8_53: &[u8] = include_bytes!("data/gray-32x32-s8-53.j2k");
const GRAY_S12_53: &[u8] = include_bytes!("data/gray-32x32-s12-53.j2k");
const GRAY_MULTILAYER_53: &[u8] = include_bytes!("data/gray-64x64-multilayer-53.j2k");
const RGB_RCT_53: &[u8] = include_bytes!("data/rgb-16x16-rct-53.j2k");
const RGB_SUB21_53: &[u8] = include_bytes!("data/rgb-95x48-sub21-53.j2k");
const RGB_CPRL_SUB3_53: &[u8] = include_bytes!("data/rgb-24x24-cprl-sub3-53.j2k");
const RGB_JP2: &[u8] = include_bytes!("data/rgb-48x32.jp2");
const BGR_CDEF_JP2: &[u8] = include_bytes!("data/bgr-cdef-16x16.jp2");
const PAL_JP2: &[u8] = include_bytes!("data/pal-32x24.jp2");
const HT_RGB24: &[u8] = include_bytes!("fixtures/ht_rgb24_rev.j2c");

const ALL_FIXTURES: &[&[u8]] = &[
    GRAY_17X13_53,
    GRAY_U16_53,
    GRAY_MULTILAYER_53,
    RGB_RCT_53,
    RGB_SUB21_53,
    RGB_CPRL_SUB3_53,
    RGB_JP2,
    BGR_CDEF_JP2,
    PAL_JP2,
    HT_RGB24,
];

/// The contract image's samples of channel `c` at `(x, y)` as the exact
/// integer, whatever the storage width.
fn sample(img: &Jpeg2000Image, c: usize, x: usize, y: usize) -> u32 {
    let f = img.format;
    let bps = f.bytes_per_sample();
    let (plane, at) = if f.is_yuv() {
        let (sx, sy) = if c == 1 || c == 2 {
            f.chroma_subsampling()
        } else {
            (1, 1)
        };
        let p = &img.planes[c];
        (
            p,
            (y / usize::from(sy)) * p.stride + (x / usize::from(sx)) * bps,
        )
    } else {
        let p = &img.planes[0];
        (p, y * p.stride + (x * f.components() + c) * bps)
    };
    if bps == 1 {
        u32::from(plane.data[at])
    } else {
        u32::from(u16::from_le_bytes([plane.data[at], plane.data[at + 1]]))
    }
}

/// Every sample of the contract image equals the depth API's plane
/// (after the same JP2 channel mapping) — the "decoded samples stay
/// byte-identical" gate.
fn assert_matches_depth_api(bytes: &[u8], img: &Jpeg2000Image) {
    let depth = if info(bytes).unwrap().jp2 {
        oxideav_jpeg2000::jp2::decode_jp2(bytes).unwrap()
    } else {
        decode_j2k(bytes).unwrap()
    };
    assert_eq!(depth.components.len(), img.components(), "channel count");
    for (c, comp) in depth.components.iter().enumerate() {
        let (pw, ph) = img.format.plane_dimensions(
            if img.format.is_yuv() { c } else { 0 },
            img.width,
            img.height,
        );
        assert_eq!((comp.width, comp.height), (pw, ph), "channel {c} geometry");
        for y in 0..ph as usize {
            for x in 0..pw as usize {
                let want = comp.samples[y * pw as usize + x];
                let got = if img.format.is_yuv() {
                    let p = &img.planes[c];
                    let bps = img.format.bytes_per_sample();
                    let at = y * p.stride + x * bps;
                    if bps == 1 {
                        u32::from(p.data[at])
                    } else {
                        u32::from(u16::from_le_bytes([p.data[at], p.data[at + 1]]))
                    }
                } else {
                    sample(img, c, x, y)
                };
                assert_eq!(got as i32, want, "channel {c} sample ({x}, {y})");
            }
        }
    }
}

#[test]
fn probe_accepts_both_framings_and_rejects_noise() {
    for f in ALL_FIXTURES {
        assert!(probe(f));
    }
    assert!(!probe(&[]));
    assert!(!probe(&[0xFF, 0x4F]));
    assert!(!probe(&[0xFF, 0xD8, 0xFF, 0xE0]));
    assert!(!probe(b"\x89PNG\r\n\x1a\n"));
    assert!(!probe(&RGB_JP2[..11]));
    assert!(
        probe(&RGB_JP2[..12]),
        "the signature box alone is a positive sniff"
    );
}

#[test]
fn info_describes_layouts_without_decoding() {
    let i = info(GRAY_17X13_53).unwrap();
    assert_eq!((i.width, i.height), (17, 13));
    assert_eq!(i.format, PixelFormat::Gray8);
    assert_eq!((i.frames, i.bit_depth, i.components), (1, 8, 1));
    assert!(!i.jp2 && !i.has_alpha && !i.has_icc && !i.has_exif && !i.has_xmp);
    assert!(i.reversible);
    assert_eq!(i.color, ColorInfo::unspecified());

    let i = info(GRAY_U16_53).unwrap();
    assert_eq!(i.format, PixelFormat::Gray16Le);
    assert_eq!(i.bit_depth, 16);

    let i = info(RGB_RCT_53).unwrap();
    assert_eq!(i.format, PixelFormat::Rgb24);
    assert_eq!(i.components, 3);

    let i = info(RGB_JP2).unwrap();
    assert!(i.jp2);
    assert_eq!(i.format, PixelFormat::Rgb24);
    assert_eq!(i.color, ColorInfo::srgb(), "enumerated sRGB colr");

    let i = info(BGR_CDEF_JP2).unwrap();
    assert_eq!(i.format, PixelFormat::Rgb24);

    let i = info(PAL_JP2).unwrap();
    assert_eq!(i.format, PixelFormat::Pal8);
    assert_eq!((i.width, i.height), (32, 24));
    assert_eq!(i.components, 1, "one index component");

    let i = info(RGB_SUB21_53).unwrap();
    let siz = oxideav_jpeg2000::parse_j2k_header(RGB_SUB21_53)
        .unwrap()
        .siz;
    assert!(siz
        .components
        .iter()
        .all(|c| (c.h_separation, c.v_separation) == (2, 1)));
    assert_eq!(
        i.format,
        PixelFormat::Rgb24,
        "uniform 2×1 sub-sampling is a narrower 1:1 picture"
    );
    assert_eq!((i.width, i.height), (48, 48));

    let i = info(RGB_CPRL_SUB3_53).unwrap();
    let siz = oxideav_jpeg2000::parse_j2k_header(RGB_CPRL_SUB3_53)
        .unwrap()
        .siz;
    let (xr, yr) = (
        siz.components[0].h_separation,
        siz.components[0].v_separation,
    );
    assert!(siz
        .components
        .iter()
        .all(|c| (c.h_separation, c.v_separation) == (xr, yr)));
    assert_eq!(
        i.format,
        PixelFormat::Rgb24,
        "uniform sub-sampling is a smaller 1:1 picture"
    );
    assert_eq!(
        (i.width, i.height),
        (
            siz.x_size.div_ceil(u32::from(xr)),
            siz.y_size.div_ceil(u32::from(yr))
        )
    );

    let i = info(HT_RGB24).unwrap();
    assert!(i.high_throughput);
    assert_eq!(i.format, PixelFormat::Rgb24);

    // Signed components have no contract layout.
    for f in [GRAY_S8_53, GRAY_S12_53] {
        assert!(matches!(info(f), Err(Error::Unsupported(_))));
        assert!(matches!(decode(f), Err(Error::Unsupported(_))));
        assert!(decode_j2k(f).is_ok(), "the depth API still decodes it");
    }
    assert!(matches!(info(&[]), Err(Error::MissingSoc)));
}

#[test]
fn decode_matches_the_depth_api_on_every_fixture() {
    for f in ALL_FIXTURES {
        let i = info(f).unwrap();
        let img = decode(f).unwrap();
        assert_eq!(
            (img.width, img.height, img.format),
            (i.width, i.height, i.format)
        );
        assert_eq!(img.bit_depth, i.bit_depth);
        assert_eq!(img.planes.len(), img.format.plane_count());
        img.validate().unwrap();
        if img.format == PixelFormat::Pal8 {
            // The palette path keeps the index plane; its expansion is
            // what the depth API returns.
            let depth = oxideav_jpeg2000::jp2::decode_jp2(f).unwrap();
            let rgb = img.to_rgb8();
            for (c, comp) in depth.components.iter().enumerate() {
                for (k, &s) in comp.samples.iter().enumerate() {
                    assert_eq!(
                        i32::from(rgb[k * 3 + c]),
                        s,
                        "palette channel {c} pixel {k}"
                    );
                }
            }
        } else {
            assert_matches_depth_api(f, &img);
        }
        // The raw paths agree with the native conversion.
        assert_eq!(decode_rgb8(f).unwrap().into_raw(), img.to_rgb8());
        assert_eq!(decode_rgba8(f).unwrap().into_raw(), img.to_rgba8());
        assert_eq!(decode_from(Cursor::new(*f)).unwrap(), img);
    }
}

#[test]
fn jp2_colour_and_palette_surface_on_the_image() {
    let img = decode(RGB_JP2).unwrap();
    assert_eq!(img.color, ColorInfo::srgb());
    assert!(img.metadata.icc.is_none());

    let pal = decode(PAL_JP2).unwrap();
    let table = pal.palette.as_ref().expect("palette");
    assert!(!table.is_empty() && table.len() <= 256);
    assert!(!pal.has_alpha());
    assert_eq!(pal.as_bytes().map(<[u8]>::len), Some(32 * 24));

    let sub = decode(RGB_SUB21_53).unwrap();
    assert_eq!(sub.format, PixelFormat::Rgb24);
    assert_eq!((sub.width, sub.height), (48, 48));
    assert_eq!(sub.color, ColorInfo::unspecified());
    assert_eq!(sub.to_rgba8().len(), 48 * 48 * 4);
}

#[test]
fn decode_options_limits_strict_reduce_and_layers() {
    // Limits fire before decoding, from the header geometry.
    let tight = DecodeOptions::default().with_max_width(16);
    assert!(matches!(
        decode_with(GRAY_17X13_53, &tight),
        Err(Error::LimitExceeded(_))
    ));
    let tight = DecodeOptions::default().with_max_pixels(100);
    assert!(matches!(
        decode_with(GRAY_17X13_53, &tight),
        Err(Error::LimitExceeded(_))
    ));
    let tight = DecodeOptions::default().with_max_bytes(17 * 13 * 4 - 1);
    assert!(matches!(
        decode_with(GRAY_17X13_53, &tight),
        Err(Error::LimitExceeded(_))
    ));
    let loose = DecodeOptions::default().with_max_bytes(17 * 13 * 4);
    assert!(decode_with(GRAY_17X13_53, &loose).is_ok());
    assert!(decode_with(GRAY_17X13_53, &DecodeOptions::default().unlimited()).is_ok());

    // Strict rejects trailing bytes; lenient ignores them.
    let mut trailing = GRAY_17X13_53.to_vec();
    trailing.extend_from_slice(&[0, 1, 2, 3]);
    assert!(decode(&trailing).is_ok());
    assert!(matches!(
        decode_with(&trailing, &DecodeOptions::default().with_strict(true)),
        Err(Error::InvalidData(_))
    ));
    assert!(decode_with(GRAY_17X13_53, &DecodeOptions::default().with_strict(true)).is_ok());
    assert!(decode_with(RGB_JP2, &DecodeOptions::default().with_strict(true)).is_ok());

    // Reduce and layers mirror the depth API exactly.
    let r1 = decode_with(GRAY_MULTILAYER_53, &DecodeOptions::default().with_reduce(1)).unwrap();
    let want = decode_j2k_reduced(GRAY_MULTILAYER_53, 1).unwrap();
    assert_eq!((r1.width, r1.height), (want.width, want.height));
    assert_eq!((r1.width, r1.height), (32, 32));
    let got: Vec<i32> = r1.planes[0].data.iter().map(|&v| i32::from(v)).collect();
    assert_eq!(got, want.components[0].samples);
    let l1 = decode_with(GRAY_MULTILAYER_53, &DecodeOptions::default().with_layers(1)).unwrap();
    let want = decode_j2k_layers(GRAY_MULTILAYER_53, 1).unwrap();
    let got: Vec<i32> = l1.planes[0].data.iter().map(|&v| i32::from(v)).collect();
    assert_eq!(got, want.components[0].samples);
    assert_ne!(
        l1,
        decode(GRAY_MULTILAYER_53).unwrap(),
        "one layer is a coarser picture"
    );
    assert!(matches!(
        decode_with(GRAY_MULTILAYER_53, &DecodeOptions::default().with_layers(0)),
        Err(Error::InvalidData(_))
    ));
    // The limit applies to the reduced geometry.
    let opts = DecodeOptions::default().with_reduce(1).with_max_width(32);
    assert!(decode_with(GRAY_MULTILAYER_53, &opts).is_ok());
}

fn plane16(samples: &[u16]) -> Vec<u8> {
    samples.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// Every native layout survives `decode(encode(img)) == img` through the
/// lossless kernel, in both containers (the bare codestream drops the
/// colour / palette / metadata it cannot carry).
#[test]
fn lossless_round_trip_every_layout() {
    let (w, h) = (7u32, 5u32);
    let n = (w * h) as usize;
    let g8: Vec<u8> = (0..n).map(|i| (i * 37 % 256) as u8).collect();
    let g4: Vec<u8> = (0..n).map(|i| (i * 7 % 16) as u8).collect();
    let g10: Vec<u16> = (0..n).map(|i| (i * 211 % 1024) as u16).collect();
    let g12: Vec<u16> = (0..n).map(|i| (i * 311 % 4096) as u16).collect();
    let g14: Vec<u16> = (0..n).map(|i| (i * 997 % 16384) as u16).collect();
    let g16: Vec<u16> = (0..n).map(|i| (i * 1999 % 65536) as u16).collect();
    let rgb: Vec<u8> = (0..n * 3).map(|i| (i * 53 % 256) as u8).collect();
    let rgba: Vec<u8> = (0..n * 4).map(|i| (i * 59 % 256) as u8).collect();
    let ya: Vec<u8> = (0..n * 2).map(|i| (i * 61 % 256) as u8).collect();
    let rgb16: Vec<u16> = (0..n * 3).map(|i| (i * 1777 % 65536) as u16).collect();
    let rgba16: Vec<u16> = (0..n * 4).map(|i| (i * 1783 % 65536) as u16).collect();
    let ya16: Vec<u16> = (0..n * 2).map(|i| (i * 1787 % 65536) as u16).collect();
    let chroma_w = w.div_ceil(2) as usize;
    let chroma_h = h.div_ceil(2) as usize;
    let cb420: Vec<u8> = (0..chroma_w * chroma_h)
        .map(|i| (90 + i * 13) as u8)
        .collect();
    let cb422: Vec<u8> = (0..chroma_w * h as usize)
        .map(|i| (70 + i * 11) as u8)
        .collect();
    let cb440: Vec<u8> = (0..w as usize * chroma_h)
        .map(|i| (60 + i * 9) as u8)
        .collect();
    let cb420_10: Vec<u16> = (0..chroma_w * chroma_h)
        .map(|i| (400 + i * 23) as u16)
        .collect();

    let packed = |f: PixelFormat, data: Vec<u8>| Jpeg2000Image::packed(w, h, f, data).unwrap();
    let mut images: Vec<Jpeg2000Image> = vec![
        packed(PixelFormat::Gray8, g8.clone()),
        packed(PixelFormat::Gray8, g4.clone())
            .with_bit_depth(4)
            .unwrap(),
        packed(PixelFormat::Gray10Le, plane16(&g10)),
        packed(PixelFormat::Gray12Le, plane16(&g12)),
        packed(PixelFormat::Gray16Le, plane16(&g14))
            .with_bit_depth(14)
            .unwrap(),
        packed(PixelFormat::Gray16Le, plane16(&g16)),
        packed(PixelFormat::Ya8, ya.clone()),
        packed(PixelFormat::Ya16Le, plane16(&ya16)),
        Jpeg2000Image::from_rgb8(w, h, rgb.clone()).unwrap(),
        Jpeg2000Image::from_rgba8(w, h, rgba.clone()).unwrap(),
        packed(PixelFormat::Rgb48Le, plane16(&rgb16)),
        packed(PixelFormat::Rgba64Le, plane16(&rgba16)),
        packed(PixelFormat::Pal8, g4.clone())
            .with_bit_depth(4)
            .unwrap()
            .with_palette(Palette::new(
                (0..16u8)
                    .map(|i| [i * 16, 255 - i * 16, i * 3, 255])
                    .collect(),
            )),
        packed(PixelFormat::Pal8, g8.clone()).with_palette(Palette::new(
            (0..=255u8)
                .map(|i| [i, i ^ 0x5A, 255 - i, i.wrapping_mul(3)])
                .collect(),
        )),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuv420P,
            vec![
                Plane::new(w as usize, g8.clone()),
                Plane::new(chroma_w, cb420.clone()),
                Plane::new(chroma_w, cb420.iter().rev().copied().collect()),
            ],
        )
        .unwrap(),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuv422P,
            vec![
                Plane::new(w as usize, g8.clone()),
                Plane::new(chroma_w, cb422.clone()),
                Plane::new(chroma_w, cb422.iter().rev().copied().collect()),
            ],
        )
        .unwrap(),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuv440P,
            vec![
                Plane::new(w as usize, g8.clone()),
                Plane::new(w as usize, cb440.clone()),
                Plane::new(w as usize, cb440.iter().rev().copied().collect()),
            ],
        )
        .unwrap(),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuv444P,
            vec![
                Plane::new(w as usize, g8.clone()),
                Plane::new(w as usize, g8.iter().rev().copied().collect()),
                Plane::new(w as usize, g8.iter().map(|v| v ^ 0x3C).collect()),
            ],
        )
        .unwrap(),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuva420P,
            vec![
                Plane::new(w as usize, g8.clone()),
                Plane::new(chroma_w, cb420.clone()),
                Plane::new(chroma_w, cb420.iter().rev().copied().collect()),
                Plane::new(w as usize, g8.iter().rev().copied().collect()),
            ],
        )
        .unwrap(),
        Jpeg2000Image::new(
            w,
            h,
            PixelFormat::Yuv420P10Le,
            vec![
                Plane::new(w as usize * 2, plane16(&g10)),
                Plane::new(chroma_w * 2, plane16(&cb420_10)),
                Plane::new(
                    chroma_w * 2,
                    plane16(&cb420_10.iter().rev().copied().collect::<Vec<_>>()),
                ),
            ],
        )
        .unwrap(),
    ];
    // Padded strides are accepted on input and come back tight.
    let padded = Jpeg2000Image::new(
        w,
        h,
        PixelFormat::Gray8,
        vec![Plane::new(
            w as usize + 3,
            (0..h as usize)
                .flat_map(|y| {
                    let mut row = g8[y * w as usize..(y + 1) * w as usize].to_vec();
                    row.extend_from_slice(&[0xEE; 3]);
                    row
                })
                .collect(),
        )],
    )
    .unwrap();
    images.push(padded);

    let opts = EncodeOptions::default();
    for img in &images {
        let f = img.format;
        // JP2 keeps everything.
        let jp2 = encode(img, &opts).unwrap_or_else(|e| panic!("{f:?}: encode jp2: {e}"));
        assert!(probe(&jp2));
        let i = info(&jp2).unwrap_or_else(|e| panic!("{f:?}: info: {e}"));
        assert!(i.jp2);
        assert_eq!(i.format, f, "{f:?}: header-only layout");
        assert_eq!(i.bit_depth, img.bit_depth, "{f:?}: header-only depth");
        let back = decode(&jp2).unwrap_or_else(|e| panic!("{f:?}: decode jp2: {e}"));
        let mut want = img.clone();
        // The JP2 header stamps the conventional colourspace on the way
        // out, and strides come back tight.
        want.color = if f.is_yuv() {
            ColorInfo::sycc()
        } else {
            ColorInfo::srgb()
        };
        for (k, p) in want.planes.iter_mut().enumerate() {
            let (pw, ph) = f.plane_dimensions(k, w, h);
            let row = pw as usize * f.bytes_per_pixel();
            if p.stride != row {
                let mut tight = Vec::with_capacity(row * ph as usize);
                for y in 0..ph as usize {
                    tight.extend_from_slice(&p.data[y * p.stride..y * p.stride + row]);
                }
                *p = Plane::new(row, tight);
            }
        }
        assert_eq!(back, want, "{f:?}: JP2 round trip");
        assert_eq!(back.to_rgba8(), img.to_rgba8(), "{f:?}: RGBA view");

        // The bare codestream keeps the samples, not the colour / palette.
        let j2k_opts = EncodeOptions::default().with_container(Container::J2k);
        if f == PixelFormat::Pal8 {
            assert!(matches!(encode(img, &j2k_opts), Err(Error::Unsupported(_))));
            continue;
        }
        let j2k = encode(img, &j2k_opts).unwrap_or_else(|e| panic!("{f:?}: encode j2k: {e}"));
        assert_eq!(&j2k[..2], &[0xFF, 0x4F]);
        let back = decode(&j2k).unwrap_or_else(|e| panic!("{f:?}: decode j2k: {e}"));
        let mut want_raw = want.clone();
        want_raw.color = ColorInfo::unspecified();
        if f == PixelFormat::Yuv444P {
            // Without a colr box three 1:1 components read as RGB; the
            // samples are untouched.
            assert_eq!(back.format, PixelFormat::Rgb24);
            assert_eq!(back.components(), 3);
            for c in 0..3 {
                for y in 0..h as usize {
                    for x in 0..w as usize {
                        assert_eq!(sample(&back, c, x, y), sample(img, c, x, y));
                    }
                }
            }
            continue;
        }
        assert_eq!(back, want_raw, "{f:?}: J2K round trip");
    }
}

#[test]
fn encode_derives_mct_and_sub_sampling_from_the_layout() {
    let (w, h) = (8u32, 8u32);
    let rgb: Vec<u8> = (0..(w * h * 3) as usize)
        .map(|i| (i * 29 % 256) as u8)
        .collect();
    let header = |bytes: &[u8]| {
        let c = oxideav_jpeg2000::jp2::parse_jp2(bytes).unwrap();
        oxideav_jpeg2000::parse_j2k_header(
            &bytes[c.codestream_offset..c.codestream_offset + c.codestream_len],
        )
        .unwrap()
    };
    // RGB: MCT on by default, off on request.
    let auto = encode_rgb8(w, h, &rgb, &EncodeOptions::default()).unwrap();
    assert_eq!(header(&auto).cod.multi_component_transform, 1);
    let off = encode_rgb8(w, h, &rgb, &EncodeOptions::default().with_mct(false)).unwrap();
    assert_eq!(header(&off).cod.multi_component_transform, 0);
    assert_eq!(decode_rgb8(&off).unwrap().into_raw(), rgb);
    // Gray: never.
    let gray = encode(
        &Jpeg2000Image::packed(w, h, PixelFormat::Gray8, rgb[..64].to_vec()).unwrap(),
        &EncodeOptions::default().with_mct(true),
    )
    .unwrap();
    assert_eq!(header(&gray).cod.multi_component_transform, 0);
    // 4:2:0: SIZ sub-sampling, no MCT, sYCC colr, decodes planar.
    let yuv = Jpeg2000Image::new(
        w,
        h,
        PixelFormat::Yuv420P,
        vec![
            Plane::new(8, rgb[..64].to_vec()),
            Plane::new(4, rgb[64..80].to_vec()),
            Plane::new(4, rgb[80..96].to_vec()),
        ],
    )
    .unwrap();
    let file = encode(&yuv, &EncodeOptions::default().with_mct(true)).unwrap();
    let hdr = header(&file);
    assert_eq!(hdr.cod.multi_component_transform, 0);
    assert_eq!(
        (
            hdr.siz.components[1].h_separation,
            hdr.siz.components[1].v_separation
        ),
        (2, 2)
    );
    let back = decode(&file).unwrap();
    assert_eq!(back.format, PixelFormat::Yuv420P);
    assert_eq!(back.color, ColorInfo::sycc());
    assert_eq!(back.planes, yuv.planes);
    // RGBA: the fourth component is declared as opacity.
    let rgba: Vec<u8> = (0..(w * h * 4) as usize)
        .map(|i| (i * 31 % 256) as u8)
        .collect();
    let file = encode_rgba8(w, h, &rgba, &EncodeOptions::default()).unwrap();
    let c = oxideav_jpeg2000::jp2::parse_jp2(&file).unwrap();
    let defs = c.header.cdef.expect("cdef");
    assert!(defs.iter().any(
        |d| d.channel == 3 && d.channel_type == oxideav_jpeg2000::jp2::ChannelDef::TYPE_OPACITY
    ));
    let back = decode(&file).unwrap();
    assert_eq!(back.format, PixelFormat::Rgba);
    assert!(back.has_alpha() && info(&file).unwrap().has_alpha);
    assert_eq!(back.as_bytes(), Some(&rgba[..]));
}

#[test]
fn colour_and_icc_travel_through_the_jp2_header() {
    let (w, h) = (4u32, 4u32);
    let data: Vec<u8> = (0..48).map(|i| (i * 5) as u8).collect();
    // An ICC profile wins the colr box and comes back on metadata.
    let icc = vec![0u8, 0, 0, 0x80, b'I', b'C', b'C', 0xAA, 1, 2, 3, 4];
    let img = Jpeg2000Image::from_rgb8(w, h, data.clone())
        .unwrap()
        .with_metadata(Metadata::new().with_icc(icc.clone()));
    let file = encode(&img, &EncodeOptions::default()).unwrap();
    let i = info(&file).unwrap();
    assert!(i.has_icc);
    assert_eq!(
        i.color,
        ColorInfo::unspecified(),
        "ICC-only colr: code points unspecified"
    );
    let back = decode(&file).unwrap();
    assert_eq!(back.metadata.icc.as_deref(), Some(&icc[..]));
    assert_eq!(back.as_bytes(), Some(&data[..]));

    // A non-enumerable colour description is flagged as an
    // approximation (UnkC) around the conventional sRGB box in a JP2…
    let bt2020 = ColorInfo::new(ColorRange::Full, 9, 16, 0);
    let img = Jpeg2000Image::from_rgb8(w, h, data.clone())
        .unwrap()
        .with_color(bt2020);
    let file = encode(&img, &EncodeOptions::default()).unwrap();
    let c = oxideav_jpeg2000::jp2::parse_jp2(&file).unwrap();
    assert_eq!(c.header.ihdr.colourspace_unknown, 1);
    assert_eq!(decode(&file).unwrap().color, ColorInfo::srgb());
    // …and written verbatim as a T.814 parameterized box in a JPH file.
    let file = encode(&img, &EncodeOptions::default().with_high_throughput(true)).unwrap();
    let c = oxideav_jpeg2000::jp2::parse_jp2(&file).unwrap();
    assert!(c.ftyp.is_jph_compatible());
    let back = decode(&file).unwrap();
    assert_eq!(back.color, bt2020);
    assert_eq!(back.as_bytes(), Some(&data[..]));
}

#[test]
fn lossy_kernel_rate_control_and_encode_to() {
    let (w, h) = (32u32, 24u32);
    let rgb: Vec<u8> = (0..(w * h) as usize)
        .flat_map(|i| {
            let (x, y) = ((i % w as usize) as u8, (i / w as usize) as u8);
            [x * 5, y * 7, x.wrapping_add(y) * 3]
        })
        .collect();
    let lossless = encode_rgb8(w, h, &rgb, &EncodeOptions::default()).unwrap();
    let lossy = encode_rgb8(
        w,
        h,
        &rgb,
        &EncodeOptions::default()
            .with_lossy(3)
            .with_layers(2)
            .with_target_psnr(38.0),
    )
    .unwrap();
    assert!(lossy.len() < lossless.len());
    let i = info(&lossy).unwrap();
    assert!(!i.reversible);
    assert_eq!(i.layers, 2);
    let back = decode_rgb8(&lossy).unwrap().into_raw();
    let mse = back
        .iter()
        .zip(&rgb)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum::<f64>()
        / rgb.len() as f64;
    assert!(10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10() > 36.0);
    let budget = encode_rgb8(w, h, &rgb, &EncodeOptions::default().with_target_bytes(400)).unwrap();
    assert!(
        budget.len() <= 400 + 200,
        "{} (JP2 boxes on top of the codestream budget)",
        budget.len()
    );

    let img = Jpeg2000Image::from_rgb8(w, h, rgb).unwrap();
    let mut buf = Vec::new();
    encode_to(&img, &EncodeOptions::default(), &mut buf).unwrap();
    assert_eq!(buf, encode(&img, &EncodeOptions::default()).unwrap());
    assert_eq!(
        decode_from(Cursor::new(&buf)).unwrap(),
        decode(&buf).unwrap()
    );
}

#[test]
fn errors_are_typed_and_io_wraps() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("nope"))
        }
    }
    let e = decode_from(Broken).unwrap_err();
    assert!(matches!(e, Error::Io(_)));
    assert!(std::error::Error::source(&e).is_some());
    let e: Error = std::io::Error::other("x").into();
    assert!(e.to_string().contains("io error"));
    assert!(matches!(
        Jpeg2000Image::from_rgb8(2, 2, vec![0; 11]),
        Err(Error::InvalidData(_))
    ));
    assert!(matches!(
        Jpeg2000Image::packed(0, 1, PixelFormat::Gray8, vec![]),
        Err(Error::InvalidData(_))
    ));
    assert!(matches!(
        encode(
            &Jpeg2000Image::packed(1, 1, PixelFormat::Pal8, vec![0]).unwrap(),
            &EncodeOptions::default()
        ),
        Err(Error::InvalidData(_))
    ));
    assert!(matches!(
        encode_rgb8(0, 0, &[], &EncodeOptions::default()),
        Err(Error::InvalidData(_))
    ));
}

#[test]
fn hostile_prefixes_and_flips_never_panic() {
    for f in ALL_FIXTURES {
        for cut in (0..f.len()).step_by((f.len() / 40).max(1)) {
            let _ = probe(&f[..cut]);
            let _ = info(&f[..cut]);
            let _ = decode(&f[..cut]);
        }
        let mut flipped = f.to_vec();
        for k in (0..flipped.len()).step_by((flipped.len() / 16).max(1)) {
            flipped[k] ^= 0x55;
            let _ = info(&flipped);
            let _ = decode(&flipped);
            flipped[k] ^= 0x55;
        }
    }
}
