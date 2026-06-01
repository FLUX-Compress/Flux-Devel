import os

filepath = r"d:\Chat Server\FLUX\crates\flux-core\src\lib.rs"
with open(filepath, 'r', encoding='utf-8') as f:
    lines = f.readlines()

print("Searching for 'fn decompress' in lib.rs:")
for i, line in enumerate(lines):
    if 'pub fn decompress' in line or 'fn decompress(' in line:
        print(f"Line {i+1}: {line.strip()}")
