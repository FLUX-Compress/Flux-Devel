import os
import sys
import subprocess
import hashlib
import shutil
import zipfile

# Absolute paths
ROOT_DIR = r"D:\Chat Server\FLUX"
CLI_PATH = os.path.join(ROOT_DIR, "target", "release", "flux-cli.exe")
DATA_DIR = os.path.join(ROOT_DIR, "data")
SCRATCH_DIR = os.path.join(ROOT_DIR, "scratch")

os.makedirs(SCRATCH_DIR, exist_ok=True)

# Helper functions
def sha256_file(filepath):
    if not os.path.exists(filepath):
        return None
    h = hashlib.sha256()
    with open(filepath, 'rb') as f:
        while True:
            chunk = f.read(65536)
            if not chunk:
                break
            h.update(chunk)
    return h.hexdigest()

def run_cmd(args):
    proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    stdout, stderr = proc.communicate()
    return proc.returncode, stdout, stderr

# Create custom datasets
edge_empty = os.path.join(SCRATCH_DIR, "edge_empty.bin")
with open(edge_empty, "wb") as f:
    pass

edge_tiny = os.path.join(SCRATCH_DIR, "edge_tiny.bin")
with open(edge_tiny, "wb") as f:
    f.write(b"FLUX!")

edge_random = os.path.join(SCRATCH_DIR, "edge_random.bin")
with open(edge_random, "wb") as f:
    f.write(os.urandom(5 * 1024 * 1024))  # 5MB high entropy

edge_zip = os.path.join(SCRATCH_DIR, "edge_already_compressed.zip")
with zipfile.ZipFile(edge_zip, "w") as z:
    z.writestr("test.txt", "This is some text that is zipped.")

# Single files to test
files_to_test = [
    ("coordinates_xyz.bin", os.path.join(DATA_DIR, "coordinates_xyz.bin")),
    ("float64_scientific.bin", os.path.join(DATA_DIR, "float64_scientific.bin")),
    ("float32_timeseries.bin", os.path.join(DATA_DIR, "float32_timeseries.bin")),
    ("sensor_log.bin", os.path.join(DATA_DIR, "sensor_log.bin")),
    ("real_audio.wav", os.path.join(DATA_DIR, "real_audio.wav")),
    ("real_scientific.bin", os.path.join(DATA_DIR, "real_scientific.bin")),
    ("audio_pcm16.bin", os.path.join(DATA_DIR, "audio_pcm16.bin")),
    ("gutenberg.txt", os.path.join(DATA_DIR, "gutenberg.txt")),
    ("edge_empty.bin", edge_empty),
    ("edge_tiny.bin", edge_tiny),
    ("edge_random.bin", edge_random),
    ("edge_already_compressed.zip", edge_zip),
]

# Output tables
single_file_results = []
cli_outputs = []

for name, path in files_to_test:
    if not os.path.exists(path):
        print(f"Skipping {name} (does not exist at {path})")
        continue

    orig_size = os.path.getsize(path)
    orig_hash = sha256_file(path)
    orig_hash_short = orig_hash[:8] if orig_hash else "None"

    # Compress
    archive_path = os.path.join(SCRATCH_DIR, f"{name}.flx")
    if os.path.exists(archive_path):
        os.remove(archive_path)

    comp_code, comp_out, comp_err = run_cmd([CLI_PATH, "compress", path, archive_path])
    archive_size = os.path.getsize(archive_path) if os.path.exists(archive_path) else 0
    ratio = orig_size / archive_size if archive_size > 0 else 0.0

    cli_outputs.append({
        "step": f"Compress {name}",
        "exit_code": comp_code,
        "stdout": comp_out,
        "stderr": comp_err
    })

    # Decompress
    out_dir = os.path.join(SCRATCH_DIR, f"extracted_{name}")
    if os.path.exists(out_dir):
        shutil.rmtree(out_dir)
    os.makedirs(out_dir)

    dec_code, dec_out, dec_err = run_cmd([CLI_PATH, "decompress", archive_path, out_dir])
    
    cli_outputs.append({
        "step": f"Decompress {name}",
        "exit_code": dec_code,
        "stdout": dec_out,
        "stderr": dec_err
    })

    decomp_path = os.path.join(out_dir, name)
    decomp_size = os.path.getsize(decomp_path) if os.path.exists(decomp_path) else -1
    decomp_hash = sha256_file(decomp_path)
    decomp_hash_short = decomp_hash[:8] if decomp_hash else "None"

    match = (orig_size == decomp_size) and (orig_hash == decomp_hash)

    single_file_results.append({
        "file": name,
        "orig_size": orig_size,
        "orig_hash": orig_hash_short,
        "decomp_hash": decomp_hash_short,
        "match": match,
        "ratio": f"{ratio:.2f}x" if ratio > 0 else "N/A"
    })

