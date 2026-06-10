//! BCJ (Branch/Call/Jump) x86 Preprocessing Filter.
//!
//! Implements the x86 executable filter to normalize CALL (0xE8) and
//! JMP (0xE9) relative addresses into absolute ones.
//!
//! This improves LZ77/entropy compression ratios on x86/x86_64 executables.
//!
//! Conceptually inspired by Igor Pavlov (7-Zip) and Lasse Collin (xz).

/// Normalizes x86 relative addresses to absolute ones.
/// Operates in place.
#[inline]
pub fn bcj_x86_forward(buf: &mut [u8]) {
    if buf.len() < 5 {
        return;
    }
    let limit = buf.len() - 5;
    let mut i = 0;
    while i <= limit {
        if buf[i] == 0xE8 || buf[i] == 0xE9 {
            // Read relative address as little-endian i32
            let rel = i32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]);
            // Convert to absolute
            let abs = rel.wrapping_add(i as i32).wrapping_add(5) as u32;
            // Write back absolute address
            let bytes = abs.to_le_bytes();
            buf[i + 1] = bytes[0];
            buf[i + 2] = bytes[1];
            buf[i + 3] = bytes[2];
            buf[i + 4] = bytes[3];
            i += 5;
        } else {
            i += 1;
        }
    }
}

/// Denormalizes x86 absolute addresses back to relative ones.
/// Operates in place.
#[inline]
pub fn bcj_x86_inverse(buf: &mut [u8]) {
    if buf.len() < 5 {
        return;
    }
    let limit = buf.len() - 5;
    let mut i = 0;
    while i <= limit {
        if buf[i] == 0xE8 || buf[i] == 0xE9 {
            // Read absolute address as little-endian u32
            let abs = u32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]);
            // Convert to relative
            let rel = (abs as i32).wrapping_sub(i as i32).wrapping_sub(5);
            // Write back relative address
            let bytes = rel.to_le_bytes();
            buf[i + 1] = bytes[0];
            buf[i + 2] = bytes[1];
            buf[i + 3] = bytes[2];
            buf[i + 4] = bytes[3];
            i += 5;
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1. Empty buffer: bcj_x86_forward(&mut []) does nothing, doesn't panic.
    #[test]
    fn test_empty_buffer() {
        let mut buf = [];
        bcj_x86_forward(&mut buf);
        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, []);
    }

    // 2. Buffer too short (< 5 bytes): no transform, doesn't panic.
    #[test]
    fn test_short_buffer() {
        let mut buf = [0xE8, 0x01, 0x02, 0x03];
        let original = buf;
        bcj_x86_forward(&mut buf);
        assert_eq!(buf, original);
        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 3. Buffer with no E8/E9: no transform (output == input).
    #[test]
    fn test_no_opcodes() {
        let mut buf = [0x90, 0x90, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00];
        let original = buf;
        bcj_x86_forward(&mut buf);
        assert_eq!(buf, original);
        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 4. Buffer with one CALL (E8) at position 0: verify address is transformed and restored.
    #[test]
    fn test_single_call() {
        let mut buf = [0xE8, 0x10, 0x00, 0x00, 0x00, 0x90, 0x90];
        let original = buf;

        // Relative target is 16 (0x10)
        // absolute = 16 + 0 + 5 = 21 (0x15)
        bcj_x86_forward(&mut buf);
        assert_eq!(buf[0], 0xE8);
        assert_eq!(buf[1..5], [0x15, 0x00, 0x00, 0x00]);

        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 5. Buffer with one JMP (E9) at position 0: same as above.
    #[test]
    fn test_single_jmp() {
        let mut buf = [0xE9, 0x20, 0x00, 0x00, 0x00, 0x90, 0x90];
        let original = buf;

        // Relative target is 32 (0x20)
        // absolute = 32 + 0 + 5 = 37 (0x25)
        bcj_x86_forward(&mut buf);
        assert_eq!(buf[0], 0xE9);
        assert_eq!(buf[1..5], [0x25, 0x00, 0x00, 0x00]);

        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 6. Buffer with E8/E9 NEAR THE END such that fewer than 5 bytes remain: not transformed.
    #[test]
    fn test_near_end() {
        let mut buf = [0x90, 0x90, 0xE8, 0x01, 0x02, 0x03]; // E8 is at index 2, buf has length 6. Index 2 + 5 = 7 > 6.
        let original = buf;
        bcj_x86_forward(&mut buf);
        assert_eq!(buf, original);
        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 7. Buffer with multiple CALLs at varying positions: byte-perfect roundtrip.
    #[test]
    fn test_multiple_calls() {
        let mut buf = vec![
            0xE8, 0x05, 0x00, 0x00, 0x00, // CALL 5 at pos 0 (abs = 5 + 0 + 5 = 10)
            0x90, 0x90,                   // NOPs
            0xE8, 0x10, 0x00, 0x00, 0x00, // CALL 16 at pos 7 (abs = 16 + 7 + 5 = 28)
            0x90,
        ];
        let original = buf.clone();
        bcj_x86_forward(&mut buf);
        assert_eq!(buf[0], 0xE8);
        assert_eq!(buf[1..5], [10, 0, 0, 0]);
        assert_eq!(buf[7], 0xE8);
        assert_eq!(buf[8..12], [28, 0, 0, 0]);

        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 8. Random buffer of 1024 bytes with scattered E8/E9: roundtrip matches original.
    // We use a deterministic LCG pseudo-random sequence for reproducibility.
    #[test]
    fn test_pseudo_random_buffer() {
        let mut buf = vec![0u8; 1024];
        let mut state = 42u32;
        for byte in buf.iter_mut() {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            *byte = (state >> 16) as u8;
        }
        let original = buf.clone();

        bcj_x86_forward(&mut buf);
        // Ensure some E8/E9 was actually in the buffer to make the test meaningful
        assert!(original.contains(&0xE8) || original.contains(&0xE9));
        assert_ne!(buf, original); // should be modified

        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 9. A small real-ish test pattern representing a mock x86 function.
    #[test]
    fn test_realish_x86_pattern() {
        let mut buf = vec![
            0x55,                   // push ebp
            0x89, 0xE5,             // mov ebp, esp
            0xE8, 0x20, 0x00, 0x00, 0x00, // call function_1 (rel offset 0x20)
            0x8B, 0x45, 0x08,       // mov eax, [ebp + 8]
            0xE9, 0x80, 0xFF, 0xFF, 0xFF, // jmp loop_start (rel offset -128)
            0x5D,                   // pop ebp
            0xC3,                   // ret
        ];
        let original = buf.clone();

        bcj_x86_forward(&mut buf);
        assert_ne!(buf, original);

        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }

    // 10. Worst-case: buffer of all 0xE8 bytes. Verify roundtrip.
    #[test]
    fn test_worst_case_all_e8() {
        let mut buf = vec![0xE8; 100];
        let original = buf.clone();

        bcj_x86_forward(&mut buf);
        bcj_x86_inverse(&mut buf);
        assert_eq!(buf, original);
    }
}
