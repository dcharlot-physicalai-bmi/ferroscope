//! Chunk decompression — optional, and pure Rust on purpose.
//!
//! An MCAP chunk stores its records under one of a small set of codecs. Uncompressed chunks are a
//! conforming profile and this crate writes those, but almost nothing else does: `ros2 bag record`
//! and Foxglove write zstd by default. Reading only what we write is not interoperability.
//!
//! The codecs are behind features so the default build keeps its zero-dependency promise. What
//! makes them usable at all is that both decoders are **pure Rust**: the reference `mcap` crate
//! binds C zstd and lz4, which is precisely what keeps that tooling off `wasm32`. These build for
//! wasm unchanged, so a browser can open a real robot log.

use crate::{Error, Result};
use std::borrow::Cow;

/// Ceiling on the pre-allocation taken from a chunk header.
///
/// `uncompressed_size` is data, not a fact: a corrupt or hostile file can claim `u64::MAX` and a
/// reader that trusts it asks the allocator for that before decoding a single byte. The hint only
/// saves regrowth, so capping it costs a few reallocations on genuinely large chunks and removes
/// the failure mode entirely.
const MAX_HINT: usize = 64 << 20;

/// The records inside a chunk, borrowed when stored plainly and owned when decoded.
pub(crate) fn chunk_records<'a>(
    compression: &str,
    stored: &'a [u8],
    uncompressed_size: u64,
) -> Result<Cow<'a, [u8]>> {
    if compression.is_empty() || compression == "none" {
        return Ok(Cow::Borrowed(stored));
    }
    #[cfg_attr(
        not(any(feature = "zstd", feature = "lz4")),
        allow(unused_variables, unused_mut)
    )]
    let hint = (uncompressed_size as usize).min(MAX_HINT);
    match compression {
        #[cfg(feature = "zstd")]
        "zstd" => {
            use std::io::Read;
            let mut out = Vec::with_capacity(hint);
            let mut d =
                ruzstd::decoding::StreamingDecoder::new(stored).map_err(|e| Error::Decompress {
                    codec: "zstd",
                    detail: e.to_string(),
                })?;
            d.read_to_end(&mut out).map_err(|e| Error::Decompress {
                codec: "zstd",
                detail: e.to_string(),
            })?;
            Ok(Cow::Owned(out))
        }
        #[cfg(feature = "lz4")]
        "lz4" => {
            use std::io::Read;
            let mut out = Vec::with_capacity(hint);
            let mut d = lz4_flex::frame::FrameDecoder::new(stored);
            d.read_to_end(&mut out).map_err(|e| Error::Decompress {
                codec: "lz4",
                detail: e.to_string(),
            })?;
            Ok(Cow::Owned(out))
        }
        _ => Err(Error::UnsupportedCompression(compression.to_string())),
    }
}

/// What this build can actually decode. Named in the error, because "unsupported" without the
/// available set sends a reader to the source to find out whether it is a missing feature or a
/// codec nobody implemented.
pub(crate) const fn available() -> &'static str {
    match (cfg!(feature = "zstd"), cfg!(feature = "lz4")) {
        (true, true) => "none, zstd, lz4",
        (true, false) => "none, zstd",
        (false, true) => "none, lz4",
        (false, false) => "none",
    }
}
