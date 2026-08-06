//! `pz` - the Photonic Zero command line tool.
//!
//! Encodes files into optical frames, decodes captures back into files, and
//! reports what the format can carry.

mod ppm;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pz_core::render::{render, RenderOptions};
use pz_core::{
    ColorMode, Decoder, Encoder, EncoderConfig, FrameProfile, GridSize, Progress, RgbView,
};

const USAGE: &str = "\
pz - Photonic Zero: data over light, from a screen to a camera

USAGE
    pz <command> [options]

COMMANDS
    encode <input>    Encode a file into a sequence of optical frames
    decode <files>    Decode captured frames back into the original file
    info              Print what each profile can carry
    selftest          Run an end-to-end transfer through a simulated camera

ENCODE
    pz encode <input> [-o DIR] [options]
        Use - as <input> to read from standard input.

    -o, --out DIR        Output directory (default: pz-frames)
    -n, --frames N       Frames to write. Default is 1.5x the minimum plus a
                         few, which is enough for a receiver that misses a
                         third of them. The stream is endless, so more frames
                         only means more resilience.
    -p, --profile NAME   balanced | robust | fast | resilient
    -g, --grid N         33 | 49 | 65 | 81 | 97
    -m, --mode NAME      mono | rgb4 | rgb8
        --parity N       0-7, higher spends more of each frame on repair data
        --module-px N    Pixels per cell (default 8)
        --quiet N        Quiet zone in cells (default 4)
        --format FMT     png | ppm (default png)
        --session N      Pin the session id instead of deriving it

DECODE
    pz decode <files...> [-o FILE]

    -o, --out FILE       Write the payload here (default: standard output)

    Reads PPM, and PNGs produced by pz itself. For a capture from a camera or
    another tool, convert first:
        ffmpeg -i capture.mp4 frame%04d.ppm

