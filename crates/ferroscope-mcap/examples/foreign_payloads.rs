//! Hash the MESSAGE PAYLOADS of one recording, by streaming it.
//!
//! Counts and topic lists come from an MCAP summary, which sits OUTSIDE the chunks: a reader that
//! never decoded a single chunk still prints them correctly. Only the payloads prove decompression
//! happened, so this walks messages and folds their bytes.
use ferroscope_mcap::{Flow, Record, stream};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: foreign_payloads <file.mcap>");
    let f = std::fs::File::open(&path).expect("open");
    let (mut n, mut h) = (0u64, 0xcbf2_9ce4_8422_2325u64);
    stream(f, &mut |r: Record<'_>| {
        if let Record::Message(m) = r {
            n += 1;
            for b in m.data {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        Ok(Flow::Continue)
    })
    .expect("stream");
    println!("{n} messages  payload_fnv={h:016x}");
}
