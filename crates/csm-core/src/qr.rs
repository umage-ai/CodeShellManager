//! QR codes for text that CSM draws (the add-on screen's `qr` node, the peer pairing link).
//!
//! Pure: a string in, a grid of modules out. Drawing is `csm-ui`'s job. Encoding never panics:
//! a text the encoder refuses gives `None`.

use qrcodegen::{QrCode, QrCodeEcc};

/// The longest text a `qr` node accepts, in bytes. The worst pairing link the phone remote can
/// offer is 615 bytes (`docs/design/phone-remote.md`, section 8.2), so 1024 leaves room.
pub const MAX_QR_TEXT: usize = 1024;

/// The error correction a code is asked for. The encoder may raise the level when the text fits,
/// so the matrix carries at least this much correction, never less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecc {
    Low,
    Medium,
}

/// A square of modules, row by row. `true` is dark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matrix {
    size: usize,
    dark: Vec<bool>,
}

impl Matrix {
    /// The side length in modules.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Whether the module at `(x, y)` is dark. Outside the square, `false`.
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        if x >= self.size || y >= self.size {
            return false;
        }
        self.dark.get(y * self.size + x).copied().unwrap_or(false)
    }

    /// The modules row by row, `size * size` of them.
    pub fn modules(&self) -> &[bool] {
        &self.dark
    }
}

/// The code for `text` at (at least) `ecc`. `None` when the encoder refuses the text (too long).
pub fn encode(text: &str, ecc: Ecc) -> Option<Matrix> {
    let level = match ecc {
        Ecc::Low => QrCodeEcc::Low,
        Ecc::Medium => QrCodeEcc::Medium,
    };
    let code = QrCode::encode_text(text, level).ok()?;
    let size = usize::try_from(code.size()).ok()?;
    let mut dark = Vec::with_capacity(size.checked_mul(size)?);
    for y in 0..size {
        for x in 0..size {
            dark.push(code.get_module(i32::try_from(x).ok()?, i32::try_from(y).ok()?));
        }
    }
    Some(Matrix { size, dark })
}

/// Why a `qr` node's text is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextError {
    Empty,
    TooLong,
    Control,
    TooBig,
}

/// Checks a `qr` node's text (1 to [`MAX_QR_TEXT`] bytes, no control characters) and encodes it at
/// medium error correction. Every refusal is a value, never a panic.
pub fn node_code(text: &str) -> Result<Matrix, TextError> {
    if text.is_empty() {
        return Err(TextError::Empty);
    }
    if text.len() > MAX_QR_TEXT {
        return Err(TextError::TooLong);
    }
    if text.chars().any(char::is_control) {
        return Err(TextError::Control);
    }
    encode(text, Ecc::Medium).ok_or(TextError::TooBig)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A relay URL of the largest size the pairing offer allows (256 bytes), inside an example
    /// origin: the worst case the `qr` text cap is sized for.
    fn worst_link() -> String {
        let relay = format!("wss://relay.example.org/{}", "r".repeat(256 - "wss://relay.example.org/".len()));
        let payload = format!("{{\"v\":1,\"r\":\"{relay}\",\"h\":\"{}\",\"k\":\"{}\",\"i\":\"p_{}\",\"p\":\"{}\"}}", "h".repeat(43), "k".repeat(43), "i".repeat(20), "p".repeat(43));
        let body = base64_url(payload.as_bytes());
        format!("https://phone.example.org/v/0.1.0/#p={body}")
    }

    fn base64_url(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for c in bytes.chunks(3) {
            let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..=c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn the_worst_pairing_link_fits_the_cap() {
        let link = worst_link();
        assert!(link.len() <= MAX_QR_TEXT && link.len() > 512, "the worst case is over 512 bytes: {}", link.len());
        let m = node_code(&link).expect("the worst case must encode");
        assert!(m.size() >= 21 && m.size() <= 177);
        assert!(m.is_dark(0, 0) && m.is_dark(m.size() - 1, 0) && m.is_dark(0, m.size() - 1), "the finder patterns");
    }

    #[test]
    fn a_code_is_at_least_medium_correction() {
        let m = encode("https://example.org/x", Ecc::Low).unwrap();
        let med = encode("https://example.org/x", Ecc::Medium).unwrap();
        assert!(med.size() >= m.size(), "a higher level never shrinks the code");
    }

    #[test]
    fn refusals_are_values() {
        assert_eq!(node_code(""), Err(TextError::Empty));
        assert_eq!(node_code(&"a".repeat(MAX_QR_TEXT + 1)), Err(TextError::TooLong));
        assert_eq!(node_code("ok\u{0}bad"), Err(TextError::Control));
        assert_eq!(node_code("tab\there"), Err(TextError::Control));
        assert!(node_code(&"a".repeat(MAX_QR_TEXT)).is_ok(), "the cap itself is accepted");
    }

    #[test]
    fn modules_outside_the_square_are_light() {
        let m = encode("x", Ecc::Low).unwrap();
        assert!(!m.is_dark(m.size(), 0));
        assert!(!m.is_dark(0, m.size()));
        assert_eq!(m.modules().len(), m.size() * m.size());
    }
}
