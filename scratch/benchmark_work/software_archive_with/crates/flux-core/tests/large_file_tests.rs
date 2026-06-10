use flux_core_v1::{FluxCompressionLevel, FluxCompressor, FluxDecompressor, FluxOptions};
use std::fs;
use tempfile::tempdir;

fn run_large_file_test(size_bytes: usize) {
    let temp_src = tempdir().unwrap();
    let temp_dest = tempdir().unwrap();

    // Generate synthetic repeating patterns to test sliding window & lz77
    let mut data = Vec::with_capacity(size_bytes);
    for i in 0..size_bytes {
        data.push((i % 256) as u8);
    }

    let file_path = temp_src.path().join("large_file.bin");
    fs::write(&file_path, &data).unwrap();

    let archive_path = temp_src.path().join("archive.flx");
    let options = FluxOptions {
        level: FluxCompressionLevel::Balanced,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 0,
        volume_size: 0,
    };

    let mut compressor = FluxCompressor::new(options);
    let c_stats = compressor.compress_file(&file_path, &archive_path).unwrap();

    assert!(c_stats.original_size > 0);
    assert!(c_stats.compressed_size > 0);
    assert_eq!(c_stats.original_size, size_bytes as u64);

    let mut decompressor = FluxDecompressor::new(options);
    let d_stats = decompressor
        .decompress(&archive_path, temp_dest.path())
        .unwrap();

    assert_eq!(d_stats.files_extracted, 1);
    assert_eq!(d_stats.bytes_written, size_bytes as u64);
    assert!(d_stats.integrity_verified);

    let decompressed_data = fs::read(temp_dest.path().join("large_file.bin")).unwrap();
    assert_eq!(decompressed_data, data);
}

#[test]
fn test_compress_1mb() {
    run_large_file_test(1_000_000);
}

#[test]
fn test_compress_5mb() {
    run_large_file_test(5_000_000);
}

#[test]
fn test_compress_20mb() {
    run_large_file_test(20_000_000);
}

#[test]
fn test_compress_1mb_high_entropy() {
    use rand::{RngCore, SeedableRng};
    let temp_src = tempdir().unwrap();
    let temp_dest = tempdir().unwrap();

    let mut data = vec![0u8; 1_000_000];
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    rng.fill_bytes(&mut data);

    let file_path = temp_src.path().join("high_entropy.bin");
    fs::write(&file_path, &data).unwrap();

    let archive_path = temp_src.path().join("archive.flx");
    let options = FluxOptions {
        level: FluxCompressionLevel::Balanced,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 0,
        volume_size: 0,
    };

    let start = std::time::Instant::now();
    let mut compressor = FluxCompressor::new(options);
    let _c_stats = compressor.compress_file(&file_path, &archive_path).unwrap();
    let duration = start.elapsed();
    println!(
        "\n[BENCH] 1MB high-entropy compression time: {:?}",
        duration
    );

    let mut decompressor = FluxDecompressor::new(options);
    let _d_stats = decompressor
        .decompress(&archive_path, temp_dest.path())
        .unwrap();

    let decompressed_data = fs::read(temp_dest.path().join("high_entropy.bin")).unwrap();
    assert_eq!(decompressed_data, data);
}

#[test]
fn test_compress_binary_speed() {
    use rand::{Rng, SeedableRng};
    let temp_src = tempdir().unwrap();
    let temp_dest = tempdir().unwrap();

    // Generate 100KB of data with values 0..45 (entropy ~5.5, triggers BinaryPipeline)
    let mut data = vec![0u8; 100_000];
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    for val in data.iter_mut() {
        *val = rng.gen_range(0..45) as u8;
    }

    let file_path = temp_src.path().join("binary_test.bin");
    fs::write(&file_path, &data).unwrap();

    let archive_path = temp_src.path().join("archive.flx");
    let options = FluxOptions {
        level: FluxCompressionLevel::Balanced,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 0,
        volume_size: 0,
    };

    let start = std::time::Instant::now();
    let mut compressor = FluxCompressor::new(options);
    let _c_stats = compressor.compress_file(&file_path, &archive_path).unwrap();
    let duration = start.elapsed();
    println!("\n[BENCH] 100KB binary compression time: {:?}", duration);

    let mut decompressor = FluxDecompressor::new(options);
    let _d_stats = decompressor
        .decompress(&archive_path, temp_dest.path())
        .unwrap();

    let decompressed_data = fs::read(temp_dest.path().join("binary_test.bin")).unwrap();
    assert_eq!(decompressed_data, data);
}

#[test]
fn test_rounding_equivalence() {
    let mut diff_count = 0;
    for i in 0..10_000_000 {
        let p = i as f32 / 10_000_000.0;
        let r1 = (p * 4096.0).round() as u32;
        let r2 = (p * 4096.0 + 0.5) as u32;
        if r1 != r2 {
            diff_count += 1;
            if diff_count <= 10 {
                println!("Difference at p = {}: round() = {}, +0.5 = {}", p, r1, r2);
            }
        }
    }
    assert_eq!(diff_count, 0, "Rounding methods are not equivalent!");
}
