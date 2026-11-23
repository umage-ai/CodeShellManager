//! Length-prefixed framing on the TCP stream: `u32 BE length || payload`. After the handshake
//! the payload is exactly one Noise message (at most [`MAX_FRAME_LEN`] bytes); during the
//! handshake it is a handshake message. The length is checked as soon as the four header bytes
//! are in, before any payload is buffered, so a hostile length never allocates.

use crate::limits::MAX_FRAME_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// A zero-length frame: no Noise message is empty (the tag alone is 16 bytes).
    Empty,
    /// The length is above [`MAX_FRAME_LEN`] (the value is what the peer claimed or tried).
    TooLarge(usize),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty frame"),
            Self::TooLarge(n) => write!(f, "frame of {n} bytes is over the {MAX_FRAME_LEN} byte limit"),
        }
    }
}

impl std::error::Error for FrameError {}

/// `u32 BE length || payload`.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// An incremental decoder for a byte stream. Feed it what the socket gave, then call
/// [`FrameDecoder::next_frame`] until it returns `Ok(None)`. After an error the stream is out of
/// step and the decoder keeps returning that error: close the connection.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    failed: Option<FrameError>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        if self.failed.is_none() {
            self.buf.extend_from_slice(data);
        }
    }

    /// Bytes held that are not yet a whole frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        let Some(header) = self.buf.first_chunk::<4>() else {
            return Ok(None);
        };
        let len = u32::from_be_bytes(*header) as usize;
        if len == 0 {
            return Err(self.fail(FrameError::Empty));
        }
        if len > MAX_FRAME_LEN {
            return Err(self.fail(FrameError::TooLarge(len)));
        }
        let end = 4 + len;
        if self.buf.len() < end {
            return Ok(None);
        }
        let payload = self.buf.get(4..end).map(<[u8]>::to_vec).unwrap_or_default();
        self.buf.drain(..end);
        Ok(Some(payload))
    }

    fn fail(&mut self, e: FrameError) -> FrameError {
        self.failed = Some(e);
        self.buf = Vec::new();
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_across_any_split() {
        let a = encode_frame(b"hello").unwrap();
        let b = encode_frame(&[9u8; 300]).unwrap();
        let all = [a, b].concat();
        for cut in 0..=all.len() {
            let mut d = FrameDecoder::new();
            d.push(&all[..cut]);
            let mut got = Vec::new();
            while let Some(f) = d.next_frame().unwrap() {
                got.push(f);
            }
            d.push(&all[cut..]);
            while let Some(f) = d.next_frame().unwrap() {
                got.push(f);
            }
            assert_eq!(got, vec![b"hello".to_vec(), vec![9u8; 300]], "cut at {cut}");
            assert_eq!(d.buffered(), 0);
        }
    }

    #[test]
    fn empty_and_oversize_are_refused_on_both_sides() {
        assert_eq!(encode_frame(&[]), Err(FrameError::Empty));
        assert_eq!(encode_frame(&vec![0; MAX_FRAME_LEN + 1]), Err(FrameError::TooLarge(MAX_FRAME_LEN + 1)));
        assert!(encode_frame(&vec![0; MAX_FRAME_LEN]).is_ok());

        let mut d = FrameDecoder::new();
        d.push(&[0, 0, 0, 0]);
        assert_eq!(d.next_frame(), Err(FrameError::Empty));
        let mut d = FrameDecoder::new();
        d.push(&(u32::MAX).to_be_bytes());
        assert_eq!(d.next_frame(), Err(FrameError::TooLarge(u32::MAX as usize)), "refused on the header alone");
        assert_eq!(d.buffered(), 0);
        d.push(&[1, 2, 3]);
        assert_eq!(d.next_frame(), Err(FrameError::TooLarge(u32::MAX as usize)), "an error is sticky");
        let mut d = FrameDecoder::new();
        d.push(&((MAX_FRAME_LEN as u32) + 1).to_be_bytes());
        assert!(matches!(d.next_frame(), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn a_frame_at_the_limit_decodes() {
        let f = encode_frame(&vec![5; MAX_FRAME_LEN]).unwrap();
        let mut d = FrameDecoder::new();
        d.push(&f);
        assert_eq!(d.next_frame().unwrap().map(|p| p.len()), Some(MAX_FRAME_LEN));
    }
}
