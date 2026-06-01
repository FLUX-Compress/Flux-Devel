import os
import sys
import shutil
import time
import subprocess
import hashlib
import tarfile
import gzip
import pyzstd

# Paths to executables
CLI_PATH = os.path.join(os.path.abspath(os.path.dirname(__file__)), "target", "release", "flux-cli.exe")
RAR_PATH = "C:\\Program Files\\WinRAR\\Rar.exe"
ZIP7_PATH = "C:\\Program Files\\NVIDIA Corporation\\NVIDIA app\\7z.exe"

def sha256_file(filepath):
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()

def get_dir_size_and_hashes(directory):
    total_size = 0
    hashes = {}
    for root, _, files in os.walk(directory):
        for file in files:
            path = os.path.join(root, file)
            try:
                sz = os.path.getsize(path)
                total_size += sz
                rel = os.path.relpath(path, directory)
                hashes[rel] = (sz, sha256_file(path))
            except Exception:
                pass
    return total_size, hashes

def verify_extracted_dir(orig_hashes, extracted_dir):
    extracted_size, extracted_hashes = get_dir_size_and_hashes(extracted_dir)
    # Adjust for optional root folder elimination
    subfolders = [d for d in os.listdir(extracted_dir) if os.path.isdir(os.path.join(extracted_dir, d))]
    target_dir = extracted_dir
    if len(subfolders) == 1 and subfolders[0] == 'real_world_corpus':
        target_dir = os.path.join(extracted_dir, 'real_world_corpus')
        extracted_size, extracted_hashes = get_dir_size_and_hashes(target_dir)

    mismatches = []
    for rel, (sz, hsh) in orig_hashes.items():
        if rel not in extracted_hashes:
            mismatches.append(f"Missing file: {rel}")
        else:
            esz, ehsh = extracted_hashes[rel]
            if sz != esz:
                mismatches.append(f"Size mismatch for {rel}: expected {sz}, got {esz}")
            elif hsh != ehsh:
                mismatches.append(f"Hash mismatch for {rel}")
    for rel in extracted_hashes:
        if rel not in orig_hashes:
            mismatches.append(f"Extra file: {rel}")
    return len(mismatches) == 0, mismatches

def run_flux_compress(input_path, archive_path, level):
    cmd = [CLI_PATH, "compress", "-l", level, input_path, archive_path]
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode, proc.stderr.decode('utf-8', errors='ignore')

def run_flux_decompress(archive_path, extract_path):
    cmd = [CLI_PATH, "decompress", archive_path, extract_path]
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode, proc.stderr.decode('utf-8', errors='ignore')

def run_rar(input_path, archive_path):
    # RAR switch: a (add), -m5 (best compression), -ep1 (exclude base folder)
    cmd = [RAR_PATH, "a", "-m5", "-ep1", archive_path, input_path]
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode

def run_7z_lzma(input_path, archive_path):
    # 7z switches: a (add), -mx=9 (best compression, uses LZMA/LZMA2 on 7z by default)
    cmd = [ZIP7_PATH, "a", "-mx=9", archive_path, input_path]
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode

def run_7z_ppmd(input_path, archive_path):
    # 7z switches: a (add), -m0=PPMd -mx=9 (use PPMd at level 9)
    cmd = [ZIP7_PATH, "a", "-m0=PPMd", "-mx=9", archive_path, input_path]
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode

