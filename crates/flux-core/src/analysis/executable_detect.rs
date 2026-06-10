//! Magic-byte detection for common executable formats (PE, ELF, Mach-O).
//!
//! Specifically checks for x86 and x86-64 target architectures to support BCJ filters.

/// Returns true if the buffer starts with a valid PE (Windows) executable signature
/// targeting x86 or x86-64.
pub fn is_pe_x86_or_x64(buf: &[u8]) -> bool {
    // A PE file must be at least 64 bytes to contain the PE header offset at 0x3C.
    if buf.len() < 64 {
        return false;
    }

    // Must start with "MZ"
    if buf[0] != 0x4D || buf[1] != 0x5A {
        return false;
    }

    // Offset of the PE header is stored as a little-endian u32 at 0x3C.
    let pe_offset = u32::from_le_bytes([buf[0x3C], buf[0x3D], buf[0x3E], buf[0x3F]]) as usize;

    // Verify PE header bounds and signature "PE\0\0"
    if pe_offset + 4 > buf.len() {
        return false;
    }

    buf[pe_offset] == 0x50
        && buf[pe_offset + 1] == 0x45
        && buf[pe_offset + 2] == 0x00
        && buf[pe_offset + 3] == 0x00
}

/// Returns true if the buffer starts with a valid ELF (Linux/Unix) executable signature
/// targeting x86 or x86-64.
pub fn is_elf_x86_or_x64(buf: &[u8]) -> bool {
    // ELF header must be at least 20 bytes to check the e_machine field at bytes 18-19.
    if buf.len() < 20 {
        return false;
    }

    // Magic bytes: 0x7F, 'E', 'L', 'F'
    if buf[0] != 0x7F || buf[1] != 0x45 || buf[2] != 0x4C || buf[3] != 0x46 {
        return false;
    }

    // EI_CLASS (byte 4): 1 = ELF32, 2 = ELF64
    if buf[4] != 1 && buf[4] != 2 {
        return false;
    }

    // EI_DATA (byte 5): 1 = little-endian, 2 = big-endian
    let data_encoding = buf[5];
    if data_encoding != 1 && data_encoding != 2 {
        return false;
    }

    // e_machine at offset 18-19: 0x0003 for x86, 0x003E for x86-64
    let machine_bytes = [buf[18], buf[19]];
    if data_encoding == 1 {
        // Little endian
        machine_bytes == [0x03, 0x00] || machine_bytes == [0x3E, 0x00]
    } else {
        // Big endian
        machine_bytes == [0x00, 0x03] || machine_bytes == [0x00, 0x3E]
    }
}

/// Returns true if the buffer starts with a valid Mach-O (macOS) executable signature
/// targeting x86 or x86-64.
pub fn is_mach_o_x86_or_x64(buf: &[u8]) -> bool {
    // Mach-O header must be at least 8 bytes to check the CPU type at bytes 4-7.
    if buf.len() < 8 {
        return false;
    }

    let magic = [buf[0], buf[1], buf[2], buf[3]];

    // Magic signatures
    // 0xFEEDFACE (32-bit BE), 0xCEFAEDFE (32-bit LE)
    // 0xFEEDFACF (64-bit BE), 0xCFFAEDFE (64-bit LE)
    let is_be = magic == [0xFE, 0xED, 0xFA, 0xCE] || magic == [0xFE, 0xED, 0xFA, 0xCF];
    let is_le = magic == [0xCE, 0xFA, 0xED, 0xFE] || magic == [0xCF, 0xFA, 0xED, 0xFE];

    if !is_be && !is_le {
        return false;
    }

    // CPU type is at bytes 4-7
    let cpu_type = if is_be {
        u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]])
    } else {
        u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]])
    };

    // CPU_TYPE_I386 = 7, CPU_TYPE_X86_64 = 7 | 0x01000000 = 0x01000007
    cpu_type == 7 || cpu_type == 0x01000007
}
