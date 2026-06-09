//! Command line interface for FLUX.
//!
//! Exposes archive compression, extraction, and listing capabilities
//! as a thin wrapper around the public safe `flux` API.

use clap::{Parser, Subcommand};
use flux::{Archive, Compression};
use std::path::PathBuf;

#[derive(Parser)]
#[clap(
    name = "flux-cli",
    version = "1.0.0",
    author = "FLUX Contributors",
    about = "FLUX Archiver Command Line Interface"
)]
struct Cli {
    #[clap(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compress a file or directory into a FLUX archive
    Compress {
        /// Input file or directory to compress
        input: PathBuf,
        /// Output archive file (.flx)
        output: PathBuf,
        /// Optional encryption password
        #[clap(short, long)]
        password: Option<String>,
        /// Compression level (tiny, fast, balanced, max, extreme)
        #[clap(short, long, default_value = "balanced")]
        level: String,
        /// Target volume size in bytes (0 = single volume)
        #[clap(short = 's', long, default_value = "0")]
        volume_size: u64,
        /// Force overwrite of existing sibling volume files
        #[clap(long)]
        force: bool,
    },
    /// Decompress a FLUX archive
    Decompress {
        /// Input archive file (.flx)
        input: PathBuf,
        /// Output destination directory
        output: PathBuf,
        /// Optional encryption password
        #[clap(short, long)]
        password: Option<String>,
    },
    /// List the contents of a FLUX archive
    List {
        /// Archive file to list (.flx)
        archive: PathBuf,
        /// Optional encryption password (required if archive is encrypted)
        #[clap(short, long)]
        password: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Compress {
            input,
            output,
            password,
            level,
            volume_size,
            force,
        } => {
            let comp_level = match level.to_lowercase().as_str() {
                "tiny" => Compression::Tiny,
                "fast" => Compression::Fast,
                "balanced" => Compression::Balanced,
                "max" | "maximum" => Compression::Maximum,
                "extreme" => Compression::Extreme,
                other => {
                    eprintln!(
                        "Error: Invalid compression level '{}'. Allowed levels: tiny, fast, balanced, max, extreme.",
                        other
                    );
                    std::process::exit(1);
                }
            };

            if force {
                for vol_num in 1..=999 {
                    let ext = format!("{:03}", vol_num);
                    let mut path = output.clone();
                    if let Some(existing_ext) = path.extension() {
                        if existing_ext == "flx" {
                            let filename = path.file_name().unwrap().to_string_lossy().into_owned();
                            path.set_file_name(format!("{}.{}", filename, ext));
                        } else {
                            let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            path.set_file_name(format!("{}.{}", filename, ext));
                        }
                    } else {
                        let filename = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        path.set_file_name(format!("{}.{}", filename, ext));
                    }
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }

            let mut builder = Archive::compress(&input)
                .output(&output)
                .level(comp_level)
                .volume_size(volume_size)
                .on_progress(|p| {
                    print!(
                        "\rProgress: {:.1}% — {} processed of {} [Current: {}]            ",
                        p.percent(),
                        p.bytes_processed(),
                        p.bytes_total(),
                        p.current_file()
                    );
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                });

            if let Some(ref pass) = password {
                builder = builder.password(pass);
            }

            println!(
                "Compressing '{}' to '{}'...",
                input.display(),
                output.display()
            );

            match builder.run() {
                Ok(stats) => {
                    println!("Compression completed successfully!");
                    println!("Files processed: {}", stats.files_processed());
                    println!("Original size:   {} bytes", stats.original_size());
                    println!("Compressed size: {} bytes", stats.compressed_size());
                    if stats.original_size() > 0 {
                        println!(
                            "Ratio:           {:.2}x (lower is better)",
                            stats.compression_ratio()
                        );
                    } else {
                        println!("Ratio:           1.00x");
                    }
                    println!("Time elapsed:    {} ms", stats.elapsed_ms());
                }
                Err(err) => {
                    eprintln!("Error during compression: {}", err);
                    std::process::exit(1);
                }
            }
        }
        Commands::Decompress {
            input,
            output,
            password,
        } => {
            let mut builder = Archive::extract(&input).output(&output).on_progress(|p| {
                print!(
                    "\rProgress: {:.1}% — {} processed of {} [Current: {}]            ",
                    p.percent(),
                    p.bytes_processed(),
                    p.bytes_total(),
                    p.current_file()
                );
                let _ = std::io::Write::flush(&mut std::io::stdout());
            });

            if let Some(ref pass) = password {
                builder = builder.password(pass);
            }

            println!(
                "Extracting '{}' to '{}'...",
                input.display(),
                output.display()
            );

            match builder.run() {
                Ok(stats) => {
                    println!("Extraction completed successfully!");
                    println!("Files extracted: {}", stats.files_extracted());
                    println!("Bytes written:   {} bytes", stats.bytes_written());
                    println!("Integrity verified: {}", stats.integrity_verified());
                    println!("Time elapsed:    {} ms", stats.elapsed_ms());
                }
                Err(err) => {
                    eprintln!("Error during decompression: {}", err);
                    std::process::exit(1);
                }
            }
        }
        Commands::List { archive, password } => {
            let mut builder = Archive::extract(&archive);

            if let Some(ref pass) = password {
                builder = builder.password(pass);
            }

            println!("Listing contents of archive '{}'...", archive.display());

            match builder.list_files() {
                Ok(files) => {
                    println!("\nFiles in archive (total: {}):", files.len());
                    for file in files {
                        println!("  {}", file);
                    }
                }
                Err(err) => {
                    eprintln!("Error listing archive contents: {}", err);
                    std::process::exit(1);
                }
            }
        }
    }
}
