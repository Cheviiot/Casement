//! Unique flat colours for the private input-to-paint fixture, not the wire protocol.

pub fn color(code: u32) -> [u8; 3] {
    [
        (20 + code % 4 * 70) as u8,
        (20 + code / 4 % 4 * 70) as u8,
        (20 + code / 16 % 4 * 70) as u8,
    ]
}
