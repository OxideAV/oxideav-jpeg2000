//! Output-pinning tool for optimisation work:
//!
//! ```text
//! cargo run --release --example corpus_hash -- [--threads N] [--dump DIR] [DIR|FILE]...
//! ```
//!
//! Decodes every file under the given paths (recursively, sorted) three
//! ways — full, one resolution level discarded, first quality layer
//! only — and prints an FNV-1a hash of the decoded planes or the error
//! text; then encodes a matrix of small synthetic images across the
//! encoder's option space and prints each codestream's hash. Diffing
//! the output of two builds (or two thread budgets) shows whether a
//! change kept decoded samples and emitted bytes identical. `--dump DIR`
//! also writes every matrix codestream to `DIR` (for a black-box
//! decoder to cross-check).

#![forbid(unsafe_code)]

use oxideav_jpeg2000::encode::{ComponentOverride, PackedHeaders, RoiRegion, TilePartSplit};
use oxideav_jpeg2000::{
    Container, DecodeOptions, EncodeKernel, EncodeOptions, Jpeg2000Image, PixelFormat,
    ProgressionOrder, decode_with, encode,
};
use std::{error::Error, path::PathBuf};

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
}

fn describe(result: Result<Jpeg2000Image, oxideav_jpeg2000::Error>) -> String {
    match result {
        Ok(image) => {
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for plane in &image.planes {
                fnv(&mut hash, &plane.data);
            }
            format!(
                "{}x{} {:?}/{} {hash:016x}",
                image.width, image.height, image.format, image.bit_depth
            )
        }
        Err(error) => format!("ERR {error}"),
    }
}

fn collect(path: PathBuf, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        for entry in std::fs::read_dir(&path)? {
            collect(entry?.path(), out)?;
        }
    } else {
        out.push(path);
    }
    Ok(())
}