EXAMPLES
    pz encode secret.txt -o frames --profile robust
    pz encode - -o frames --format ppm < message.bin
    pz decode frames/*.ppm -o recovered.bin
    pz info
    pz selftest
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    }

    let result = match args[0].as_str() {
        "encode" => cmd_encode(&args[1..]),
        "decode" => cmd_decode(&args[1..]),
        "info" => cmd_info(),
        "selftest" => cmd_selftest(),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "-V" | "--version" | "version" => {
            println!("pz {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("pz: {message}");
            ExitCode::FAILURE
        }
    }
}

/// A tiny flag parser. A real CLI framework would be the largest dependency in
/// the project, and this needs about twenty lines.
struct Args {
    positional: Vec<String>,
    flags: Vec<(String, String)>,
}

impl Args {
    fn parse(input: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let arg = &input[i];
            if let Some(name) = arg.strip_prefix("--") {
                if let Some((k, v)) = name.split_once('=') {
                    flags.push((k.to_string(), v.to_string()));
                } else {
                    let value = input
                        .get(i + 1)
                        .ok_or_else(|| format!("--{name} needs a value"))?;
                    flags.push((name.to_string(), value.clone()));
                    i += 1;
                }
            } else if arg.len() == 2 && arg.starts_with('-') && arg != "-" {
                let value = input
                    .get(i + 1)
                    .ok_or_else(|| format!("{arg} needs a value"))?;
                flags.push((arg[1..].to_string(), value.clone()));
                i += 1;
            } else {
                positional.push(arg.clone());
            }
            i += 1;
        }
        Ok(Self { flags, positional })
    }

    fn get(&self, short: &str, long: &str) -> Option<&str> {
        self.flags
            .iter()
            .find(|(k, _)| k == short || k == long)
            .map(|(_, v)| v.as_str())
    }

    fn parse_get<T: std::str::FromStr>(
        &self,
        short: &str,
        long: &str,
    ) -> Result<Option<T>, String> {
        match self.get(short, long) {
            None => Ok(None),
            Some(raw) => raw
                .parse()
                .map(Some)
                .map_err(|_| format!("--{long}: '{raw}' is not valid")),
        }
    }
}

fn config_from(args: &Args) -> Result<EncoderConfig, String> {
    let mut config = match args.get("p", "profile").unwrap_or("balanced") {
        "balanced" | "default" => EncoderConfig::default(),
        "robust" => EncoderConfig::robust(),
        "fast" => EncoderConfig::fast(),
        "resilient" => EncoderConfig::resilient(),
        other => {
            return Err(format!(
                "unknown profile '{other}' (balanced, robust, fast, resilient)"
            ))
        }
    };

    if let Some(grid) = args.parse_get::<usize>("g", "grid")? {
        config.grid = GridSize::from_modules(grid)
            .ok_or_else(|| format!("grid must be 33, 49, 65, 81 or 97, not {grid}"))?;
    }
    if let Some(mode) = args.get("m", "mode") {
        config.mode = match mode {
            "mono" => ColorMode::Mono,
            "rgb4" => ColorMode::Rgb4,
            "rgb8" => ColorMode::Rgb8,
            other => return Err(format!("unknown mode '{other}' (mono, rgb4, rgb8)")),
        };
    }
    if let Some(parity) = args.parse_get::<u8>("", "parity")? {
        if parity > 7 {
            return Err("parity must be 0-7".to_string());
        }
        config.parity_code = parity;
    }
    if let Some(session) = args.parse_get::<u16>("", "session")? {
        config.session_id = Some(session);
    }
    Ok(config)
}

fn cmd_encode(input: &[String]) -> Result<(), String> {
    let args = Args::parse(input)?;
    let source = args
        .positional
        .first()
        .ok_or("encode needs an input file (or - for standard input)")?;

    let payload = if source == "-" {
        let mut buffer = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buffer)
            .map_err(|e| format!("reading standard input: {e}"))?;
        buffer
    } else {
        fs::read(source).map_err(|e| format!("reading {source}: {e}"))?
    };

    if payload.is_empty() {
        return Err("input is empty".to_string());
    }

    let config = config_from(&args)?;
    let encoder = Encoder::new(&payload, config).map_err(|e| e.to_string())?;

    let out_dir = PathBuf::from(args.get("o", "out").unwrap_or("pz-frames"));
    fs::create_dir_all(&out_dir).map_err(|e| format!("creating {}: {e}", out_dir.display()))?;

    let module_px = args
        .parse_get::<usize>("", "module-px")?
        .unwrap_or(8)
        .max(1);
    let quiet_zone = args.parse_get::<usize>("", "quiet")?.unwrap_or(4);
    let format = args.get("", "format").unwrap_or("png").to_string();
    if format != "png" && format != "ppm" {
        return Err(format!("unknown format '{format}' (png or ppm)"));
    }

    // The stream is rateless, so "how many frames" is a resilience choice, not
    // a correctness one. Half again the minimum tolerates a receiver that
    // misses roughly a third of what it sees.
    let minimum = encoder.block_count();
    let count = args
        .parse_get::<usize>("n", "frames")?
        .unwrap_or_else(|| minimum + minimum / 2 + 4)
        .max(1);

    let options = RenderOptions {
        module_px,
        quiet_zone,
        background: [255, 255, 255],
        ink: None,
    };

    for index in 0..count {
        let frame = encoder.frame(index as u32).map_err(|e| e.to_string())?;
        let image = render(&frame, &options);
        let bytes = if format == "png" {
            pz_core::png::encode(&image)
        } else {
            ppm::write(&image)
        };
        let path = out_dir.join(format!("frame{index:05}.{format}"));
        fs::write(&path, &bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }

    let profile = encoder.profile();
    eprintln!("encoded {} bytes", payload.len());
    eprintln!(
        "  grid        {}x{} cells, {:?}, parity {}",
        encoder.layout().modules(),
        encoder.layout().modules(),
        profile.mode(),
        profile.parity_code()
    );
    eprintln!("  per frame   {} bytes", profile.droplet_size());
    eprintln!("  minimum     {minimum} frames");
    eprintln!("  written     {count} frames to {}", out_dir.display());
    eprintln!("  session     0x{:04X}", encoder.session_id());
    eprintln!(
        "  at 30 fps   about {:.1}s if the receiver catches 4 frames in 5",
        encoder.estimated_seconds(30.0, 0.8)
    );
    Ok(())
}

fn load_image(path: &Path) -> Result<pz_core::render::RgbImage, String> {
    let bytes = fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;

    if bytes.starts_with(b"P6") {
        return ppm::read(&bytes).map_err(|e| format!("{}: {e}", path.display()));
    }
    if bytes.starts_with(&[137, 80, 78, 71]) {
        return pz_core::png::decode(&bytes).map_err(|e| {
            format!(
                "{}: {e}\n       pz reads its own PNGs; convert others first, \
                 e.g. `ffmpeg -i in.png out.ppm`",
                path.display()
            )
        });
    }
    Err(format!(
        "{}: unrecognised image format (expected PPM or PNG)",
        path.display()
    ))
}

fn cmd_decode(input: &[String]) -> Result<(), String> {
    let args = Args::parse(input)?;
    if args.positional.is_empty() {
        return Err("decode needs at least one frame file".to_string());
    }

    let mut decoder = Decoder::new();
    let mut recovered: Option<Vec<u8>> = None;

    for path in &args.positional {
        let image = match load_image(Path::new(path)) {
            Ok(image) => image,
            Err(message) => {
                eprintln!("pz: skipping {message}");
                continue;
            }
        };

        let view = RgbView::rgb(image.width, image.height, &image.data)
            .ok_or_else(|| format!("{path}: image buffer is too small"))?;

        match decoder.ingest_image(&view).map_err(|e| e.to_string())? {
            Progress::Complete(bytes) => {
                recovered = Some(bytes);
                break;
            }
            Progress::Progressed {
                recovered: got,
                total,
                ..
            } => eprintln!("  {path}: {got}/{total} blocks"),
            Progress::NotFound => eprintln!("  {path}: no frame found"),
            Progress::Rejected => eprintln!("  {path}: frame unusable"),
        }
    }

    let payload = recovered.ok_or_else(|| {
        format!(
            "not enough frames: recovered {:.0}% after {} images",
            decoder.progress() * 100.0,
            decoder.frames_seen()
        )
    })?;

    eprintln!(
        "recovered {} bytes from {} of {} images",
        payload.len(),
        decoder.frames_accepted(),
        decoder.frames_seen()
    );

    match args.get("o", "out") {
        Some(path) => fs::write(path, &payload).map_err(|e| format!("writing {path}: {e}"))?,
        None => std::io::stdout()
            .write_all(&payload)
            .map_err(|e| format!("writing to standard output: {e}"))?,
    }
    Ok(())
}

fn cmd_info() -> Result<(), String> {
    println!("Photonic Zero capacity\n");
    println!(
        "{:<6} {:<6} {:>7} {:>7} {:>9} {:>12} {:>12}",
        "grid", "mode", "parity", "cells", "bytes/fr", "KB/s @30fps", "KB/s @60fps"
    );
    println!("{}", "-".repeat(68));

    for grid in GridSize::ALL {
        for mode in [ColorMode::Mono, ColorMode::Rgb4, ColorMode::Rgb8] {
            for parity_code in [1u8, 3, 5] {
                let Ok(profile) = FrameProfile::new(grid, mode, parity_code) else {
                    continue;
                };
                println!(
                    "{:<6} {:<6} {:>7} {:>7} {:>9} {:>12.1} {:>12.1}",
                    grid.modules(),
                    match mode {
                        ColorMode::Mono => "mono",
                        ColorMode::Rgb4 => "rgb4",
                        ColorMode::Rgb8 => "rgb8",
                    },
                    parity_code,
                    profile.data_cells(),
                    profile.droplet_size(),
                    profile.bytes_per_second(30.0) / 1024.0,
                    profile.bytes_per_second(60.0) / 1024.0,
                );
            }
        }
    }

    println!(
        "\nThroughput assumes every displayed frame is captured. A real camera\n\
         misses some; the fountain code absorbs that at a small cost in frames."
    );
    Ok(())
}

fn cmd_selftest() -> Result<(), String> {
    println!("Photonic Zero self test\n");

    let payload: Vec<u8> = (0..8192u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
        .collect();

    for (name, config) in [
        ("robust   ", EncoderConfig::robust()),
        ("balanced ", EncoderConfig::default()),
        ("resilient", EncoderConfig::resilient()),
        ("fast     ", EncoderConfig::fast()),
    ] {
        let encoder = Encoder::new(&payload, config).map_err(|e| e.to_string())?;
        let mut decoder = Decoder::new();

        let minimum = encoder.block_count();
        let mut index = 0u32;
        let mut offered = 0usize;

        let recovered = loop {
            // Drop one frame in four, as a hand-held camera would.
            if index % 4 != 3 {
                offered += 1;
                let frame = encoder.frame(index).map_err(|e| e.to_string())?;
                if let Progress::Complete(bytes) =
                    decoder.ingest_frame(&frame).map_err(|e| e.to_string())?
                {
                    break bytes;
                }
            }
            index += 1;
            if index > 200_000 {
                return Err(format!("{name}: never converged"));
            }
        };

        if recovered != payload {
            return Err(format!("{name}: recovered bytes do not match"));
        }

        let overhead = decoder.frames_accepted() as f64 / minimum as f64;
        println!(
            "  {name}  {:>3} cells  {:>4} B/frame  minimum {:>4} frames  \
             used {:>4} ({:.2}x)  offered {:>4}  OK",
            encoder.layout().modules(),
            encoder.profile().droplet_size(),
            minimum,
            decoder.frames_accepted(),
            overhead,
            offered,
        );
    }

    println!("\nall profiles round-tripped 8192 bytes with 25% frame loss");
    Ok(())
}
