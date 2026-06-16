import os
import sys
import math
import subprocess
import re

# Deterministic H calculation
def calculate_entropy(counts, total):
    if total == 0:
        return 0.0
    entropy = 0.0
    for c in counts:
        if c > 0:
            p = c / total
            entropy -= p * math.log2(p)
    return entropy

def analyze_trace_file(trace_path):
    if not os.path.exists(trace_path):
        print(f"Error: trace file not found at {trace_path}")
        return None

    with open(trace_path, 'rb') as f:
        data = f.read()

    n = len(data) // 3
    if n == 0:
        return {
            'N': 0,
            'H_lit': 0.0,
            'MSB6': (0, 0.0),
            'LSB6': (0, 0.0),
            'FULL': (0, 0.0),
            'PREV2_MIX': (0, 0.0)
        }

    literals = []
    p1s = []
    p2s = []
    for i in range(n):
        literals.append(data[3 * i])
        p1s.append(data[3 * i + 1])
        p2s.append(data[3 * i + 2])

    # Unconditional entropy
    lit_counts = [0] * 256
    for lit in literals:
        lit_counts[lit] += 1
    h_lit = calculate_entropy(lit_counts, n)

    # Helper function for conditional entropy
    def analyze_mode(get_context):
        # map context -> list of literals in that context
        buckets = {}
        for i in range(n):
            ctx = get_context(p1s[i], p2s[i])
            if ctx not in buckets:
                buckets[ctx] = []
            buckets[ctx].append(literals[i])

        num_buckets = len(buckets)
        cond_entropy = 0.0
        for ctx, bucket_lits in buckets.items():
            b_total = len(bucket_lits)
            b_counts = [0] * 256
            for lit in bucket_lits:
                b_counts[lit] += 1
            b_entropy = calculate_entropy(b_counts, b_total)
            cond_entropy += (b_total / n) * b_entropy

        return num_buckets, cond_entropy

    # Context modes
    # a) MSB6: previous byte's top 6 bits as context
    msb6_buckets, h_msb6 = analyze_mode(lambda p1, p2: p1 >> 2)

    # b) LSB6: previous byte's bottom 6 bits as context
    lsb6_buckets, h_lsb6 = analyze_mode(lambda p1, p2: p1 & 0x3F)

    # c) FULL: full previous byte as context
    full_buckets, h_full = analyze_mode(lambda p1, p2: p1)

    # d) PREV2_MIX: a small mix function over previous two bytes
    #    yielding ~64 contexts. We use: ((p1 >> 2) ^ (p2 >> 2)) & 0x3F
    prev2_mix_buckets, h_prev2 = analyze_mode(lambda p1, p2: ((p1 >> 2) ^ (p2 >> 2)) & 0x3F)

    return {
        'N': n,
        'H_lit': h_lit,
        'MSB6': (msb6_buckets, h_msb6),
        'LSB6': (lsb6_buckets, h_lsb6),
        'FULL': (full_buckets, h_full),
        'PREV2_MIX': (prev2_mix_buckets, h_prev2)
    }

def run_experiment(cli_path, input_path):
    print(f"\n==================================================")
    print(f"Running experiment on: {input_path}")
    print(f"==================================================")
    
    if os.path.isdir(input_path):
        orig_size = 0
        for root, dirs, files in os.walk(input_path):
            for file in files:
                orig_size += os.path.getsize(os.path.join(root, file))
    else:
        orig_size = os.path.getsize(input_path)
    
    # We will run compression on this file/directory
    temp_archive = input_path + ".temp.flx"
    if os.path.exists(temp_archive):
        os.remove(temp_archive)
        
    trace_path = input_path + ".temp.flx.lit_trace"
    if os.path.exists(trace_path):
        os.remove(trace_path)

    # Run with FLUX_LITERAL_TRACE=1 and FLUX_ANALYZE=1
    env = os.environ.copy()
    env['FLUX_LITERAL_TRACE'] = '1'
    env['FLUX_ANALYZE'] = '1'
    
    cmd = [cli_path, "compress", "-l", "balanced", "--force", input_path, temp_archive]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
    stdout, stderr = proc.communicate()
    
    if proc.returncode != 0:
        print(f"Error during compression of {input_path}:")
        print(stderr)
        return None

    # Parse stdout for stream analysis
    # "Total compressed size:        {} bytes"
    # "Literals stream:     {} bytes"
    total_compressed = 0
    literals_bytes = 0
    
    m_comp = re.search(r"Total compressed size:\s+(\d+) bytes", stdout)
    if m_comp:
        total_compressed = int(m_comp.group(1))
    else:
        # Fallback to file size if diagnostics not printed or format changed
        if os.path.exists(temp_archive):
            total_compressed = os.path.getsize(temp_archive)
            
    m_lit = re.search(r"Literals stream:\s+(\d+) bytes", stdout)
    if m_lit:
        literals_bytes = int(m_lit.group(1))

    # Read and analyze the trace
    results = analyze_trace_file(trace_path)
    
    # Cleanup temp files
    if os.path.exists(temp_archive):
        os.remove(temp_archive)
    if os.path.exists(trace_path):
        os.remove(trace_path)
        
    if results is None:
        return None

    # Print results summary
    N = results['N']
    h_lit = results['H_lit']
    print(f"Original file size: {orig_size} bytes")
    print(f"Total compressed size: {total_compressed} bytes")
    print(f"Total literals (N): {N}")
    print(f"Unconditional entropy H(literal): {h_lit:.4f} bits/symbol")
    print(f"Literals stream size (original): {literals_bytes} bytes")
    
    modes = ['MSB6', 'LSB6', 'FULL', 'PREV2_MIX']
    for mode in modes:
        buckets, h_cond = results[mode]
        improvement = h_lit - h_cond
        implied_bits_saved = improvement * N
        implied_bytes_saved = implied_bits_saved / 8.0
        
        # as % of original file size
        pct_orig = (implied_bytes_saved / orig_size * 100.0) if orig_size > 0 else 0.0
        # as % of literal stream size
        pct_lit = (implied_bytes_saved / literals_bytes * 100.0) if literals_bytes > 0 else 0.0
        
        print(f"\nContext Mode: {mode}")
        print(f"  Buckets observed: {buckets}")
        print(f"  H(literal | context): {h_cond:.4f} bits/symbol")
        print(f"  Improvement: {improvement:.4f} bits/symbol")
        print(f"  Implied savings: {implied_bytes_saved:.2f} bytes ({implied_bits_saved:.0f} bits)")
        print(f"  Savings as % of original file size: {pct_orig:.4f}%")
        print(f"  Savings as % of literal stream size: {pct_lit:.4f}%")

    # Return structured data for final reporting
    return {
        'file': os.path.basename(input_path),
        'orig_size': orig_size,
        'total_compressed': total_compressed,
        'literals_bytes': literals_bytes,
        'N': N,
        'H_lit': h_lit,
        'MSB6': results['MSB6'],
        'LSB6': results['LSB6'],
        'FULL': results['FULL'],
        'PREV2_MIX': results['PREV2_MIX']
    }

