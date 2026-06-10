import os
import sys
import shutil
import time
import subprocess

# Paths to tools
ROOT_DIR = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
CLI_PATH = os.path.join(ROOT_DIR, "target", "release", "flux-cli.exe")
SCRATCH_DIR = os.path.join(ROOT_DIR, "scratch")

def find_7z():
    candidates = [
        "C:\\Program Files\\7-Zip\\7z.exe",
        "C:\\Program Files (x86)\\7-Zip\\7z.exe",
    ]
    for p in candidates:
        if os.path.exists(p):
            return p
    p = shutil.which("7z")
    if p:
        return p
    return None

ZIP7_PATH = find_7z()

def run_cmd(cmd):
    start = time.perf_counter()
    proc = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    elapsed = time.perf_counter() - start
    return elapsed, proc.returncode, proc.stdout, proc.stderr

def copy_and_zero_mz(src_exe, dest_path):
    shutil.copy2(src_exe, dest_path)
    with open(dest_path, "r+b") as f:
        # Zero out MZ magic bytes
        f.seek(0)
        f.write(b"\x00\x00")

def build_software_archive(dest_dir, exe_path):
    os.makedirs(dest_dir, exist_ok=True)
    # Copy crates source code
    shutil.copytree(os.path.join(ROOT_DIR, "crates"), os.path.join(dest_dir, "crates"), dirs_exist_ok=True)
    # Copy root doc files
    for doc in ["README.md", "SPEC.md", "CONTRIBUTING.md", "LICENSE.md"]:
        p = os.path.join(ROOT_DIR, doc)
        if os.path.exists(p):
            shutil.copy2(p, dest_dir)
    # Copy the executable
    bin_dir = os.path.join(dest_dir, "bin")
    os.makedirs(bin_dir, exist_ok=True)
    shutil.copy2(exe_path, os.path.join(bin_dir, "flux-cli.exe"))

