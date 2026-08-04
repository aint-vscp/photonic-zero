//! Emits the conformance test vectors quoted in RFC-0001.
//!
//! An independent implementation should reproduce every line of this output
//! exactly. Run with: cargo run -p pz-core --example vectors

use pz_core::color::{modulate, ColorMode};
use pz_core::frame::FrameProfile;
use pz_core::header::{FrameHeader, FLAG_CRC32_PREFIX, PROTOCOL_VERSION};
use pz_core::layout::{GridSize, Layout, Role};
use pz_core::{Encoder, EncoderConfig};
use pz_fec::{crc16, crc32};
use pz_fountain::{DegreeTable, SolitonParams, SplitMix64};

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    println!("# PZ conformance vectors (wire format version {PROTOCOL_VERSION})\n");

    // ---------------------------------------------------------------- CRC ---
    println!("## Checksums");
    println!(
        "crc16(\"123456789\")            = 0x{:04X}",
        crc16(b"123456789")
    );
    println!("crc16(\"\")                     = 0x{:04X}", crc16(b""));
    println!(
        "crc32(\"123456789\")            = 0x{:08X}",
        crc32(b"123456789")
    );
    println!("crc32(\"\")                     = 0x{:08X}", crc32(b""));
    println!(
        "crc32(\"photonic zero\")        = 0x{:08X}",
        crc32(b"photonic zero")
    );

    // --------------------------------------------------------------- PRNG ---
    println!("\n## SplitMix64");
    let mut rng = SplitMix64::new(0);
    print!("seed 0:      ");
    for _ in 0..4 {
        print!("{:016X} ", rng.next_u64());
    }
    println!();

    let mut rng = SplitMix64::for_frame(0x1234, 7);
    print!("session 0x1234 frame 7: ");
    for _ in 0..4 {
        print!("{:016X} ", rng.next_u64());
    }
    println!();

    // ------------------------------------------------------------- layout ---
    println!("\n## Layout cell counts");
    println!(
        "{:<6} {:>7} {:>7} {:>8} {:>7} {:>7}",
        "grid", "total", "fixed", "palette", "header", "data"
    );
    for grid in GridSize::ALL {
        let layout = Layout::new(grid);
        let n = layout.modules();
        let mut counts = [0usize; 4];
        for r in 0..n {
            for c in 0..n {
                counts[match layout.role(r, c) {
                    Role::Fixed => 0,
                    Role::Palette => 1,
                    Role::Header => 2,
                    Role::Data => 3,
                }] += 1;
            }
        }
        println!(
            "{:<6} {:>7} {:>7} {:>8} {:>7} {:>7}",
            n,
            n * n,
            counts[0],
            counts[1],
            counts[2],
            counts[3]
        );
    }

    // ------------------------------------------------------------- header ---
    println!("\n## Header encoding");
    let header = FrameHeader {
        version: PROTOCOL_VERSION,
        mode: ColorMode::Rgb8,
        grid: GridSize::G49,
        parity_code: 3,
        session_id: 0xBEEF,
        frame_index: 7,
        payload_len: 1000,
        flags: FLAG_CRC32_PREFIX,
    };
    println!("fields: version=1 mode=Rgb8(2) grid=G49(1) parity=3");
    println!("        session=0xBEEF frame=7 payload_len=1000 flags=0x01");
    println!("plain  (16 bytes): {}", hex(&header.encode()));
    println!(
        "RS(32,16)         : {}",
        hex(&header.encode_protected().unwrap())
    );

    // ----------------------------------------------------------- capacity ---
    println!("\n## Capacity derivation");
    println!(
        "{:<6} {:<6} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8}",
        "grid", "mode", "parity", "cells", "T", "blocks", "block_n", "block_k", "droplet"
    );
    for grid in GridSize::ALL {
        for mode in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
            for parity_code in [0u8, 3, 7] {
                let Ok(p) = FrameProfile::new(grid, mode, parity_code) else {
                    continue;
                };
                println!(
                    "{:<6} {:<6} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8} {:>8}",
                    grid.modules(),
                    match mode {
                        ColorMode::Mono => "mono",
                        ColorMode::Rgb4 => "rgb4",
                        ColorMode::Rgb8 => "rgb8",
                    },
                    parity_code,
                    p.data_cells(),
                    p.data_cells() * mode.bits_per_cell() / 8,
                    p.blocks(),
                    p.block_n(),
                    p.block_k(),
                    p.droplet_size(),
                );
            }
        }
    }

    // ------------------------------------------------------------ soliton ---
    println!("\n## Robust soliton (c = 0.1, delta = 0.05)");
    for k in [4usize, 16, 64] {
        let table = DegreeTable::new(k, SolitonParams::default());
        let cdf = table.cdf();
        let shown = cdf.len().min(6);
        print!("K={k:<4} cdf[0..{shown}] =");
        for c in &cdf[..shown] {
            print!(" {c:.9}");
        }
        println!();
    }

    println!("\n## Droplet plans (session 0x0001, K = 16)");
    let table = DegreeTable::new(16, SolitonParams::default());
    for frame in [0u32, 1, 15, 16, 17, 18, 40, 100] {
        println!("frame {frame:<4} -> {:?}", table.plan(1, frame));
    }

    // ---------------------------------------------------------- modulation --
    println!("\n## Colour modulation");
    println!("mode  value -> colour code (rgb bits, bit0 = red)");
    for value in 0..2u8 {
        println!(
            "mono  {value}     -> {:03b}",
            modulate(ColorMode::Mono, value)
        );
    }
    for value in 0..4u8 {
        println!(
            "rgb4  {value}     -> {:03b}",
            modulate(ColorMode::Rgb4, value)
        );
    }
    for value in 0..8u8 {
        println!(
            "rgb8  {value}     -> {:03b}",
            modulate(ColorMode::Rgb8, value)
        );
    }

    // ------------------------------------------------------- full session ---
    println!("\n## End-to-end session");
    let payload = b"photonic zero";
    let encoder = Encoder::new(payload, EncoderConfig::default()).unwrap();
    println!(
        "payload            = {:?}",
        core::str::from_utf8(payload).unwrap()
    );
    println!("crc32(payload)     = 0x{:08X}", crc32(payload));
    println!("container length   = {} bytes", payload.len() + 4);
    println!("derived session id = 0x{:04X}", encoder.session_id());
    println!(
        "droplet size       = {} bytes",
        encoder.profile().droplet_size()
    );
    println!("block count        = {}", encoder.block_count());
    let frame = encoder.frame(0).unwrap();
    println!("frame 0 modules    = {}", frame.modules());
    let dark = frame.cells().iter().filter(|&&c| c == 0).count();
    println!("frame 0 black cells= {dark}");
}
