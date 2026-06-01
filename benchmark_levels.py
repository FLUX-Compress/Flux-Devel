import os
import sys
import shutil
import time
import subprocess
import hashlib
import tarfile
import gzip
import pyzstd
import psutil
import threading

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

def build_real_world_corpus(corpus_dir, workspace_root):
    if os.path.exists(corpus_dir):
        shutil.rmtree(corpus_dir)
    os.makedirs(corpus_dir)

    # 1. Copy crates/ to corpus_dir/src
    src_dest = os.path.join(corpus_dir, 'src')
    shutil.copytree(os.path.join(workspace_root, 'crates'), src_dest)

    # 2. Copy docs
    docs_dest = os.path.join(corpus_dir, 'docs')
    os.makedirs(docs_dest)
    for doc in ['README.md', 'SPEC.md', 'CONTRIBUTING.md', 'LICENSE.md', 'LICENSE-COMMERCIAL.txt', 'LICENSE-GPL.txt']:
        p = os.path.join(workspace_root, doc)
        if os.path.exists(p):
            shutil.copy(p, docs_dest)

    # 3. Create repetitive log file (~10MB)
    logs_dest = os.path.join(corpus_dir, 'logs')
    os.makedirs(logs_dest)
    log_file = os.path.join(logs_dest, 'app.log')
    
    log_templates = [
        "2026-06-01 {:02d}:{:02d}:{:02d} [INFO] [flux::core] Processing block {}\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [DEBUG] [flux::lz77] Found match at distance {}, length {}\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [DEBUG] [flux::rans] Encoded 256 symbols in 1.2ms\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [INFO] [flux::core] Finished block {}, ratio {:.2f}x\n",
        "2026-06-01 {:02d}:{:02d}:{:02d} [WARN] [flux::sys] System memory usage high, available RAM: {} MB\n"
    ]
    
    target_size = 10 * 1024 * 1024 # 10MB
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

    # 4. Copy binary
    bin_dest = os.path.join(corpus_dir, 'bin')
    os.makedirs(bin_dest)
    cli_bin = os.path.join(workspace_root, 'target', 'release', 'flux-cli.exe')
    if os.path.exists(cli_bin):
        shutil.copy(cli_bin, bin_dest)

def monitor_process_memory(proc, peak_mem_container):
    try:
        p = psutil.Process(proc.pid)
        while proc.poll() is None:
            try:
                mem = p.memory_info().rss
                if mem > peak_mem_container[0]:
                    peak_mem_container[0] = mem
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                break
            time.sleep(0.005)
    except Exception:
        pass

def run_compress_flux(cli_path, input_path, archive_path, level):
    cmd = [cli_path, "compress", "-l", level, input_path, archive_path]
    peak_mem = [0]
    
    start_time = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    
    monitor_thread = threading.Thread(target=monitor_process_memory, args=(proc, peak_mem))
    monitor_thread.start()
    
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    monitor_thread.join()
    
    elapsed = end_time - start_time
    return elapsed, peak_mem[0], proc.returncode, stdout, stderr

def run_decompress_flux(cli_path, archive_path, output_dir):
    cmd = [cli_path, "decompress", archive_path, output_dir]
    start_time = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    return end_time - start_time, proc.returncode, stdout, stderr

def test_level_flux(cli_path, corpus_dir, out_dir, extract_dir, level):
    archive_name = f"corpus_{level}.flx"
    archive_path = os.path.join(out_dir, archive_name)
    if os.path.exists(archive_path):
        os.remove(archive_path)
        
    # Compress
    comp_time, peak_rss, code, stdout, stderr = run_compress_flux(cli_path, corpus_dir, archive_path, level)
    if code != 0:
        print(f"Compression failed for level {level}:")
        print(stderr.decode('utf-8', errors='ignore'))
        return None
        
    compressed_size = os.path.getsize(archive_path)
    
    # Decompress
    extracted_path = os.path.join(extract_dir, f"extracted_{level}")
    if os.path.exists(extracted_path):
        shutil.rmtree(extracted_path)
    os.makedirs(extracted_path)
    
    dec_time, dec_code, _, dec_stderr = run_decompress_flux(cli_path, archive_path, extracted_path)
    if dec_code != 0:
        print(f"Decompression failed for level {level}:")
        print(dec_stderr.decode('utf-8', errors='ignore'))
        return None
        
    return {
        'level': level,
        'size': compressed_size,
        'comp_time': comp_time,
        'dec_time': dec_time,
        'peak_rss': peak_rss,
        'extracted_path': extracted_path
    }