if __name__ == '__main__':
    cli = r"D:\Chat Server\FLUX\target\release\flux-cli.exe"
    if len(sys.argv) > 1:
        cli = sys.argv[1]
    
    # Verify CLI exists
    if not os.path.exists(cli):
        print(f"CLI not found at {cli}. Please build it first.")
        sys.exit(1)
        
    corpus = [
        r"D:\Chat Server\FLUX\target\release\flux-cli.exe",
        r"D:\Chat Server\FLUX\data\gutenberg.txt",
        r"D:\Chat Server\FLUX\data\coordinates_xyz.bin",
    ]
    
    # For mixed file/source code, we will compress the crates/flux-core/src directory.
    # To do that, we will first tar/zip it, or we can compress the directory itself!
    # Wait, the tool compress_directory is called if the input is a directory.
    # Let's see if we can compress the directory "D:\Chat Server\FLUX\crates\flux-core\src" directly!
    corpus.append(r"D:\Chat Server\FLUX\crates\flux-core\src")
    
    all_results = []
    for path in corpus:
        if not os.path.exists(path):
            print(f"Skipping {path} (does not exist)")
            continue
        res = run_experiment(cli, path)
        if res:
            all_results.append(res)
            
    # Print markdown table
    print("\n\n==================================================")
    print("FINAL SUMMARY REPORT")
    print("==================================================")
    
    headers = [
        "File", "N literals", "H(literal)", 
        "H|MSB6", "H|LSB6", "H|FULL", "H|PREV2_MIX",
        "Savings MSB6", "Savings LSB6", "Savings FULL", "Savings PREV2"
    ]
    
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join(["---"] * len(headers)) + " |")
    
    for r in all_results:
        f_name = r['file']
        N = r['N']
        h_lit = r['H_lit']
        
        h_msb6 = r['MSB6'][1]
        h_lsb6 = r['LSB6'][1]
        h_full = r['FULL'][1]
        h_prev2 = r['PREV2_MIX'][1]
        
        # Savings in bits/symbol
        s_msb6 = h_lit - h_msb6
        s_lsb6 = h_lit - h_lsb6
        s_full = h_lit - h_full
        s_prev2 = h_lit - h_prev2
        
        # Savings as % of original size
        # bytes_saved = s * N / 8
        # pct = bytes_saved / orig_size * 100
        p_msb6 = ((s_msb6 * N / 8) / r['orig_size'] * 100) if r['orig_size'] > 0 else 0.0
        p_lsb6 = ((s_lsb6 * N / 8) / r['orig_size'] * 100) if r['orig_size'] > 0 else 0.0
        p_full = ((s_full * N / 8) / r['orig_size'] * 100) if r['orig_size'] > 0 else 0.0
        p_prev2 = ((s_prev2 * N / 8) / r['orig_size'] * 100) if r['orig_size'] > 0 else 0.0
        
        row = [
            f_name,
            str(N),
            f"{h_lit:.4f}",
            f"{h_msb6:.4f}",
            f"{h_lsb6:.4f}",
            f"{h_full:.4f}",
            f"{h_prev2:.4f}",
            f"{s_msb6:.4f} ({p_msb6:.3f}%)",
            f"{s_lsb6:.4f} ({p_lsb6:.3f}%)",
            f"{s_full:.4f} ({p_full:.3f}%)",
            f"{s_prev2:.4f} ({p_prev2:.3f}%)",
        ]
        print("| " + " | ".join(row) + " |")