def main():
    print("="*60)
    print("FLUX BCJ CHUNK C BENCHMARK SCRIPT")
    print("="*60)
    print(f"CLI Path: {CLI_PATH}")
    print(f"7-Zip Path: {ZIP7_PATH}")
    
    if not os.path.exists(CLI_PATH):
        print("Error: flux-cli release binary not found. Run cargo build --release first.")
        sys.exit(1)
        
    temp_work_dir = os.path.join(SCRATCH_DIR, "benchmark_work")
    shutil.rmtree(temp_work_dir, ignore_errors=True)
    os.makedirs(temp_work_dir, exist_ok=True)
    
    # 1. Pure Executable Benchmarks
    print("\n--- Benchmark 1: Pure Executable (flux-cli.exe) ---")
    orig_exe = CLI_PATH
    orig_size = os.path.getsize(orig_exe)
    print(f"Original size: {orig_size} bytes ({orig_size / (1024*1024):.2f} MB)")
    
    # Create with BCJ input (original)
    exe_with_bcj = os.path.join(temp_work_dir, "flux-cli-with.exe")
    shutil.copy2(orig_exe, exe_with_bcj)
    
    # Create without BCJ input (MZ zeroed)
    exe_without_bcj = os.path.join(temp_work_dir, "flux-cli-without.exe")
    copy_and_zero_mz(orig_exe, exe_without_bcj)
    
    results = []
    
    # Compress commands
    configs = [
        ("FLUX Balanced (No BCJ)", CLI_PATH, ["compress", "-l", "balanced", exe_without_bcj]),
        ("FLUX Balanced (With BCJ)", CLI_PATH, ["compress", "-l", "balanced", exe_with_bcj]),
        ("FLUX Maximum (No BCJ)", CLI_PATH, ["compress", "-l", "max", exe_without_bcj]),
        ("FLUX Maximum (With BCJ)", CLI_PATH, ["compress", "-l", "max", exe_with_bcj]),
    ]
    
    for label, tool, args in configs:
        archive_name = label.replace(" ", "_").replace("(", "").replace(")", "") + ".flx"
        archive_path = os.path.join(temp_work_dir, archive_name)
        cmd = [tool] + args + [archive_path]
        
        elapsed, code, stdout, stderr = run_cmd(cmd)
        if code == 0 and os.path.exists(archive_path):
            comp_size = os.path.getsize(archive_path)
            ratio = orig_size / comp_size
            results.append((label, comp_size, ratio, elapsed))
            print(f"{label:<30}: Size = {comp_size:<10} Ratio = {ratio:.3f}x, Time = {elapsed:.2f}s")
        else:
            print(f"{label:<30}: FAILED (code={code}, stderr={stderr.decode('utf-8', errors='ignore')})")
            
    # Run 7-Zip if available
    if ZIP7_PATH:
        archive_7z = os.path.join(temp_work_dir, "7z_lzma_max.7z")
        cmd_7z = [ZIP7_PATH, "a", "-mx=9", archive_7z, exe_with_bcj]
        elapsed, code, stdout, stderr = run_cmd(cmd_7z)
        if code == 0 and os.path.exists(archive_7z):
            comp_size = os.path.getsize(archive_7z)
            ratio = orig_size / comp_size
            results.append(("7-Zip LZMA -mx=9", comp_size, ratio, elapsed))
            print(f"{'7-Zip LZMA -mx=9':<30}: Size = {comp_size:<10} Ratio = {ratio:.3f}x, Time = {elapsed:.2f}s")
        else:
            print(f"7-Zip: FAILED (code={code})")
            
    # 2. Software Archive Benchmarks
    print("\n--- Benchmark 2: Software Archive (Sources + Docs + flux-cli.exe) ---")
    sa_with_bcj_dir = os.path.join(temp_work_dir, "software_archive_with")
    build_software_archive(sa_with_bcj_dir, orig_exe)
    
    sa_without_bcj_dir = os.path.join(temp_work_dir, "software_archive_without")
    build_software_archive(sa_without_bcj_dir, exe_without_bcj)
    
    # Total original size
    orig_sa_size = 0
    for root, _, files in os.walk(sa_with_bcj_dir):
        for f in files:
            orig_sa_size += os.path.getsize(os.path.join(root, f))
    print(f"Original size: {orig_sa_size} bytes ({orig_sa_size / (1024*1024):.2f} MB)")
    
    sa_results = []
    
    sa_configs = [
        ("FLUX Balanced (No BCJ)", CLI_PATH, ["compress", "-l", "balanced", sa_without_bcj_dir]),
        ("FLUX Balanced (With BCJ)", CLI_PATH, ["compress", "-l", "balanced", sa_with_bcj_dir]),
        ("FLUX Maximum (No BCJ)", CLI_PATH, ["compress", "-l", "max", sa_without_bcj_dir]),
        ("FLUX Maximum (With BCJ)", CLI_PATH, ["compress", "-l", "max", sa_with_bcj_dir]),
    ]
    
    for label, tool, args in sa_configs:
        archive_name = "sa_" + label.replace(" ", "_").replace("(", "").replace(")", "") + ".flx"
        archive_path = os.path.join(temp_work_dir, archive_name)
        cmd = [tool] + args + [archive_path]
        
        elapsed, code, stdout, stderr = run_cmd(cmd)
        if code == 0 and os.path.exists(archive_path):
            comp_size = os.path.getsize(archive_path)
            ratio = orig_sa_size / comp_size
            sa_results.append((label, comp_size, ratio, elapsed))
            print(f"{label:<30}: Size = {comp_size:<10} Ratio = {ratio:.3f}x, Time = {elapsed:.2f}s")
        else:
            print(f"{label:<30}: FAILED (code={code}, stderr={stderr.decode('utf-8', errors='ignore')})")
            
    # Run 7-Zip on software archive
    if ZIP7_PATH:
        archive_7z = os.path.join(temp_work_dir, "sa_7z_lzma_max.7z")
        # Exclude the temporary dir name structure by adding -ep1 equivalent or compress the dir itself
        cmd_7z = [ZIP7_PATH, "a", "-mx=9", archive_7z, sa_with_bcj_dir]
        elapsed, code, stdout, stderr = run_cmd(cmd_7z)
        if code == 0 and os.path.exists(archive_7z):
            comp_size = os.path.getsize(archive_7z)
            ratio = orig_sa_size / comp_size
            sa_results.append(("7-Zip LZMA -mx=9", comp_size, ratio, elapsed))
            print(f"{'7-Zip LZMA -mx=9':<30}: Size = {comp_size:<10} Ratio = {ratio:.3f}x, Time = {elapsed:.2f}s")
        else:
            print(f"7-Zip: FAILED (code={code})")
            
    # Print Markdown summary tables
    print("\n\n" + "="*50)
    print("MARKDOWN TABLES FOR REPORT")
    print("="*50)
    
    print("\n### Pure Executable (flux-cli.exe)")
    print("| Configuration | Compressed Size (bytes) | Ratio (x) | Compression Time (s) |")
    print("| --- | --- | --- | --- |")
    for r in results:
        print(f"| {r[0]} | {r[1]:,} | {r[2]:.3f}x | {r[3]:.2f}s |")
        
    print("\n### Software Archive")
    print("| Configuration | Compressed Size (bytes) | Ratio (x) | Compression Time (s) |")
    print("| --- | --- | --- | --- |")
    for r in sa_results:
        print(f"| {r[0]} | {r[1]:,} | {r[2]:.3f}x | {r[3]:.2f}s |")

if __name__ == "__main__":
    main()