def verify_extracted(orig_hashes, extracted_dir):
    extracted_size, extracted_hashes = get_dir_size_and_hashes(extracted_dir)
    subfolders = [d for d in os.listdir(extracted_dir) if os.path.isdir(os.path.join(extracted_dir, d))]
    target_dir = extracted_dir
    if len(subfolders) == 1 and subfolders[0] == 'real_world_corpus':
        target_dir = os.path.join(extracted_dir, 'real_world_corpus')
        extracted_size, extracted_hashes = get_dir_size_and_hashes(target_dir)
        
    mismatches = []
    for rel, (sz, hsh) in orig_hashes.items():
        if rel not in extracted_hashes:
            mismatches.append(f"Missing: {rel}")
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

def main():
    root_dir = os.path.abspath(os.path.dirname(__file__))
    scratch_dir = os.path.join(root_dir, 'scratch')
    os.makedirs(scratch_dir, exist_ok=True)
    
    corpus_dir = os.path.join(scratch_dir, 'real_world_corpus')
    out_dir = os.path.join(scratch_dir, 'out')
    extract_dir = os.path.join(scratch_dir, 'extracted')
    
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(extract_dir, exist_ok=True)
    
    cli_path = os.path.join(root_dir, "target", "release", "flux-cli.exe")
    if not os.path.exists(cli_path):
        print("Error: flux-cli binary not found in target/release/flux-cli.exe")
        sys.exit(1)
        
    print("Building real-world benchmark corpus...")
    build_real_world_corpus(corpus_dir, root_dir)
    
    orig_size, orig_hashes = get_dir_size_and_hashes(corpus_dir)
    print(f"Real-world corpus built. Total size: {orig_size} bytes ({orig_size / (1024*1024):.2f} MB), {len(orig_hashes)} files.")
    
    levels = ['tiny', 'fast', 'balanced', 'maximum', 'extreme']
    results = {}
    
    for lvl in levels:
        print(f"Testing FLUX level: {lvl}...")
        res = test_level_flux(cli_path, corpus_dir, out_dir, extract_dir, lvl)
        if res:
            ok, errs = verify_extracted(orig_hashes, res['extracted_path'])
            res['verified'] = ok
            res['errors'] = errs
            results[lvl] = res
            print(f"  Ratio: {orig_size / res['size']:.2f}x, Time: {res['comp_time']:.2f}s, Peak RAM: {res['peak_rss'] / (1024*1024):.2f} MB, Verified: {ok}")
            if not ok:
                print(f"  Verification errors: {errs}")
                
    # Baselines: gzip -9 (Tar + Gzip)
    print("Running gzip baseline...")
    tar_gz_path = os.path.join(out_dir, "corpus.tar.gz")
    start_time = time.perf_counter()
    with tarfile.open(tar_gz_path, "w:gz", compresslevel=9) as tar:
        tar.add(corpus_dir, arcname="real_world_corpus")
    gz_time = time.perf_counter() - start_time
    gz_size = os.path.getsize(tar_gz_path)
    
    # Baselines: zstd -19 (Tar + Zstd)
    print("Running zstd baseline...")
    tar_path = os.path.join(out_dir, "corpus.tar")
    with tarfile.open(tar_path, "w") as tar:
        tar.add(corpus_dir, arcname="real_world_corpus")
    
    start_time = time.perf_counter()
    with open(tar_path, "rb") as f:
        tar_data = f.read()
    zstd_data = pyzstd.compress(tar_data, 19)
    zstd_time = time.perf_counter() - start_time
    
    tar_zst_path = os.path.join(out_dir, "corpus.tar.zst")
    with open(tar_zst_path, "wb") as f:
        f.write(zstd_data)
    zstd_size = len(zstd_data)
    
    if os.path.exists(tar_path):
        os.remove(tar_path)
        
    print("\n" + "="*75)
    print("REAL-WORLD CORPUS BENCHMARK TABLE")
    print("="*75)
    print(f"{'Level':<12} {'Window':<10} {'Ratio':<10} {'Time (s)':<10} {'Peak Memory (MB)':<18} {'Verified':<8}")
    print("-" * 75)
    
    window_sizes = {
        'tiny': '256 KB',
        'fast': '4 MB',
        'balanced': '32 MB',
        'maximum': '128 MB',
        'extreme': '256 MB'
    }
    
    for lvl in ['fast', 'balanced', 'maximum', 'extreme', 'tiny']:
        if lvl in results:
            res = results[lvl]
            ratio = orig_size / res['size']
            peak_mb = res['peak_rss'] / (1024*1024)
            verified_str = "Yes" if res['verified'] else "FAILED"
            print(f"{lvl.capitalize():<12} {window_sizes[lvl]:<10} {ratio:.3f}x      {res['comp_time']:.3f}s      {peak_mb:.1f} MB            {verified_str:<8}")
            
    print("-" * 75)
    print(f"{'tar.gz -9':<12} {'32 KB':<10} {orig_size / gz_size:.3f}x      {gz_time:.3f}s      N/A                Yes")
    print(f"{'tar.zst -19':<12} {'N/A':<10} {orig_size / zstd_size:.3f}x      {zstd_time:.3f}s      N/A                Yes")
    print("="*75)

if __name__ == '__main__':
    main()