# Part 3: Directory round-trip
dir_src = os.path.join(SCRATCH_DIR, "verify_dir")
if os.path.exists(dir_src):
    shutil.rmtree(dir_src)
os.makedirs(dir_src)

# Create folder layout
with open(os.path.join(dir_src, "text.txt"), "w") as f:
    f.write("This is a simple text file inside the test folder.\n" * 100)
with open(os.path.join(dir_src, "tiny.bin"), "wb") as f:
    f.write(b"TINY")
os.makedirs(os.path.join(dir_src, "subdir", "empty_dir"))
with open(os.path.join(dir_src, "subdir", "nested.bin"), "wb") as f:
    f.write(os.urandom(1024 * 50))  # 50KB random binary

# Record source dir files and hashes
src_files = {}
for root, dirs, files in os.walk(dir_src):
    for d in dirs:
        rel_path = os.path.relpath(os.path.join(root, d), dir_src)
        src_files[rel_path + "/"] = ("DIR", 0, "")
    for f in files:
        full_p = os.path.join(root, f)
        rel_path = os.path.relpath(full_p, dir_src)
        src_files[rel_path] = ("FILE", os.path.getsize(full_p), sha256_file(full_p))

dir_archive = os.path.join(SCRATCH_DIR, "verify_dir.flx")
if os.path.exists(dir_archive):
    os.remove(dir_archive)

dir_comp_code, dir_comp_out, dir_comp_err = run_cmd([CLI_PATH, "compress", dir_src, dir_archive])
cli_outputs.append({
    "step": "Compress Directory",
    "exit_code": dir_comp_code,
    "stdout": dir_comp_out,
    "stderr": dir_comp_err
})

dir_dest = os.path.join(SCRATCH_DIR, "verify_dir_decompressed")
if os.path.exists(dir_dest):
    shutil.rmtree(dir_dest)
os.makedirs(dir_dest)

dir_dec_code, dir_dec_out, dir_dec_err = run_cmd([CLI_PATH, "decompress", dir_archive, dir_dest])
cli_outputs.append({
    "step": "Decompress Directory",
    "exit_code": dir_dec_code,
    "stdout": dir_dec_out,
    "stderr": dir_dec_err
})

# Verify directory round-trip content
dest_files = {}
for root, dirs, files in os.walk(dir_dest):
    for d in dirs:
        rel_path = os.path.relpath(os.path.join(root, d), dir_dest)
        dest_files[rel_path + "/"] = ("DIR", 0, "")
    for f in files:
        full_p = os.path.join(root, f)
        rel_path = os.path.relpath(full_p, dir_dest)
        dest_files[rel_path] = ("FILE", os.path.getsize(full_p), sha256_file(full_p))

dir_match = (src_files == dest_files)

# Part 4: Encrypted round-trip
enc_file = os.path.join(DATA_DIR, "sensor_log.bin")
enc_archive = os.path.join(SCRATCH_DIR, "encrypted_sensor_log.flx")
if os.path.exists(enc_archive):
    os.remove(enc_archive)

enc_comp_code, enc_comp_out, enc_comp_err = run_cmd([
    CLI_PATH, "compress", "-p", "CorrectPassword123!", enc_file, enc_archive
])
cli_outputs.append({
    "step": "Compress Encrypted File",
    "exit_code": enc_comp_code,
    "stdout": enc_comp_out,
    "stderr": enc_comp_err
})

# 4.2 Decompress with correct password
enc_dec_ok_dir = os.path.join(SCRATCH_DIR, "encrypted_extracted_ok")
if os.path.exists(enc_dec_ok_dir):
    shutil.rmtree(enc_dec_ok_dir)