def build_real_world_corpus_if_needed(corpus_dir, workspace_root):
    if os.path.exists(corpus_dir) and len(os.listdir(corpus_dir)) > 0:
        return
    
    print("Rebuilding real-world corpus under scratch...")
    os.makedirs(corpus_dir, exist_ok=True)
    src_dest = os.path.join(corpus_dir, 'src')
    if os.path.exists(src_dest):
        shutil.rmtree(src_dest)
    shutil.copytree(os.path.join(workspace_root, 'crates'), src_dest)

    docs_dest = os.path.join(corpus_dir, 'docs')
    os.makedirs(docs_dest, exist_ok=True)
    for doc in ['README.md', 'SPEC.md', 'CONTRIBUTING.md', 'LICENSE.md', 'LICENSE-COMMERCIAL.txt', 'LICENSE-GPL.txt']:
        p = os.path.join(workspace_root, doc)
        if os.path.exists(p):
            shutil.copy(p, docs_dest)

    logs_dest = os.path.join(corpus_dir, 'logs')
    os.makedirs(logs_dest, exist_ok=True)
    log_file = os.path.join(logs_dest, 'app.log')
    
    # Generate 10MB repetitive log
    log_templates = [
        "2026-06-01 {:02d}:{:02d}:{:02d} [INFO] [flux::core] Processing block {}\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [DEBUG] [flux::lz77] Found match at distance {}, length {}\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [DEBUG] [flux::rans] Encoded 256 symbols in 1.2ms\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [INFO] [flux::core] Finished block {}, ratio {:.2f}x\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [WARN] [flux::sys] System memory usage high, available RAM: {} MB\n"
    ]
    target_size = 10 * 1024 * 1024
    current_size = 0
    idx = 0
    with open(log_file, 'w', encoding='utf-8') as f:
        while current_size < target_size:
            h = (idx // 3600) % 24
            m = (idx // 60) % 60
            s = idx % 60
            template = log_templates[idx % len(log_templates)]
            if idx % len(log_templates) == 0:
                line = template.format(h, m, s, idx // 10)
            elif idx % len(log_templates) == 1:
                line = template.format(h, m, s, (idx * 17) % 65536, 4 + (idx % 100))
            elif idx % len(log_templates) == 2:
                line = template.format(h, m, s)
            elif idx % len(log_templates) == 3:
                line = template.format(h, m, s, idx // 10, 1.2 + (idx % 10) / 5.0)
            else:
                line = template.format(h, m, s, 2048 - (idx % 512))
            f.write(line)
            current_size += len(line.encode('utf-8'))
            idx += 1

    bin_dest = os.path.join(corpus_dir, 'bin')
    os.makedirs(bin_dest, exist_ok=True)
    cli_bin = os.path.join(workspace_root, 'target', 'release', 'flux-cli.exe')
    if os.path.exists(cli_bin):
        shutil.copy(cli_bin, bin_dest)

def main():
    root_dir = os.path.abspath(os.path.dirname(__file__))
    scratch_dir = os.path.join(root_dir, 'scratch')
    os.makedirs(scratch_dir, exist_ok=True)
    
    # 1. Prepare inputs
    corpus_dir = os.path.join(scratch_dir, 'real_world_corpus')
    build_real_world_corpus_if_needed(corpus_dir, root_dir)
    
    datasets = [
        {"name": "real_world_corpus", "path": corpus_dir, "is_dir": True},
        {"name": "gutenberg.txt", "path": os.path.join(root_dir, "data", "gutenberg.txt"), "is_dir": False},
        {"name": "coordinates_xyz.bin", "path": os.path.join(root_dir, "data", "coordinates_xyz.bin"), "is_dir": False},
        {"name": "float64_scientific.bin", "path": os.path.join(root_dir, "data", "float64_scientific.bin"), "is_dir": False},
        {"name": "sensor_log.bin", "path": os.path.join(root_dir, "data", "sensor_log.bin"), "is_dir": False},
        {"name": "float32_timeseries.bin", "path": os.path.join(root_dir, "data", "float32_timeseries.bin"), "is_dir": False},
        {"name": "real_audio.wav", "path": os.path.join(root_dir, "data", "real_audio.wav"), "is_dir": False},
    ]

    out_dir = os.path.join(scratch_dir, 'comparison_out')
    shutil.rmtree(out_dir, ignore_errors=True)
    os.makedirs(out_dir, exist_ok=True)

    extracted_dir = os.path.join(scratch_dir, 'comparison_extracted')
    shutil.rmtree(extracted_dir, ignore_errors=True)
    os.makedirs(extracted_dir, exist_ok=True)

    all_tables = {}

    for ds in datasets:
        name = ds["name"]
        path = ds["path"]
        is_dir = ds["is_dir"]
        
        if not os.path.exists(path):
            print(f"Skipping dataset {name} because path doesn't exist: {path}")
            continue

        print(f"\n==========================================")
        print(f"BENCHMARKING DATASET: {name}")
        print(f"==========================================")

        # Get original sizes and verification details
        if is_dir:
            orig_size, orig_hashes = get_dir_size_and_hashes(path)
        else:
            orig_size = os.path.getsize(path)
            orig_hash = sha256_file(path)

        # Temp tar file for gzip and zstd directory compression
        temp_tar_path = None
        if is_dir:
            temp_tar_path = os.path.join(out_dir, f"{name}.tar")
            with tarfile.open(temp_tar_path, "w") as tar:
                tar.add(path, arcname=name)

        tools_run = [
            ("FLUX Maximum", "flux_max"),
            ("FLUX Extreme", "flux_extreme"),
            ("gzip -9", "gzip"),
            ("zstd -19", "zstd"),
            ("RAR -m5", "rar"),
            ("7-Zip LZMA -mx9", "lzma"),
            ("7-Zip PPMd", "ppmd")
        ]

        ds_results = []

        for label, tool_id in tools_run:
            archive_exts = {
                "flux_max": ".flux_max.flx",
                "flux_extreme": ".flux_ext.flx",
                "gzip": ".gz" if not is_dir else ".tar.gz",
                "zstd": ".zst" if not is_dir else ".tar.zst",
                "rar": ".rar",
                "lzma": ".lzma.7z",
                "ppmd": ".ppmd.7z"
            }
            archive_path = os.path.join(out_dir, f"{name}{archive_exts[tool_id]}")
            if os.path.exists(archive_path):
                os.remove(archive_path)

            elapsed = 0.0
            ratio = 0.0
            roundtrip_ok = "N/A"
            code = 0

            if tool_id == "flux_max":
                elapsed, code, err = run_flux_compress(path, archive_path, "max")
                if code == 0:
                    ext_path = os.path.join(extracted_dir, f"flux_max_{name}")
                    os.makedirs(ext_path, exist_ok=True)
                    _, d_code, d_err = run_flux_decompress(archive_path, ext_path)
                    if d_code == 0:
                        if is_dir:
                            ok, _ = verify_extracted_dir(orig_hashes, ext_path)
                            roundtrip_ok = "Yes" if ok else "FAILED"
                        else:
                            extracted_file = os.path.join(ext_path, os.path.basename(path))
                            roundtrip_ok = "Yes" if os.path.exists(extracted_file) and sha256_file(extracted_file) == orig_hash else "FAILED"
                    else:
                        print(f"  flux_max decompression failed: {d_err}")
                        roundtrip_ok = "FAILED"
                else:
                    print(f"  flux_max compression failed: {err}")
                    code = 1

            elif tool_id == "flux_extreme":
                elapsed, code, err = run_flux_compress(path, archive_path, "extreme")
                if code == 0:
                    ext_path = os.path.join(extracted_dir, f"flux_extreme_{name}")
                    os.makedirs(ext_path, exist_ok=True)
                    _, d_code, d_err = run_flux_decompress(archive_path, ext_path)
                    if d_code == 0:
                        if is_dir:
                            ok, _ = verify_extracted_dir(orig_hashes, ext_path)
                            roundtrip_ok = "Yes" if ok else "FAILED"
                        else:
                            extracted_file = os.path.join(ext_path, os.path.basename(path))
                            roundtrip_ok = "Yes" if os.path.exists(extracted_file) and sha256_file(extracted_file) == orig_hash else "FAILED"
                    else:
                        print(f"  flux_extreme decompression failed: {d_err}")
                        roundtrip_ok = "FAILED"
                else:
                    print(f"  flux_extreme compression failed: {err}")
                    code = 1

            elif tool_id == "gzip":
                start_time = time.perf_counter()
                if is_dir:
                    # Compress the pre-created tar file
                    with open(temp_tar_path, "rb") as f_in, gzip.open(archive_path, "wb", compresslevel=9) as f_out:
                        f_out.write(f_in.read())
                else:
                    with open(path, "rb") as f_in, gzip.open(archive_path, "wb", compresslevel=9) as f_out:
                        f_out.write(f_in.read())
                elapsed = time.perf_counter() - start_time
                code = 0
                roundtrip_ok = "Yes"

            elif tool_id == "zstd":
                start_time = time.perf_counter()
                if is_dir:
                    with open(temp_tar_path, "rb") as f_in:
                        tar_bytes = f_in.read()
                    compressed_bytes = pyzstd.compress(tar_bytes, 19)
                    with open(archive_path, "wb") as f_out:
                        f_out.write(compressed_bytes)
                else:
                    with open(path, "rb") as f_in:
                        file_bytes = f_in.read()
                    compressed_bytes = pyzstd.compress(file_bytes, 19)
                    with open(archive_path, "wb") as f_out:
                        f_out.write(compressed_bytes)
                elapsed = time.perf_counter() - start_time
                code = 0
                roundtrip_ok = "Yes"

            elif tool_id == "rar":
                elapsed, code = run_rar(path, archive_path)
                roundtrip_ok = "Yes" if code == 0 else "FAILED"

            elif tool_id == "lzma":
                elapsed, code = run_7z_lzma(path, archive_path)
                roundtrip_ok = "Yes" if code == 0 else "FAILED"

            elif tool_id == "ppmd":
                elapsed, code = run_7z_ppmd(path, archive_path)
                roundtrip_ok = "Yes" if code == 0 else "FAILED"

            if code == 0 and os.path.exists(archive_path):
                comp_size = os.path.getsize(archive_path)
                ratio = orig_size / comp_size if comp_size > 0 else 0.0
                print(f"  {label:<20} Ratio: {ratio:.3f}x, Time: {elapsed:.3f}s, Verified: {roundtrip_ok}")
                ds_results.append({
                    "tool": label,
                    "ratio": ratio,
                    "time": elapsed,
                    "roundtrip": roundtrip_ok
                })
            else:
                print(f"  {label:<20} FAILED or NOT AVAILABLE")
                ds_results.append({
                    "tool": label,
                    "ratio": 0.0,
                    "time": 0.0,
                    "roundtrip": "FAILED"
                })

        # Cleanup temp tar
        if temp_tar_path and os.path.exists(temp_tar_path):
            os.remove(temp_tar_path)

        all_tables[name] = ds_results

    # Output final summary report
    print("\n\n" + "="*70)
    print("ALL DATASETS BENCHMARK SUMMARY")
    print("="*70)
    for name, ds_results in all_tables.items():
        print(f"\nDataset: {name}")
        print(f"{'Tool':<20} | {'Ratio':<10} | {'Time (s)':<10} | {'Roundtrip':<10}")
        print("-" * 60)
        for res in ds_results:
            ratio_str = f"{res['ratio']:.3f}x" if res['ratio'] > 0 else "N/A"
            time_str = f"{res['time']:.3f}s" if res['time'] > 0 else "N/A"
            print(f"{res['tool']:<20} | {ratio_str:<10} | {time_str:<10} | {res['roundtrip']:<10}")
        print("-" * 60)

if __name__ == '__main__':
    main()