fn synthetic(width: u32, height: u32, format: PixelFormat, seed: u32) -> Jpeg2000Image {
    let wide = matches!(
        format,
        PixelFormat::Rgb48Le | PixelFormat::Gray16Le | PixelFormat::Rgba64Le
    );
    let channels = format.components();
    let max = if wide { 65_535i64 } else { 255 };
    let mut random = seed;
    let mut data = Vec::new();
    for y in 0..i64::from(height) {
        for x in 0..i64::from(width) {
            for c in 0..channels as i64 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = i64::from((random >> 9) % 23) - 11;
                let wave = ((x * (3 + c) + y * 5) % 97 - 48).abs() * max / 60;
                let edge = if (x / 19 + y / 13 + c) % 3 == 0 {
                    max / 3
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
    Jpeg2000Image::packed(width, height, format, data).expect("synthetic image is well-formed")
}

fn encode_matrix(threads: usize, dump: Option<&std::path::Path>) {
    let base = || {
        EncodeOptions::new()
            .with_container(Container::J2k)
            .with_threads(threads)
    };
    let lossy = |bits| base().with_lossy(bits);
    let mut variants: Vec<(&str, EncodeOptions)> = vec![
        ("default-jp2", EncodeOptions::new().with_threads(threads)),
        ("lossless", base()),
        ("lossless-nl0", base().with_decomposition_levels(0)),
        ("lossless-nl5", base().with_decomposition_levels(5)),
        ("lossless-cb16", base().with_code_block_exp((4, 4))),
        ("lossless-cb4x1024", base().with_code_block_exp((2, 10))),
        ("lossless-nomct", base().with_mct(false)),
        ("lossless-layers5", base().with_layers(5)),
        ("lossless-tiles", base().with_tile_size((48, 40))),
        (
            "lossless-rpcl-prec",
            base()
                .with_progression(ProgressionOrder::Rpcl)
                .with_precincts(vec![0x44, 0x55, 0x55, 0x66]),
        ),
        (
            "lossless-pcrl",
            base().with_progression(ProgressionOrder::Pcrl),
        ),
        (
            "lossless-cprl",
            base().with_progression(ProgressionOrder::Cprl),
        ),
        (
            "lossless-rlcp",
            base().with_progression(ProgressionOrder::Rlcp),
        ),
        (
            "lossless-markers",
            base()
                .with_sop(true)
                .with_eph(true)
                .with_plt(true)
                .with_tlm(true)
                .with_comment("pin".to_string()),
        ),
        ("lossless-rate", base().with_target_bytes(6000)),
        (
            "lossless-rate-layers",
            base().with_layers(4).with_target_bytes(9000),
        ),
        ("lossless-psnr", base().with_target_psnr(34.0)),
        ("lossy0", lossy(0)),
        ("lossy2", lossy(2)),
        ("lossy6", lossy(6)),
        ("lossy2-rate", lossy(2).with_target_bytes(5000)),
        (
            "lossy2-rate-layers-tiles",
            lossy(2)
                .with_layers(3)
                .with_target_bytes(8000)
                .with_tile_size((64, 64)),
        ),
        ("lossy2-psnr", lossy(2).with_target_psnr(38.0)),
        ("lossy2-nomct", lossy(2).with_mct(false)),
        ("plm", base().with_plm(true)),
        (
            "plm-plt-tlm-tiles",
            base()
                .with_plm(true)
                .with_plt(true)
                .with_tlm(true)
                .with_tile_size((40, 56)),
        ),
        ("ht", base().with_high_throughput(true)),
        ("ht-lossy", lossy(3).with_high_throughput(true)),
        (
            "ht-layers",
            base().with_high_throughput(true).with_layers(3),
        ),
        (
            "ht-tiles",
            base().with_high_throughput(true).with_tile_size((50, 50)),
        ),
    ];
    let mut styled = |name: &'static str, edit: &dyn Fn(&mut EncodeOptions)| {
        let mut options = base();
        edit(&mut options);
        variants.push((name, options));
    };
    styled("bypass", &|o| o.bypass = true);
    styled("termall", &|o| o.terminate_all = true);
    styled("bypass-termall", &|o| {
        o.bypass = true;
        o.terminate_all = true;
    });
    styled("reset", &|o| o.reset_probabilities = true);
    styled("causal", &|o| o.vertically_causal = true);
    styled("pterm", &|o| o.predictable_termination = true);
    styled("segsym", &|o| o.segmentation_symbols = true);
    styled("all-styles-layers-rate", &|o| {
        o.bypass = true;
        o.terminate_all = true;
        o.reset_probabilities = true;
        o.vertically_causal = true;
        o.predictable_termination = true;
        o.segmentation_symbols = true;
        o.layers = 3;
        o.target_bytes = Some(9000);
    });
    styled("bypass-layers", &|o| {
        o.bypass = true;
        o.layers = 4;
    });
    styled("ht-refinement", &|o| {
        o.high_throughput = true;
        o.ht_refinement = true;
    });
    styled("ht-mixed", &|o| o.ht_mixed = true);
    styled("roi", &|o| {
        o.roi = Some(RoiRegion {
            x0: 20,
            y0: 10,
            x1: 70,
            y1: 60,
        });
    });
    styled("roi-lossy", &|o| {
        o.kernel = EncodeKernel::Lossy9x7 { fine_bits: 2 };
        o.roi = Some(RoiRegion {
            x0: 5,
            y0: 30,
            x1: 90,
            y1: 80,
        });
    });
    styled("tileparts-r", &|o| {
        o.tile_parts = TilePartSplit::ByResolution;
        o.tile_size = Some((64, 64));
    });
    styled("ppm", &|o| o.packed_headers = PackedHeaders::Ppm);
    styled("ppt", &|o| o.packed_headers = PackedHeaders::Ppt);
    styled("coc", &|o| {
        o.mct = Some(false);
        o.component_overrides = vec![
            ComponentOverride::new(1)
                .with_decomposition_levels(Some(1))
                .with_code_block_exp(Some((3, 5))),
        ];
    });
    let images = [
        ("rgb8-127x93", synthetic(127, 93, PixelFormat::Rgb24, 7)),
        ("rgb16-65x130", synthetic(65, 130, PixelFormat::Rgb48Le, 11)),
        ("gray8-200x33", synthetic(200, 33, PixelFormat::Gray8, 13)),
        ("gray16-70x70", synthetic(70, 70, PixelFormat::Gray16Le, 17)),
        ("rgba8-64x64", synthetic(64, 64, PixelFormat::Rgba, 19)),
        ("rgb8-1x1", synthetic(1, 1, PixelFormat::Rgb24, 23)),
        ("rgb8-3x257", synthetic(3, 257, PixelFormat::Rgb24, 29)),
    ];
    let decode_options = DecodeOptions::new().with_threads(threads);
    for (image_name, image) in &images {
        for (name, options) in &variants {
            match encode(image, options) {
                Ok(bytes) => {
                    if let Some(directory) = dump {
                        let extension = if *name == "default-jp2" { "jp2" } else { "j2k" };
                        let path = directory.join(format!("{image_name}_{name}.{extension}"));
                        if let Err(error) = std::fs::write(&path, &bytes) {
                            println!("dump {}: {error}", path.display());
                        }
                    }
                    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
                    fnv(&mut hash, &bytes);
                    println!(
                        "encode {image_name} {name}: {} bytes {hash:016x} -> {}",
                        bytes.len(),
                        describe(decode_with(&bytes, &decode_options))
                    );
                }
                Err(error) => println!("encode {image_name} {name}: ERR {error}"),
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut threads = 1usize;
    let mut roots = Vec::new();
    let mut dump: Option<PathBuf> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--threads" {
            threads = arguments.next().ok_or("--threads needs a value")?.parse()?;
        } else if argument == "--dump" {
            dump = Some(arguments.next().ok_or("--dump needs a value")?.into());
        } else {
            roots.push(PathBuf::from(argument));
        }
    }
    let mut files = Vec::new();
    for root in roots {
        collect(root, &mut files)?;
    }
    files.sort();
    let full = DecodeOptions::new().with_threads(threads);
    let reduced = full.clone().with_reduce(1);
    let first_layer = full.clone().with_layers(1);
    for path in files {
        let bytes = std::fs::read(&path)?;
        println!(
            "decode {}: {} | r1 {} | l1 {}",
            path.display(),
            describe(decode_with(&bytes, &full)),
            describe(decode_with(&bytes, &reduced)),
            describe(decode_with(&bytes, &first_layer)),
        );
    }
    encode_matrix(threads, dump.as_deref());
    Ok(())
}
