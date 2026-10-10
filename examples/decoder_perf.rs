//! Standalone raw-codestream decoder benchmark:
//! `cargo run --release --no-default-features --example decoder_perf -- image.j2k [rounds]`.
//! Input I/O, warmup, sample hashing and output destruction are not timed.

#![forbid(unsafe_code)]

use oxideav_jpeg2000::{decode_j2k, DecodedImage};
use std::{error::Error, io, time::Instant};

fn sample_hash(image: &DecodedImage) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for component in &image.components {
        for value in &component.samples {
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    hash
}

fn main() -> Result<(), Box<dyn Error>> {
    if cfg!(debug_assertions) {
        return Err(io::Error::other("benchmark requires --release").into());
    }
    let mut arguments = std::env::args().skip(1);
    let path = arguments
        .next()
        .ok_or_else(|| io::Error::other("use image.j2k [positive rounds]"))?;
    let rounds = arguments
        .next()
        .map(|argument| argument.parse::<usize>())
        .transpose()?
        .unwrap_or(5);
    if rounds == 0 || arguments.next().is_some() {
        return Err(io::Error::other("use image.j2k [positive rounds]").into());
    }
    let bytes = std::fs::read(&path)?;
    let reference = {
        let image = decode_j2k(&bytes)?;
        (
            image.width,
            image.height,
            image.components.len(),
            sample_hash(&image),
        )
    };
    println!(
        "Raw J2K {}x{}, {} components; {} timed rounds after one warmup",
        reference.0, reference.1, reference.2, rounds
    );
    let mut times = Vec::with_capacity(rounds);
    for round in 1..=rounds {
        let start = Instant::now();
        let image = decode_j2k(&bytes)?;
        let elapsed = start.elapsed();
        let result = (
            image.width,
            image.height,
            image.components.len(),
            sample_hash(&image),
        );
        if result != reference {
            return Err(io::Error::other("repeated decode changed native samples").into());
        }
        println!(
            "round {round}: {:.6}s; planar i32LE FNV1a64 {:016x}",
            elapsed.as_secs_f64(),
            result.3
        );
        times.push(elapsed);
    }
    times.sort_unstable();
    let middle = rounds / 2;
    let median = if rounds % 2 == 0 {
        (times[middle - 1].as_secs_f64() + times[middle].as_secs_f64()) / 2.0
    } else {
        times[middle].as_secs_f64()
    };
    println!("median: {median:.6}s");
    Ok(())
}
