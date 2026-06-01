import os
import math
import struct
from collections import Counter

def entropy_order_0(data):
    if not data:
        return 0.0
    counter = Counter(data)
    total = len(data)
    entropy = 0.0
    for count in counter.values():
        p = count / total
        entropy -= p * math.log2(p)
    return entropy

def main():
    sensor_path = r"d:\Chat Server\FLUX\data\sensor_log.bin"
    if not os.path.exists(sensor_path):
        print(f"Error: {sensor_path} not found.")
        return
        
    with open(sensor_path, 'rb') as f:
        data = f.read()
        
    channels = 3
    n = len(data) // (4 * channels)
    print(f"File size: {len(data)} bytes, number of frames: {n}, channels: {channels}")
    
    # De-interleave into 3 channels
    channel_u32s = [[] for _ in range(channels)]
    for i in range(n):
        for ch in range(channels):
            idx = (i * channels + ch) * 4
            val = struct.unpack('<I', data[idx:idx+4])[0]
            channel_u32s[ch].append(val)
            
    # Apply u32 delta coding per channel
    channel_deltas = [[] for _ in range(channels)]
    for ch in range(channels):
        prev = 0
        for i in range(n):
            curr = channel_u32s[ch][i]
            diff = (curr - prev) & 0xFFFFFFFF
            channel_deltas[ch].append(diff)
            prev = curr
            
    # Separate into 4 byte planes
    # Byte planes are constructed by concatenating the channels for each byte index
    planes = [bytearray() for _ in range(4)]
    for byte_idx in range(4): # 3, 2, 1, 0
        for ch in range(channels):
            for i in range(n):
                val = channel_deltas[ch][i]
                byte = (val >> (byte_idx * 8)) & 0xFF
                planes[byte_idx].append(byte)
                
    print("\nShannon Entropy (Order-0) per byte-plane:")
    print("-" * 55)
    plane_names = {
        3: "Plane 3 (Exponent / Sign)",
        2: "Plane 2 (High Mantissa)",
        1: "Plane 1 (Mid Mantissa)",
        0: "Plane 0 (Low Mantissa)"
    }
    for byte_idx in sorted(plane_names.keys(), reverse=True):
        ent = entropy_order_0(planes[byte_idx])
        print(f"{plane_names[byte_idx]:<30}: {ent:.6f} bits/byte")
        
if __name__ == '__main__':
    main()
