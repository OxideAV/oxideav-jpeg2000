//! Standalone backend benchmark: `cargo run --release --no-default-features
//! --example decoder_perf -- [width height] [--save]`.

#![forbid(unsafe_code)]

use oxideav_jpeg2000::{decode, encode, Container, EncodeOptions, Jpeg2000Image, PixelFormat};
use std::{error::Error, io, time::Instant};

fn generated_image(width: u32, height: u32, bits: u8) -> Result<Jpeg2000Image, Box<dyn Error>> {
    let max = if bits == 8 { 255 } else { 65535 };
    let length = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .and_then(|n| n.checked_mul(usize::from(bits / 8)))
        .ok_or_else(|| io::Error::other("image length overflow"))?;
    let mut data = Vec::new();
    data.try_reserve_exact(length)?;
    let amplitude = (max / 80).max(1);
    let mut random = 0x1047_a53c_u32;
    for y in 0..height {
        let gy = (u64::from(y) * u64::from(max) / u64::from((height - 1).max(1))) as u32;
        for x in 0..width {
            let gx = (u64::from(x) * u64::from(max) / u64::from((width - 1).max(1))) as u32;
            for base in [gx, gy, (gx + gy) / 2] {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = ((random >> 8) % (2 * amplitude + 1)) as i32 - amplitude as i32;
                let value = (base as i32 + noise).clamp(0, max as i32) as u16;
                if bits == 16 {
                    data.extend_from_slice(&value.to_le_bytes());
                } else {
                    data.push(value as u8);
                }
            }
        }
    }
    let format = if bits == 8 {
        PixelFormat::Rgb24
    } else {
        PixelFormat::Rgb48Le
    };
    Ok(Jpeg2000Image::packed(width, height, format, data)?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut dimensions = Vec::new();
    let mut save = false;
    for argument in std::env::args().skip(1) {
        if argument == "--save" {
            save = true;
        } else {
            dimensions.push(argument.parse::<u32>()?);
        }
    }
    let (width, height) = match dimensions.as_slice() {
        [] => (512, 512),
        [width, height] if *width > 0 && *height > 0 => (*width, *height),
        _ => return Err(io::Error::other("use [positive width height] [--save]").into()),
    };
    if cfg!(debug_assertions) {
        return Err(io::Error::other("benchmark requires --release").into());
    }
    println!("RGB {width}x{height}; generation and validation outside codec timings");
    for bits in [8, 16] {
        let image = generated_image(width, height, bits)?;
        let start = Instant::now();
        let bytes = encode(&image, &EncodeOptions::new().with_container(Container::Jp2))?;
        let encode_time = start.elapsed();
        let start = Instant::now();
        let output = decode(&bytes)?;
        let decode_time = start.elapsed();
        if output.width != width
            || output.height != height
            || output.format != image.format
            || output.bit_depth != bits
            || output.planes.first().map(|p| &p.data) != image.planes.first().map(|p| &p.data)
        {
            return Err(io::Error::other("lossless decode changed native sample bytes").into());
        }
        println!(
            "RGB{bits}: {} bytes; encode {:.6}s; decode {:.6}s; native samples exact",
            bytes.len(),
            encode_time.as_secs_f64(),
            decode_time.as_secs_f64()
        );
        if save {
            std::fs::write(format!("perf-{width}x{height}-rgb{bits}.jp2"), &bytes)?;
        }
    }
    Ok(())
}
