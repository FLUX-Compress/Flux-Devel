import os
import sys
import math
import time
import gzip
import array
import random
import hashlib
import subprocess
import urllib.request

# Ensure we can import or gracefully fallback for zstd
try:
    import pyzstd
    has_pyzstd = True
except ImportError:
    has_pyzstd = False

# --- Data Generation Constants and Logic ---

def generate_float32_timeseries(filename, size_mb=15):
    num_floats = int(size_mb * 1024 * 1024 / 4)
    print(f"Generating float32 timeseries to {filename} ({num_floats} floats)...")
    offset = 1000.0
    chunk_size = 100000
    with open(filename, 'wb') as f:
        for start in range(0, num_floats, chunk_size):
            chunk_len = min(chunk_size, num_floats - start)
            arr = array.array('f')
            for i in range(start, start + chunk_len):
                t = i / 10000.0
                signal = 10.0 * math.sin(t) + 3.0 * math.sin(t * 0.2)
                noise = random.uniform(-0.001, 0.001)
                arr.append(offset + signal + noise)
            arr.tofile(f)

def generate_float64_scientific(filename, size_mb=15):
    num_floats = int(size_mb * 1024 * 1024 / 8)
    print(f"Generating float64 scientific to {filename} ({num_floats} floats)...")
    offset = 5000.0
    chunk_size = 50000
    with open(filename, 'wb') as f:
        for start in range(0, num_floats, chunk_size):
            chunk_len = min(chunk_size, num_floats - start)
            arr = array.array('d')
            for i in range(start, start + chunk_len):
                t = i / 200000000000.0
                signal = 50.0 * math.sin(t) + 15.0 * math.cos(t * 0.2)
                noise = random.uniform(-1e-12, 1e-12)
                arr.append(offset + signal + noise)
            arr.tofile(f)

def generate_audio_pcm16(filename, size_mb=15):
    num_samples = int(size_mb * 1024 * 1024 / 2)
    print(f"Generating audio PCM16 to {filename} ({num_samples} samples)...")
    amp = 4000
    freq = 50.0
    samplerate = 44100.0
    chunk_size = 100000
    with open(filename, 'wb') as f:
        for start in range(0, num_samples, chunk_size):
            chunk_len = min(chunk_size, num_samples - start)
            arr = array.array('h')
            for i in range(start, start + chunk_len, 2):
                t = (start + i) / (2.0 * samplerate)
                val_l = int(amp * math.sin(2.0 * math.pi * freq * t))
                val_r = int(amp * math.cos(2.0 * math.pi * freq * t))
                arr.append(val_l)
                arr.append(val_r)
            arr.tofile(f)

def generate_sensor_log(filename, size_mb=15):
    num_records = int(size_mb * 1024 * 1024 / 12)
    print(f"Generating sensor log to {filename} ({num_records} records)...")
    chunk_size = 50000
    with open(filename, 'wb') as f:
        for start in range(0, num_records, chunk_size):
            chunk_len = min(chunk_size, num_records - start)
            arr = array.array('f')
            for i in range(start, start + chunk_len):
                t = start + i
                temp = 70.0 + 5.0 * math.sin(t / 10000.0) + random.uniform(-0.0001, 0.0001)
                pressure = 1013.25 + 10.0 * math.cos(t / 20000.0) + random.uniform(-0.0001, 0.0001)
                humidity = 50.0 + 15.0 * math.sin(t / 5000.0) + random.uniform(-0.0001, 0.0001)
                arr.append(temp)
                arr.append(pressure)
                arr.append(humidity)
            arr.tofile(f)

def generate_coordinates_xyz(filename, size_mb=15):
    num_records = int(size_mb * 1024 * 1024 / 12)
    print(f"Generating coordinates XYZ to {filename} ({num_records} records)...")
    chunk_size = 50000
    with open(filename, 'wb') as f:
        for start in range(0, num_records, chunk_size):
            chunk_len = min(chunk_size, num_records - start)
            arr = array.array('f')
            for i in range(start, start + chunk_len):
                t = (start + i) / 10000.0
                theta = t
                r = 10.0 + theta / 100.0
                x = 1000.0 + r * math.cos(theta)
                y = 1000.0 + r * math.sin(theta)
                z = 1000.0 + theta / 5.0
                arr.append(x)
                arr.append(y)
                arr.append(z)
            arr.tofile(f)

def download_file(url, output):
    print(f"Trying to download {url} to {output}...")
    req = urllib.request.Request(
        url, 
        headers={'User-Agent': 'Mozilla/5.0'}
    )
    with urllib.request.urlopen(req, timeout=10) as response, open(output, 'wb') as out_file:
        out_file.write(response.read())
    print("Download complete!")
    return True