os.makedirs(enc_dec_ok_dir)

enc_dec_ok_code, enc_dec_ok_out, enc_dec_ok_err = run_cmd([
    CLI_PATH, "decompress", "-p", "CorrectPassword123!", enc_archive, enc_dec_ok_dir
])
cli_outputs.append({
    "step": "Decompress Correct Password",
    "exit_code": enc_dec_ok_code,
    "stdout": enc_dec_ok_out,
    "stderr": enc_dec_ok_err
})

decomp_enc_file = os.path.join(enc_dec_ok_dir, "sensor_log.bin")
enc_ok_match = False
if os.path.exists(decomp_enc_file):
    enc_ok_match = (sha256_file(decomp_enc_file) == sha256_file(enc_file))

# 4.3 Decompress with wrong password
enc_dec_fail_dir = os.path.join(SCRATCH_DIR, "encrypted_extracted_fail")
if os.path.exists(enc_dec_fail_dir):
    shutil.rmtree(enc_dec_fail_dir)
os.makedirs(enc_dec_fail_dir)

enc_dec_fail_code, enc_dec_fail_out, enc_dec_fail_err = run_cmd([
    CLI_PATH, "decompress", "-p", "WrongPassword456!", enc_archive, enc_dec_fail_dir
])
cli_outputs.append({
    "step": "Decompress Wrong Password",
    "exit_code": enc_dec_fail_code,
    "stdout": enc_dec_fail_out,
    "stderr": enc_dec_fail_err
})

# Verify it failed and output directory is empty or not created (i.e. no corrupted file written)
enc_fail_clean = True
if os.path.exists(enc_dec_fail_dir):
    files_in_fail = os.listdir(enc_dec_fail_dir)
    if files_in_fail:
        enc_fail_clean = False

# Print Report
print("\n" + "="*80)
print("FLUX END-TO-END VERIFICATION REPORT")
print("="*80)

print("\n--- Part 1: Single File & Edge Case Round-trip Results ---")
print(f"{'File':<30} {'OrigSize':<10} {'OrigSHA256':<10} {'DecompSHA256':<12} {'Match':<6} {'Ratio':<6}")
print("-" * 80)
for r in single_file_results:
    print(f"{r['file']:<30} {r['orig_size']:<10} {r['orig_hash']:<10} {r['decomp_hash']:<12} {str(r['match']):<6} {r['ratio']:<6}")

print("\n--- Part 3: Directory Round-trip Results ---")
print(f"Directory Structure Identical: {dir_match}")
print(f"Source Directories/Files Count: {len(src_files)}")
print(f"Dest Directories/Files Count: {len(dest_files)}")

print("\n--- Part 4: Encryption Round-trip Results ---")
print(f"1. Compression with password Exit Code: {enc_comp_code}")
print(f"2. Decompression with CORRECT password Exit Code: {enc_dec_ok_code} (Match: {enc_ok_match})")
print(f"3. Decompression with WRONG password Exit Code: {enc_dec_fail_code} (Clean Error: {enc_fail_clean})")

print("\n--- Part 2: CLI Exit Codes and stderr ---")
any_stderr = False
for out in cli_outputs:
    has_err = bool(out["stderr"].strip())
    if has_err or out["exit_code"] != 0:
        any_stderr = True
    print(f"\nStep: {out['step']}")
    print(f"  Exit Code: {out['exit_code']}")
    if out["stdout"].strip():
        print(f"  Stdout:\n{out['stdout'].strip()}")
    if out["stderr"].strip():
        print(f"  Stderr:\n{out['stderr'].strip()}")

print("\n" + "="*80)
print("VERIFICATION CONCLUSION")
print("="*80)
all_success = all(r["match"] for r in single_file_results) and dir_match and enc_ok_match and enc_dec_fail_code != 0 and enc_fail_clean
print(f"All roundtrips byte-perfect: {all_success}")
print(f"Any non-zero exit codes (excluding wrong password): {any(out['exit_code'] != 0 for out in cli_outputs if 'Wrong Password' not in out['step'])}")
print(f"Any stderr output: {any_stderr}")
print("="*80)

if not all_success:
    sys.exit(1)
else:
    sys.exit(0)
