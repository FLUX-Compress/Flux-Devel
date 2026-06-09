use flux_core_v1::{FluxCompressionLevel, FluxCompressor, FluxDecompressor, FluxOptions};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Helper to create a source directory with a set of random, incompressible files
fn create_test_files(src_dir: &Path, count: usize, size_each: usize) -> Vec<PathBuf> {
    use rand::{RngCore, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let mut files = Vec::new();
    for i in 1..=count {
        let file_path = src_dir.join(format!("file_{}.bin", i));
        let mut data = vec![0u8; size_each];
        rng.fill_bytes(&mut data);
        fs::write(&file_path, data).unwrap();
        files.push(file_path);
    }
    files
}

#[test]
fn test_single_volume_identical_to_v1_2() {
    let temp_src = tempdir().unwrap();
    let temp_out = tempdir().unwrap();

    let src_dir = temp_src.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    create_test_files(&src_dir, 3, 5000); // Small files

    // 1. Compress with volume_size = 0 (produces classic single-file FLUX archive)
    let archive_path_0 = temp_out.path().join("archive_0.flx");
    let options_0 = FluxOptions {
        level: FluxCompressionLevel::Balanced,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 0,
        volume_size: 0,
    };
    let mut compressor_0 = FluxCompressor::new(options_0);
    compressor_0
        .compress_directory(&src_dir, &archive_path_0)
        .unwrap();

    // 2. Compress with volume_size = 10_000_000 (Fits in one volume but volume_size > 0)
    let archive_path_large = temp_out.path().join("archive_large.flx");
    let options_large = FluxOptions {
        level: FluxCompressionLevel::Tiny,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 0,
        volume_size: 10_000_000,
    };
    let mut compressor_large = FluxCompressor::new(options_large);
    compressor_large
        .compress_directory(&src_dir, &archive_path_large)
        .unwrap();

    // 3. Verify files exist
    assert!(archive_path_0.exists(), "archive_0.flx must exist");
    assert!(!archive_path_large.exists(), "archive_large.flx must NOT exist directly");

    let archive_large_vol1 = temp_out.path().join("archive_large.flx.001");
    assert!(archive_large_vol1.exists(), "archive_large.flx.001 must exist");

    // 4. Verify magic bytes
    let bytes_0 = fs::read(&archive_path_0).unwrap();
    let bytes_large = fs::read(&archive_large_vol1).unwrap();
    assert_eq!(&bytes_0[0..4], b"FLUX", "archive_0.flx must start with FLUX magic");
    assert_eq!(&bytes_large[0..4], b"FLXV", "archive_large.flx.001 must start with FLXV magic");

    // 5. Verify decompressor can decompress the N=1 multi-volume archive
    let dest_dir = temp_out.path().join("dest");
    let mut decompressor = FluxDecompressor::new(options_large);
    let d_stats = decompressor
        .decompress(&archive_large_vol1, &dest_dir)
        .unwrap();
    assert_eq!(d_stats.files_extracted, 3);
    assert!(dest_dir.join("file_1.bin").exists());
}

#[test]
fn test_multi_volume_roundtrip() {
    let temp_src = tempdir().unwrap();
    let temp_out = tempdir().unwrap();
    let temp_dest = tempdir().unwrap();

    let src_dir = temp_src.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    // Create 4 files, each 200 KB (random, incompressible)
    create_test_files(&src_dir, 4, 200 * 1024);

    // Compress with tiny level (256 KB window/block size) and volume_size = 280 KB
    // Because each file is ~200 KB and random, each solid block will be ~200 KB compressed.
    // Together with the metadata, they will cross 280 KB and split into multiple volumes.
    let archive_path = temp_out.path().join("archive.flx");
    let options = FluxOptions {
        level: FluxCompressionLevel::Tiny,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 256 * 1024,
        volume_size: 280 * 1024, // 280 KB volume size
    };

    let mut compressor = FluxCompressor::new(options);
    let c_stats = compressor
        .compress_directory(&src_dir, &archive_path)
        .unwrap();
    assert!(c_stats.files_processed == 4);

    // Verify volumes are created: .001, .002, etc.
    let vol1 = temp_out.path().join("archive.flx.001");
    let vol2 = temp_out.path().join("archive.flx.002");
    assert!(vol1.exists(), "Volume 1 must exist");
    assert!(vol2.exists(), "Volume 2 must exist");
    assert!(
        !archive_path.exists(),
        "Base archive path should not exist as a file itself"
    );

    // Decompress using Volume 1
    let mut decompressor = FluxDecompressor::new(options);
    let d_stats = decompressor.decompress(&vol1, temp_dest.path()).unwrap();
    assert_eq!(d_stats.files_extracted, 4);

    // Verify all 4 files are intact
    for i in 1..=4 {
        let f_path = temp_dest.path().join(format!("file_{}.bin", i));
        assert!(f_path.exists());
        let original_data = fs::read(src_dir.join(format!("file_{}.bin", i))).unwrap();
        let extracted_data = fs::read(&f_path).unwrap();
        assert_eq!(original_data, extracted_data);
    }

    // Decompress using Volume 2 (should auto-locate Volume 1 and succeed)
    let temp_dest_2 = tempdir().unwrap();
    let d_stats_2 = decompressor.decompress(&vol2, temp_dest_2.path()).unwrap();
    assert_eq!(d_stats_2.files_extracted, 4);
    assert!(temp_dest_2.path().join("file_1.bin").exists());
}

#[test]
fn test_multi_volume_partial_loss() {
    let temp_src = tempdir().unwrap();
    let temp_out = tempdir().unwrap();
    let temp_dest = tempdir().unwrap();

    let src_dir = temp_src.path().join("src");
    fs::create_dir(&src_dir).unwrap();

    // Create files of distinct classifications so they are in separate blocks
    // file_a: Text (~50 KB uncompressed, highly compressible)
    let data_a = "A".repeat(50 * 1024);
    fs::write(src_dir.join("file_a.bin"), data_a).unwrap();

    // file_b: Multimedia (Wav signature + random bytes) -> ~500 KB
    use rand::{RngCore, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let mut data_b = vec![0u8; 500 * 1024];
    rng.fill_bytes(&mut data_b);
    data_b[0..12].copy_from_slice(b"RIFF\x00\x00\x00\x00WAVE");
    fs::write(src_dir.join("file_b.bin"), &data_b).unwrap();

    // file_c: Incompressible/Binary (random bytes) -> ~500 KB
    let mut data_c = vec![0u8; 500 * 1024];
    rng.fill_bytes(&mut data_c);
    fs::write(src_dir.join("file_c.bin"), &data_c).unwrap();

    // Target volume size is 300 KB, block size is 256 KB.
    // This will force file_a, file_b, file_c to be placed in separate volumes (Volume 1, 2, and 3).
    let archive_path = temp_out.path().join("archive.flx");
    let options = FluxOptions {
        level: FluxCompressionLevel::Tiny,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 256 * 1024,
        volume_size: 300 * 1024,
    };

    let mut compressor = FluxCompressor::new(options);
    compressor
        .compress_directory(&src_dir, &archive_path)
        .unwrap();

    let vol1 = temp_out.path().join("archive.flx.001");
    let vol2 = temp_out.path().join("archive.flx.002");
    let vol3 = temp_out.path().join("archive.flx.003");

    assert!(vol1.exists());
    assert!(vol2.exists());
    assert!(vol3.exists());

    // Verify list_files works using only Volume 1
    let mut decompressor = FluxDecompressor::new(options);
    let files = decompressor.list_files(&vol1).unwrap();
    assert!(files.contains(&"file_a.bin".to_string()));
    assert!(files.contains(&"file_b.bin".to_string()));
    assert!(files.contains(&"file_c.bin".to_string()));

    // Now, delete/rename Volume 2 to simulate partial loss
    let backup_vol2 = temp_out.path().join("archive.flx.002.bak");
    fs::rename(&vol2, &backup_vol2).unwrap();

    // Verify we can still list files from Volume 1
    let files_after_loss = decompressor.list_files(&vol1).unwrap();
    assert_eq!(files_after_loss.len(), 3);

    // Verify decompression attempt fails
    let res = decompressor.decompress(&vol1, temp_dest.path());
    println!("[DEBUG] Decompression result: {:?}", res);
    assert!(res.is_err());
    let err_msg = format!("{:?}", res.err().unwrap());
    assert!(
        err_msg.contains("is missing or corrupt"),
        "Error message should report missing volume"
    );

    // But check that surviving files were extracted!
    // Since file_a was in Volume 1 (healthy), it should be extracted successfully on disk.
    // file_b was in Volume 2 (missing), so it is not extracted.
    let file_a_dest = temp_dest.path().join("file_a.bin");
    let file_b_dest = temp_dest.path().join("file_b.bin");

    assert!(
        file_a_dest.exists(),
        "file_a should be recovered as Volume 1 was healthy"
    );
    assert!(
        !file_b_dest.exists(),
        "file_b recovery should fail as Volume 2 was missing"
    );

    // Restore volume 2 and delete volume 1.
    // Back index copy is in volume N (vol 3).
    // Let's verify we can still list files from Volume 3 when Volume 1 is missing.
    fs::rename(&backup_vol2, &vol2).unwrap();
    fs::remove_file(&vol1).unwrap();

    let files_from_vol_3 = decompressor.list_files(&vol3).unwrap();
    assert_eq!(
        files_from_vol_3.len(),
        3,
        "Should list files using Volume N's redundant index"
    );
}

#[test]
fn test_mismatched_archive_ids_rejected() {
    let temp_src = tempdir().unwrap();
    let temp_out = tempdir().unwrap();

    let src_dir_a = temp_src.path().join("src_a");
    let src_dir_b = temp_src.path().join("src_b");
    fs::create_dir(&src_dir_a).unwrap();
    fs::create_dir(&src_dir_b).unwrap();

    use rand::{RngCore, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let mut data_a = vec![0u8; 300 * 1024];
    rng.fill_bytes(&mut data_a);
    fs::write(src_dir_a.join("file.bin"), &data_a).unwrap();

    let mut data_b = vec![0u8; 300 * 1024];
    rng.fill_bytes(&mut data_b);
    fs::write(src_dir_b.join("file.bin"), &data_b).unwrap();

    let options = FluxOptions {
        level: FluxCompressionLevel::Tiny,
        password: std::ptr::null(),
        thread_count: 0,
        block_size: 256 * 1024,
        volume_size: 280 * 1024,
    };

    // Compress set A
    let archive_path_a = temp_out.path().join("archive_a.flx");
    let mut comp_a = FluxCompressor::new(options);
    comp_a
        .compress_directory(&src_dir_a, &archive_path_a)
        .unwrap();

    // Compress set B
    let archive_path_b = temp_out.path().join("archive_b.flx");
    let mut comp_b = FluxCompressor::new(options);
    comp_b
        .compress_directory(&src_dir_b, &archive_path_b)
        .unwrap();

    // Sibling volume files:
    // archive_a.flx.001, archive_a.flx.002
    // archive_b.flx.001, archive_b.flx.002
    let vol_a1 = temp_out.path().join("archive_a.flx.001");
    let vol_b2 = temp_out.path().join("archive_b.flx.002");

    // Copy archive_b.flx.002 over archive_a.flx.002
    let vol_a2 = temp_out.path().join("archive_a.flx.002");
    assert!(vol_a2.exists(), "archive_a.flx.002 must exist for test");
    fs::remove_file(&vol_a2).unwrap();
    fs::copy(&vol_b2, &vol_a2).unwrap();

    // Try decompressing archive_a.flx.001. It should detect mismatched archive IDs!
    let mut decomp = FluxDecompressor::new(options);
    let res = decomp.decompress(&vol_a1, temp_src.path());
    assert!(
        res.is_err(),
        "Should fail decompression due to mismatched archive IDs"
    );
}
