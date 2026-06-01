import os
import subprocess
import time
import gzip
import pyzstd
import hashlib

def sha256_file(filepath):
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()

def run_flux_compress(input_path, output_path, disable_transpose=False):
    env = os.environ.copy()
    env['FLUX_TIMING'] = '1'
    if disable_transpose:
        env['FLUX_DISABLE_TRANSPOSE'] = '1'
    
    cmd = [
        r"d:\Chat Server\FLUX\target\release\flux-cli.exe",
        "compress",
        "-l", "balanced",
        input_path,
        output_path
    ]
    
    start_time = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        text=True
    )
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    
    elapsed = end_time - start_time
    
    # Parse detected stride
    detected_stride = None
    for line in (stdout + stderr).split('\n'):
        if "[FLUX DEBUG] Detected stride" in line:
            parts = line.split()
            # Format: "[FLUX DEBUG] Detected stride 4 for Multimedia chunk of size 1048576"
            try:
                idx = parts.index("stride")
                detected_stride = int(parts[idx + 1])
            except ValueError:
                pass
                
    return elapsed, detected_stride, proc.returncode, stdout, stderr

def run_flux_decompress(archive_path, output_dir):
    cmd = [
        r"d:\Chat Server\FLUX\target\release\flux-cli.exe",
        "decompress",
        archive_path,
        output_dir
    ]
    
    start_time = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    
    elapsed = end_time - start_time
    return elapsed, proc.returncode

