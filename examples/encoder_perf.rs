//! Standalone lossless encoder benchmark:
//! `cargo run --release --no-default-features --example encoder_perf -- [width height]`.
//! Defaults to 512 x 512; generation, comparisons and decoding are not timed.

#![forbid(unsafe_code)]

use oxideav_jpeg2000::{decode, encode, EncodeOptions, Jpeg2000Image, PixelFormat};
use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

fn generated_image(width: u32, height: u32, bits: u8) -> Result<Jpeg2000Image, Box<dyn Error>> {
    let max = if bits == 8 { 255u32 } else { 65535u32 };
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
    let dimensions: Vec<u32> = std::env::args()
        .skip(1)
        .map(|argument| argument.parse())
        .collect::<Result<_, _>>()?;
    let (width, height) = match dimensions.as_slice() {
        [] => (512, 512),
        [width, height] if *width > 0 && *height > 0 => (*width, *height),
        _ => return Err(io::Error::other("use [positive width height]").into()),
    };
    if cfg!(debug_assertions) {
        return Err(io::Error::other("benchmark requires --release").into());
    }
    println!("Lossless JP2 RGB {width}x{height}; three encode rounds, validation outside timings");
    for bits in [8, 16] {
        let image = generated_image(width, height, bits)?;
        let options = EncodeOptions::new();
        let mut times = [Duration::ZERO; 3];
        let mut first: Option<Vec<u8>> = None;
        for (round, time) in times.iter_mut().enumerate() {
            let start = Instant::now();
            let bytes = encode(&image, &options)?;
            *time = start.elapsed();
            println!(
                "RGB{bits} round {}: {:.6}s; {} bytes",
                round + 1,
                time.as_secs_f64(),
                bytes.len()
            );
            if let Some(expected) = &first {
                if &bytes != expected {
                    return Err(io::Error::other("repeated encode changed the codestream").into());
                }
            } else {
                first = Some(bytes);
            }
        }
        let bytes = first.ok_or_else(|| io::Error::other("no encode rounds"))?;
        let output = decode(&bytes)?;
        if output.width != width
            || output.height != height
            || output.format != image.format
            || output.bit_depth != bits
            || output.planes.first().map(|p| &p.data) != image.planes.first().map(|p| &p.data)
        {
            return Err(io::Error::other("lossless decode changed native sample bytes").into());
        }
        times.sort_unstable();
        println!(
            "RGB{bits} median: {:.6}s; native samples exact",
            times[1].as_secs_f64()
        );
    }
    Ok(())
}
