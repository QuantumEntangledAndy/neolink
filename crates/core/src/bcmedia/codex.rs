//! Handles sending and recieving messages as packets
//!
//! BcMediaCodex is used with a `[tokio_util::codec::Framed]` to form complete packets
//!
use crate::bcmedia::de::find_next_bcmedia_magic;
use crate::bcmedia::model::*;
use crate::{Error, Result};
use bytes::{Buf, BytesMut};
use log::*;
use tokio_util::codec::{Decoder, Encoder};

/// Codec for BcMedia framed stream (livestream and replay). Use `new(false)` for replay when the stream may have leading junk (e.g. 32-byte header).
pub struct BcMediaCodex {
    /// If true we will not search for the start of the next packet
    /// in the event that the stream appears to be corrupted
    strict: bool,
    amount_skipped: usize,
}

impl BcMediaCodex {
    /// Create a new BcMedia codec. Use strict=false for replay/streams that may have leading junk.
    pub fn new(strict: bool) -> Self {
        Self {
            strict,
            amount_skipped: 0,
        }
    }
}

impl Encoder<BcMedia> for BcMediaCodex {
    type Error = Error;

    fn encode(&mut self, item: BcMedia, dst: &mut BytesMut) -> Result<()> {
        let buf: Vec<u8> = Default::default();
        let buf = item.serialize(buf)?;
        dst.extend_from_slice(buf.as_slice());
        Ok(())
    }
}

impl Decoder for BcMediaCodex {
    type Item = BcMedia;
    type Error = Error;

    /// Since frames can cross EOF boundaries we overload this so it doesn't error if
    /// there are bytes left on the stream
    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>> {
        match self.decode(buf)? {
            Some(frame) => Ok(Some(frame)),
            None => Ok(None),
        }
    }

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>> {
        loop {
            match BcMedia::deserialize(src) {
                Ok(bc) => {
                    if self.amount_skipped > 0 {
                        trace!("Amount skipped to restore stream: {}", self.amount_skipped);
                        self.amount_skipped = 0;
                    }
                    return Ok(Some(bc));
                }
                Err(Error::NomIncomplete(_)) => {
                    if self.amount_skipped > 0 {
                        trace!("Amount skipped to restore stream: {}", self.amount_skipped);
                        self.amount_skipped = 0;
                    }
                    return Ok(None);
                }
                Err(e) => {
                    if self.strict {
                        return Err(e);
                    } else if src.len() < 4 {
                        return Ok(None);
                    } else {
                        if self.amount_skipped == 0 {
                            trace!("Error in stream attempting to restore: {:?}", e);
                        }
                        // Prefer resync to next 8-byte-aligned BcMedia magic when found; else advance 1 byte
                        // so we don't skip past the next real frame (advancing 8 when no magic discarded most of the stream).
                        let skip = find_next_bcmedia_magic(src)
                            .unwrap_or(1)
                            .min(src.len())
                            .max(1);
                        self.amount_skipped += skip;
                        src.advance(skip);
                        continue;
                    }
                }
            }
        }
    }
}