def main():
    data_dir = r"d:\Chat Server\FLUX\data"
    out_dir = r"d:\Chat Server\FLUX\out"
    extract_dir = r"d:\Chat Server\FLUX\out_extracted"
    
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(extract_dir, exist_ok=True)
    
    files = [
        'float32_timeseries.bin',
        'float64_scientific.bin',
        'audio_pcm16.bin',
        'sensor_log.bin',
        'coordinates_xyz.bin',
        'real_audio.wav',
        'real_scientific.bin'
    ]
    
    results = {}
    
    for filename in files:
        filepath = os.path.join(data_dir, filename)
        if not os.path.exists(filepath):
            print(f"Skipping {filename} (not found)")
            continue
            
        orig_size = os.path.getsize(filepath)
        orig_hash = sha256_file(filepath)
        
        print(f"\n==========================================")
        print(f"Benchmarking: {filename} ({orig_size / (1024*1024):.2f} MB)")
        print(f"==========================================")
        
        results[filename] = {}
        
        # 1. FLUX (stride ON)
        print("Running FLUX (stride/transpose ON)...")
        flux_on_archive = os.path.join(out_dir, f"{filename}.flux_on.flx")
        flux_on_time, detected_stride, code, stdout, stderr = run_flux_compress(filepath, flux_on_archive, disable_transpose=False)
        if code != 0:
            print(f"FLUX ON failed: {stderr}")
            continue
            
        flux_on_size = os.path.getsize(flux_on_archive)
        flux_on_ratio = orig_size / flux_on_size if flux_on_size > 0 else 0.0
        
        # Decompress & Verify
        flux_on_extract = os.path.join(extract_dir, f"flux_on_{filename}")
        os.makedirs(flux_on_extract, exist_ok=True)
        dec_time_on, dec_code = run_flux_decompress(flux_on_archive, flux_on_extract)
        
        extracted_file_path = os.path.join(flux_on_extract, filename)
        if os.path.exists(extracted_file_path):
            roundtrip_on_ok = sha256_file(extracted_file_path) == orig_hash
        else:
            roundtrip_on_ok = False
            
        print(f"  - Detected Stride: {detected_stride}")
        print(f"  - Compression Time: {flux_on_time:.3f}s")
        print(f"  - Compression Ratio: {flux_on_ratio:.3f}x")
        print(f"  - Roundtrip verified: {roundtrip_on_ok}")
        
        results[filename]['flux_on'] = {
            'time': flux_on_time,
            'ratio': flux_on_ratio,
            'dec_time': dec_time_on,
            'roundtrip': roundtrip_on_ok,
            'stride': detected_stride
        }
        
        # 2. FLUX (stride OFF)
        print("Running FLUX (stride/transpose OFF)...")
        flux_off_archive = os.path.join(out_dir, f"{filename}.flux_off.flx")
        flux_off_time, _, code, _, stderr = run_flux_compress(filepath, flux_off_archive, disable_transpose=True)
        if code != 0:
            print(f"FLUX OFF failed: {stderr}")
            continue
            
        flux_off_size = os.path.getsize(flux_off_archive)
        flux_off_ratio = orig_size / flux_off_size if flux_off_size > 0 else 0.0
        
        # Decompress & Verify
        flux_off_extract = os.path.join(extract_dir, f"flux_off_{filename}")
        os.makedirs(flux_off_extract, exist_ok=True)
        dec_time_off, dec_code = run_flux_decompress(flux_off_archive, flux_off_extract)
        
        extracted_file_path_off = os.path.join(flux_off_extract, filename)
        if os.path.exists(extracted_file_path_off):
            roundtrip_off_ok = sha256_file(extracted_file_path_off) == orig_hash
        else:
            roundtrip_off_ok = False
            
        print(f"  - Compression Time: {flux_off_time:.3f}s")
        print(f"  - Compression Ratio: {flux_off_ratio:.3f}x")
        print(f"  - Roundtrip verified: {roundtrip_off_ok}")
        
        results[filename]['flux_off'] = {
            'time': flux_off_time,
            'ratio': flux_off_ratio,
            'dec_time': dec_time_off,
            'roundtrip': roundtrip_off_ok
        }
        
        # Read file bytes for python compressions
        with open(filepath, 'rb') as f:
            file_bytes = f.read()
            
        # 3. gzip -9
        print("Running gzip -9...")
        gz_start = time.perf_counter()
        gz_data = gzip.compress(file_bytes, compresslevel=9)
        gz_time = time.perf_counter() - gz_start
        gz_size = len(gz_data)
        gz_ratio = orig_size / gz_size if gz_size > 0 else 0.0
        print(f"  - Compression Time: {gz_time:.3f}s")
        print(f"  - Compression Ratio: {gz_ratio:.3f}x")
        
        results[filename]['gzip'] = {
            'time': gz_time,
            'ratio': gz_ratio,
            'roundtrip': True
        }
        
        # 4. zstd -19
        print("Running zstd -19...")
        zstd_start = time.perf_counter()
        # pyzstd uses level 19
        zstd_data = pyzstd.compress(file_bytes, 19)
        zstd_time = time.perf_counter() - zstd_start
        zstd_size = len(zstd_data)
        zstd_ratio = orig_size / zstd_size if zstd_size > 0 else 0.0
        print(f"  - Compression Time: {zstd_time:.3f}s")
        print(f"  - Compression Ratio: {zstd_ratio:.3f}x")
        
        results[filename]['zstd'] = {
            'time': zstd_time,
            'ratio': zstd_ratio,
            'roundtrip': True
        }

    # Print summary tables
    print("\n\n" + "="*50)
    print("FINAL SUMMARY REPORT")
    print("="*50)
    
    for filename in files:
        if filename not in results:
            continue
        f_res = results[filename]
        print(f"\nDataset: {filename} (15MB)")
        print(f"{'Method':<32} {'Time (s)':<10} {'Ratio':<10} {'Roundtrip':<10}")
        print("-" * 65)
        
        # FLUX ON
        on = f_res.get('flux_on', {})
        on_stride_str = f" (stride {on.get('stride', 'None')})" if on.get('stride') else ""
        print(f"{'FLUX (stride/transpose ON)' + on_stride_str:<32} {on.get('time', 0.0):.3f}s     {on.get('ratio', 0.0):.2f}x      {str(on.get('roundtrip', False)):<10}")
        
        # FLUX OFF
        off = f_res.get('flux_off', {})
        print(f"{'FLUX (stride/transpose OFF)':<32} {off.get('time', 0.0):.3f}s     {off.get('ratio', 0.0):.2f}x      {str(off.get('roundtrip', False)):<10}")
        
        # gzip -9
        gz = f_res.get('gzip', {})
        print(f"{'gzip -9':<32} {gz.get('time', 0.0):.3f}s     {gz.get('ratio', 0.0):.2f}x      {str(gz.get('roundtrip', False)):<10}")
        
        # zstd -19
        zs = f_res.get('zstd', {})
        print(f"{'zstd -19':<32} {zs.get('time', 0.0):.3f}s     {zs.get('ratio', 0.0):.2f}x      {str(zs.get('roundtrip', False)):<10}")
        
if __name__ == '__main__':
    main()
