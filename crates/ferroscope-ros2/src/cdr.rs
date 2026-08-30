//! CDR, the wire format ROS 2 puts inside an MCAP message.
//!
//! # The one detail that decides everything
//!
//! A payload opens with a four-byte encapsulation header: two bytes of representation id
//! (`0x0001` little-endian, `0x0000` big-endian) and two of options. Every primitive after it is
//! aligned to its own width — and the alignment is measured **from the end of that header**, not
//! from the start of the buffer.
//!
//! Getting that backwards does not raise anything. Measured on a real `sensor_msgs/JointState`
//! recorded by `mcap-ros2-support`, the wrong origin decodes `position` as `[0.0, -0.0, 0.0]`
//! instead of `[0.0, 0.8, 0.0]` and leaves 36 of 164 bytes unread: plausible numbers, silently
//! wrong. That is why [`Cdr::finish`] insists the whole payload was consumed — a length check is
//! the cheap invariant that turns a silent misread into an error.

use crate::Error;

/// A cursor over one CDR message.
pub struct Cdr<'a> {
    buf: &'a [u8],
    pos: usize,
    little: bool,
    /// Where alignment is measured from: the end of the encapsulation header.
    origin: usize,
}

macro_rules! prim {
    ($name:ident, $t:ty, $n:literal) => {
        pub fn $name(&mut self) -> Result<$t, Error> {
            self.align($n)?;
            let b = self.take($n)?;
            let a: [u8; $n] = b.try_into().expect("take returned the requested length");
            Ok(if self.little {
                <$t>::from_le_bytes(a)
            } else {
                <$t>::from_be_bytes(a)
            })
        }
    };
}

impl<'a> Cdr<'a> {
    pub fn new(buf: &'a [u8]) -> Result<Self, Error> {
        if buf.len() < 4 {
            return Err(Error::Short {
                want: 4,
                have: buf.len(),
            });
        }
        // Representation id: 0x0000/0x0001 plain CDR, 0x0002/0x0003 parameter-list CDR (used by
        // DDS for keyed topics). Only plain CDR is decoded here; the others have a different
        // body entirely and guessing at them would produce numbers rather than an error.
        let little = match (buf[0], buf[1]) {
            (0x00, 0x00) => false,
            (0x00, 0x01) => true,
            (a, b) => return Err(Error::Encapsulation(u16::from_be_bytes([a, b]))),
        };
        Ok(Self {
            buf,
            pos: 4,
            little,
            origin: 4,
        })
    }

    fn align(&mut self, n: usize) -> Result<(), Error> {
        let rel = self.pos - self.origin;
        let pad = (n - (rel % n)) % n;
        if self.pos + pad > self.buf.len() {
            return Err(Error::Short {
                want: pad,
                have: self.buf.len() - self.pos,
            });
        }
        self.pos += pad;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Short {
                want: n,
                have: self.buf.len() - self.pos,
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    prim!(u16, u16, 2);
    prim!(i16, i16, 2);
    prim!(u32, u32, 4);
    prim!(i32, i32, 4);
    prim!(u64, u64, 8);
    prim!(i64, i64, 8);
    prim!(f32, f32, 4);
    prim!(f64, f64, 8);

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    pub fn i8(&mut self) -> Result<i8, Error> {
        Ok(self.take(1)?[0] as i8)
    }

    /// A string: a `u32` length that INCLUDES the trailing NUL, then the bytes.
    pub fn string(&mut self) -> Result<&'a str, Error> {
        let n = self.u32()? as usize;
        if n == 0 {
            return Ok("");
        }
        let raw = self.take(n)?;
        // The last byte is the NUL the length counted.
        let body = &raw[..n - 1];
        core::str::from_utf8(body).map_err(|_| Error::BadUtf8)
    }

    /// Skip `n` bytes of a type this decoder does not turn into numbers, keeping the cursor
    /// honest so the length check at the end still means something.
    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.take(n)?;
        Ok(())
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// Every byte must be accounted for.
    ///
    /// This is the check that turns a misread into an error instead of a plausible number. CDR
    /// pads the end of a message to a 4-byte boundary in some writers, so up to three unread
    /// bytes are tolerated; more than that means the definition and the bytes disagree.
    pub fn finish(&self) -> Result<(), Error> {
        let left = self.buf.len() - self.pos;
        if left > 3 {
            return Err(Error::Trailing {
                left,
                total: self.buf.len(),
            });
        }
        Ok(())
    }
}
