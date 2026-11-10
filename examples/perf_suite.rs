//! Encode + decode timing suite over deterministic synthetic images:
//!
//! ```text
//! cargo run --release --example perf_suite -- [--threads N] [--rounds N]
//!     [--case SUBSTRING] [--only enc|dec] [--registry] [--hash-only]
//! ```
//!
//! Cases: `rgb8-1024` and `rgb16-1024` lossless 5-3, `rgb8-512` lossy
//! 9-7 (plain and rate-controlled), `rgb8-4k` lossless, and an HTJ2K
//! `ht-rgb8-1024` lossless stream. Each case is encoded and decoded
//! `rounds` times after one warm-up; the median and the fastest round
//! (milliseconds) are printed together
//! with FNV-1a hashes of the codestream and of the decoded samples, so
//! two builds (or two thread budgets) can be compared for identical
//! output. Image synthesis, hashing and verification are not timed.
//!
//! `--registry` drives the same cases through the framework
//! `Encoder` / `Decoder` adapters (requires the default `registry`
//! feature); the thread budget is then handed over as an
//! `ExecutionContext`.

#![forbid(unsafe_code)]

use oxideav_jpeg2000::{
    Container, DecodeOptions, EncodeOptions, Jpeg2000Image, PixelFormat, decode_with, encode,
};
use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash = (*hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    fnv(&mut hash, bytes);
    hash
}

fn hash_image(image: &Jpeg2000Image) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for plane in &image.planes {
        fnv(&mut hash, &plane.data);
    }
    hash
}

