//! CRC-32 (IEEE 802.3, reflected, `0xEDB88320`) — the checksum MCAP specifies, in fifteen
//! lines instead of a dependency. Table-driven, built once on first use.
//!
//! ```
//! assert_eq!(ferroscope_mcap::crc32(b"123456789"), 0xCBF43926);
//! ```

/// Incremental CRC-32, so a writer can checksum a section it is streaming out without
/// buffering the section.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

const POLY: u32 = 0xEDB8_8320;

/// The 256-entry table, computed at compile time. `const fn` keeps it out of the binary's
/// initialization path and out of a `lazy_static`-shaped dependency.
const TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    pub const fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    pub fn update(&mut self, bytes: &[u8]) {
        let mut c = self.0;
        for &b in bytes {
            c = TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    pub fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

/// One-shot CRC-32 of a whole buffer.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(bytes);
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        // The check value every CRC-32/ISO-HDLC implementation agrees on.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let mut c = Crc32::new();
        for chunk in data.chunks(37) {
            c.update(chunk);
        }
        assert_eq!(c.finish(), crc32(&data));
    }
}