def generate_enhanced_real_audio(filename, size_mb=15):
    print(f"Generating enhanced real-music WAV structure to {filename}...")
    samplerate = 44100
    num_samples = int(size_mb * 1024 * 1024 / 2)
    
    notes = {
        'C4': 261.63, 'E4': 329.63, 'G4': 392.00, 'B4': 493.88,
        'A3': 220.00, 'C5': 523.25, 'E5': 659.25, 'G5': 783.99,
        'F3': 174.61, 'A4': 440.00, 'C5_': 523.25, 'F5': 698.46,
        'G3': 196.00, 'B4_': 493.88, 'D5': 587.33, 'G5_': 783.99,
    }
    progression = [
        ('A3', 'C4', 'E4', 'A4'),
        ('F3', 'A4', 'C5', 'F5'),
        ('C4', 'E4', 'G4', 'C5'),
        ('G3', 'B4_', 'D5', 'G5_')
    ]
    tempo = 120
    seconds_per_beat = 60.0 / tempo
    samples_per_beat = int(samplerate * seconds_per_beat)
    samples_per_chord = samples_per_beat * 4
    
    arr = array.array('h')
    current_sample = 0
    while len(arr) < num_samples:
        chord_idx = (current_sample // samples_per_chord) % len(progression)
        chord_t = (current_sample % samples_per_chord) / samplerate
        beat_t = (current_sample % samples_per_beat) / samplerate
        chord = progression[chord_idx]
        
        f_bass = notes[chord[0]]
        f1 = notes[chord[1]]
        f2 = notes[chord[2]]
        f3 = notes[chord[3]]
        
        chord_duration = samples_per_chord / samplerate
        if chord_t < 0.2:
            env_chord = chord_t / 0.2
        elif chord_t < chord_duration - 0.4:
            env_chord = 1.0
        else:
            env_chord = max(0.0, 1.0 - (chord_t - (chord_duration - 0.4)) / 0.4)
            
        t = current_sample / samplerate
        wave1 = math.sin(2.0 * math.pi * f1 * t) + 0.4 * math.sin(2.0 * math.pi * f1 * 2 * t)
        wave2 = math.sin(2.0 * math.pi * f2 * t)
        wave3 = math.sin(2.0 * math.pi * f3 * t)
        wave_bass = math.sin(2.0 * math.pi * f_bass * t)
        
        synth_signal = (wave1 * 0.25 + wave2 * 0.2 + wave3 * 0.15 + wave_bass * 0.4) * env_chord
        beat_idx = (current_sample // samples_per_beat) % 4
        snare = 0.0
        if beat_idx in [1, 3]:
            decay = math.exp(-25.0 * beat_t)
            noise = (random.random() * 2.0 - 1.0)
            snare = noise * decay * 0.5
            
        l_sig = synth_signal * 0.7 + snare * 0.5
        r_sig = synth_signal * 0.7 - snare * 0.5
        val_l = int(16000.0 * l_sig)
        val_r = int(16000.0 * r_sig)
        arr.append(max(-32768, min(32767, val_l)))
        arr.append(max(-32768, min(32767, val_r)))
        current_sample += 2
        
    with open(filename, 'wb') as f:
        arr.tofile(f)

def generate_enhanced_real_scientific(filename, size_mb=15):
    print(f"Generating physical heat-diffusion simulation data to {filename}...")
    grid_size = 64
    num_steps = int(size_mb * 1024 * 1024 / (grid_size * grid_size * 8))
    T = [[20.0 for _ in range(grid_size)] for _ in range(grid_size)]
    T[grid_size//2][grid_size//2] = 200.0
    T[grid_size//2+1][grid_size//2] = 200.0
    T[grid_size//2][grid_size//2+1] = 200.0
    T[10][10] = -50.0
    
    alpha = 0.1
    arr = array.array('d')
    for step in range(num_steps):
        T_new = [row[:] for row in T]
        for y in range(1, grid_size - 1):
            for x in range(1, grid_size - 1):
                laplacian = T[y+1][x] + T[y-1][x] + T[y][x+1] + T[y][x-1] - 4.0 * T[y][x]
                T_new[y][x] = T[y][x] + alpha * laplacian
        T_new[grid_size//2][grid_size//2] = 200.0
        T_new[10][10] = -50.0
        for y in range(grid_size):
            for x in range(grid_size):
                arr.append(T_new[y][x])
        T = T_new
    with open(filename, 'wb') as f:
        arr.tofile(f)

def prepare_data(data_dir):
    os.makedirs(data_dir, exist_ok=True)
    files = {
        'float32_timeseries.bin': lambda p: generate_float32_timeseries(p),
        'float64_scientific.bin': lambda p: generate_float64_scientific(p),
        'audio_pcm16.bin': lambda p: generate_audio_pcm16(p),
        'sensor_log.bin': lambda p: generate_sensor_log(p),
        'coordinates_xyz.bin': lambda p: generate_coordinates_xyz(p),
        'real_scientific.bin': lambda p: generate_enhanced_real_scientific(p)
    }
    
    for filename, generator in files.items():
        filepath = os.path.join(data_dir, filename)
        if not os.path.exists(filepath):
            generator(filepath)
            
    # Try downloading real audio, fallback to synthesis if needed
    audio_path = os.path.join(data_dir, 'real_audio.wav')
    if not os.path.exists(audio_path):
        download_success = False
        for url in [
            "https://github.com/mathiasbynens/din-din/raw/master/ambient.wav",
            "https://raw.githubusercontent.com/rafaelreis-m/samples/master/WAV/sample.wav"
        ]:
            try:
                if download_file(url, audio_path):
                    download_success = True
                    break
            except Exception as e:
                print(f"Download failed from {url}: {e}")
        if not download_success:
            generate_enhanced_real_audio(audio_path)

# --- Benchmarking Logic ---

def sha256_file(filepath):
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()

def run_flux_compress(cli_path, input_path, output_path):
    cmd = [cli_path, "compress", "-l", "balanced", input_path, output_path]
    start_time = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    return end_time - start_time, proc.returncode, stdout, stderr

def run_flux_decompress(cli_path, archive_path, output_dir):
    cmd = [cli_path, "decompress", archive_path, output_dir]
    start_time = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = proc.communicate()
    end_time = time.perf_counter()
    return end_time - start_time, proc.returncode

def main():
    root_dir = os.path.abspath(os.path.dirname(__file__))
    data_dir = os.path.join(root_dir, 'data')
    out_dir = os.path.join(root_dir, 'out')
    extract_dir = os.path.join(root_dir, 'out_extracted')
    
    os.makedirs(out_dir, exist_ok=True)
    os.makedirs(extract_dir, exist_ok=True)
    
    # 1. Prepare Datasets
    print("Preparing datasets...")
    prepare_data(data_dir)
    
    # Locate CLI binary
    cli_candidates = [
        os.path.join(root_dir, "target", "release", "flux-cli.exe"),
        os.path.join(root_dir, "target", "release", "flux-cli"),
    ]
    cli_path = None
    for candidate in cli_candidates:
        if os.path.exists(candidate):
            cli_path = candidate
            break
            
    if not cli_path:
        print("Error: flux-cli binary not found in target/release/. Did you run 'cargo build --release'?")
        sys.exit(1)
        
    print(f"Using FLUX CLI: {cli_path}")
    
    files = [
        'coordinates_xyz.bin',
        'float64_scientific.bin',
        'float32_timeseries.bin',
        'sensor_log.bin',
        'real_audio.wav',
        'real_scientific.bin'
    ]
    
    results = []
    
    for filename in files:
        filepath = os.path.join(data_dir, filename)
        if not os.path.exists(filepath):
            print(f"Skipping {filename} (not found)")
            continue
            
        orig_size = os.path.getsize(filepath)
        orig_hash = sha256_file(filepath)
        
        print(f"\nBenchmarking: {filename} ({orig_size / (1024*1024):.2f} MB)...")
        
        # FLUX
        flux_archive = os.path.join(out_dir, f"{filename}.flux.flx")
        flux_time, code, _, stderr = run_flux_compress(cli_path, filepath, flux_archive)
        if code != 0:
            print(f"FLUX failed: {stderr}")
            continue
        flux_size = os.path.getsize(flux_archive)
        flux_ratio = orig_size / flux_size if flux_size > 0 else 0.0
        
        # Decompress & Verify
        flux_extract = os.path.join(extract_dir, f"flux_extracted_{filename}")
        os.makedirs(flux_extract, exist_ok=True)
        _, dec_code = run_flux_decompress(cli_path, flux_archive, flux_extract)
        extracted_file_path = os.path.join(flux_extract, filename)
        roundtrip_ok = sha256_file(extracted_file_path) == orig_hash if os.path.exists(extracted_file_path) else False
        
        # gzip -9
        with open(filepath, 'rb') as f:
            file_bytes = f.read()
        gz_data = gzip.compress(file_bytes, compresslevel=9)
        gz_ratio = orig_size / len(gz_data)
        
        # zstd -19
        if has_pyzstd:
            zstd_data = pyzstd.compress(file_bytes, 19)
            zstd_ratio = orig_size / len(zstd_data)
        else:
            # Fallback to system zstd CLI if available
            try:
                zstd_proc = subprocess.Popen(
                    ['zstd', '-19', '-c', filepath],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE
                )
                zstd_out, _ = zstd_proc.communicate()
                if zstd_proc.returncode == 0:
                    zstd_ratio = orig_size / len(zstd_out)
                else:
                    zstd_ratio = 0.0
            except FileNotFoundError:
                zstd_ratio = 0.0
                
        results.append({
            'name': filename,
            'flux_ratio': flux_ratio,
            'gz_ratio': gz_ratio,
            'zstd_ratio': zstd_ratio,
            'roundtrip': roundtrip_ok
        })
        
    print("\n" + "="*70)
    print("BENCHMARK REPRODUCTION SHOWCASE")
    print("="*70)
    print(f"{'Dataset':<25} {'FLUX':<10} {'gzip -9':<10} {'zstd -19':<10} {'Roundtrip':<10}")
    print("-" * 70)
    for res in results:
        zstd_str = f"{res['zstd_ratio']:.2f}x" if res['zstd_ratio'] > 0 else "N/A"
        print(f"{res['name']:<25} {res['flux_ratio']:.2f}x       {res['gz_ratio']:.2f}x       {zstd_str:<10} {str(res['roundtrip']):<10}")
    print("="*70)

if __name__ == '__main__':
    main()