/// A smooth two-axis ramp with low-frequency texture, soft edges and
/// mild pseudo-random grain: enough structure for every sub-band to
/// carry several significant bit-planes without being pure noise.
fn synthetic_rgb(width: u32, height: u32, bits: u8) -> Result<Jpeg2000Image, Box<dyn Error>> {
    let max = if bits == 8 { 255i64 } else { 65_535i64 };
    let mut data = Vec::with_capacity(width as usize * height as usize * 3 * usize::from(bits / 8));
    let grain = (max / 64).max(1);
    let mut random = 0x1047_a53c_u32;
    for y in 0..i64::from(height) {
        for x in 0..i64::from(width) {
            let ramp_x = x * max / i64::from(width.max(2) - 1);
            let ramp_y = y * max / i64::from(height.max(2) - 1);
            // Triangle-wave texture at two scales plus a hard-edged
            // checker every 96 samples.
            let tri = |v: i64, period: i64| ((v % period) - period / 2).abs() * 2 * max / period;
            let checker = if ((x / 96) + (y / 96)) % 2 == 0 {
                max / 6
            } else {
                0
            };
            let bases = [
                (ramp_x * 5 + tri(x + y, 61) * 2 + checker) / 8,
                (ramp_y * 5 + tri(x * 2 + y, 173) * 3) / 8,
                ((ramp_x + ramp_y) * 2 + tri(y, 37) * 3 + checker) / 8,
            ];
            for base in bases {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = i64::from((random >> 8) % (2 * grain as u32 + 1)) - grain;
                let value = (base + noise).clamp(0, max) as u16;
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

struct Case {
    name: &'static str,
    width: u32,
    height: u32,
    bits: u8,
    options: EncodeOptions,
    lossless: bool,
}

fn cases() -> Vec<Case> {
    let base = || EncodeOptions::new().with_container(Container::J2k);
    vec![
        Case {
            name: "rgb8-1024-lossless",
            width: 1024,
            height: 1024,
            bits: 8,
            options: base().with_decomposition_levels(5),
            lossless: true,
        },
        Case {
            name: "rgb16-1024-lossless",
            width: 1024,
            height: 1024,
            bits: 16,
            options: base().with_decomposition_levels(5),
            lossless: true,
        },
        Case {
            name: "rgb8-512-lossy97",
            width: 512,
            height: 512,
            bits: 8,
            options: base().with_decomposition_levels(5).with_lossy(2),
            lossless: false,
        },
        Case {
            name: "rgb8-512-lossy97-rate",
            width: 512,
            height: 512,
            bits: 8,
            options: base()
                .with_decomposition_levels(5)
                .with_lossy(2)
                .with_layers(4)
                .with_target_bytes(96 * 1024),
            lossless: false,
        },
        Case {
            name: "rgb8-4k-lossless",
            width: 3840,
            height: 2160,
            bits: 8,
            options: base().with_decomposition_levels(5),
            lossless: true,
        },
        Case {
            name: "ht-rgb8-1024-lossless",
            width: 1024,
            height: 1024,
            bits: 8,
            options: base()
                .with_decomposition_levels(5)
                .with_high_throughput(true),
            lossless: true,
        },
    ]
}

/// Milliseconds of the fastest round (the least disturbed one on a
/// busy host).
fn fastest(times: &[Duration]) -> f64 {
    times.iter().min().map_or(0.0, Duration::as_secs_f64) * 1e3
}

fn median(times: &mut [Duration]) -> f64 {
    times.sort_unstable();
    let middle = times.len() / 2;
    let seconds = if times.len().is_multiple_of(2) {
        (times[middle - 1].as_secs_f64() + times[middle].as_secs_f64()) / 2.0
    } else {
        times[middle].as_secs_f64()
    };
    seconds * 1e3
}

#[cfg(feature = "registry")]
mod registry_path {
    use super::*;
    use oxideav_core::{
        CodecId, CodecParameters, ExecutionContext, Frame, Packet, TimeBase, VideoFrame,
    };
    use oxideav_jpeg2000::{CODEC_ID_STR, make_decoder, make_encoder};

    pub fn params(case: &Case) -> CodecParameters {
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.width = Some(case.width);
        params.height = Some(case.height);
        params.pixel_format = Some(if case.bits == 8 {
            oxideav_core::PixelFormat::Rgb24
        } else {
            oxideav_core::PixelFormat::Rgb48Le
        });
        params
    }

    pub fn frame(image: &Jpeg2000Image) -> Frame {
        Frame::Video(VideoFrame::from(image))
    }

    pub fn encode(case: &Case, frame: &Frame, threads: usize) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut encoder = make_encoder(&params(case))?;
        encoder.set_execution_context(&ExecutionContext::with_threads(threads));
        encoder.send_frame(frame)?;
        Ok(encoder.receive_packet()?.data)
    }

    pub fn decode(
        case: &Case,
        packet: &Packet,
        threads: usize,
    ) -> Result<VideoFrame, Box<dyn Error>> {
        let mut decoder = make_decoder(&params(case))?;
        decoder.set_execution_context(&ExecutionContext::with_threads(threads));
        decoder.send_packet(packet)?;
        match decoder.receive_frame()? {
            Frame::Video(video) => Ok(video),
            _ => Err(io::Error::other("decoder returned a non-video frame").into()),
        }
    }

    pub fn packet(bytes: &[u8]) -> Packet {
        Packet::new(0, TimeBase::new(1, 25), bytes.to_vec())
    }

    pub fn hash(video: &VideoFrame) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for plane in &video.planes {
            fnv(&mut hash, &plane.data);
        }
        hash
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut threads = 1usize;
    let mut rounds = 5usize;
    let mut filter: Option<String> = None;
    let mut registry = false;
    let mut hash_only = false;
    let mut only: Option<String> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| io::Error::other(format!("{argument} needs a value")))
        };
        match argument.as_str() {
            "--threads" => threads = value()?.parse()?,
            "--rounds" => rounds = value()?.parse()?,
            "--case" => filter = Some(value()?),
            "--only" => only = Some(value()?),
            "--registry" => registry = true,
            "--hash-only" => hash_only = true,
            _ => {
                return Err(io::Error::other(
                    "use [--threads N] [--rounds N] [--case SUBSTRING] [--only enc|dec] \
                     [--registry] [--hash-only]",
                )
                .into());
            }
        }
    }
    if rounds == 0 || threads == 0 {
        return Err(io::Error::other("rounds and threads must be positive").into());
    }
    if cfg!(debug_assertions) && !hash_only {
        return Err(io::Error::other("timing requires --release (or pass --hash-only)").into());
    }
    if registry && !cfg!(feature = "registry") {
        return Err(io::Error::other("--registry needs the `registry` feature").into());
    }
    if hash_only {
        rounds = 1;
    }
    let (encode_rounds, decode_rounds) = match only.as_deref() {
        None => (rounds, rounds),
        Some("enc") => (rounds, 1),
        Some("dec") => (1, rounds),
        Some(_) => return Err(io::Error::other("--only takes enc or dec").into()),
    };
    println!(
        "path={} threads={threads} rounds={rounds}",
        if registry { "registry" } else { "contract" }
    );
    println!(
        "{:<24} {:>9} {:>9} {:>9} {:>9} {:>9}  {:>16} {:>16}",
        "case", "enc med", "enc min", "dec med", "dec min", "bytes", "stream fnv", "samples fnv"
    );
    for case in cases() {
        if filter.as_ref().is_some_and(|f| !case.name.contains(f)) {
            continue;
        }
        let image = synthetic_rgb(case.width, case.height, case.bits)?;
        let encode_options = case.options.clone().with_threads(threads);
        let decode_options = DecodeOptions::new().with_threads(threads);
        #[cfg(feature = "registry")]
        let frame = registry_path::frame(&image);

        let encode_once = || -> Result<Vec<u8>, Box<dyn Error>> {
            #[cfg(feature = "registry")]
            if registry {
                return registry_path::encode(&case, &frame, threads);
            }
            Ok(encode(&image, &encode_options)?)
        };
        let reference = encode_once()?;
        let mut encode_times = Vec::with_capacity(encode_rounds);
        for _ in 0..encode_rounds {
            let start = Instant::now();
            let bytes = encode_once()?;
            encode_times.push(start.elapsed());
            if bytes != reference {
                return Err(io::Error::other("repeated encode changed the codestream").into());
            }
        }

        #[cfg(feature = "registry")]
        let packet = registry_path::packet(&reference);
        // Returns the elapsed decode time and the hash of the decoded
        // samples; hashing happens after the clock stops.
        let decode_once = || -> Result<(Duration, u64), Box<dyn Error>> {
            #[cfg(feature = "registry")]
            if registry {
                let start = Instant::now();
                let video = registry_path::decode(&case, &packet, threads)?;
                let elapsed = start.elapsed();
                return Ok((elapsed, registry_path::hash(&video)));
            }
            let start = Instant::now();
            let decoded = decode_with(&reference, &decode_options)?;
            let elapsed = start.elapsed();
            if case.lossless
                && decoded.planes.first().map(|p| &p.data) != image.planes.first().map(|p| &p.data)
            {
                return Err(io::Error::other("lossless decode changed the samples").into());
            }
            Ok((elapsed, hash_image(&decoded)))
        };
        let (_, samples) = decode_once()?;
        let mut decode_times = Vec::with_capacity(decode_rounds);
        for _ in 0..decode_rounds {
            let (elapsed, hash) = decode_once()?;
            decode_times.push(elapsed);
            if hash != samples {
                return Err(io::Error::other("repeated decode changed the samples").into());
            }
        }
        println!(
            "{:<24} {:>9.2} {:>9.2} {:>9.2} {:>9.2} {:>9}  {:016x} {:016x}",
            case.name,
            median(&mut encode_times),
            fastest(&encode_times),
            median(&mut decode_times),
            fastest(&decode_times),
            reference.len(),
            hash_bytes(&reference),
            samples
        );
    }
    Ok(())
}
