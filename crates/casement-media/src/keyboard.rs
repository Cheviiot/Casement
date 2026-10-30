//! Shared physical key positions used by the Windows host and client.

/// Linux evdev key code → PC set 1 scancode and whether it needs the E0 prefix.
pub fn scancode(code: u32) -> Option<(u16, bool)> {
    Some(match code {
        // The main block, function keys and keypad: evdev numbers are set 1 scancodes.
        1..=83 | 86..=88 => (code as u16, false),
        96 => (0x1C, true),  // keypad Enter
        97 => (0x1D, true),  // right Ctrl
        98 => (0x35, true),  // keypad /
        99 => (0x37, true),  // Print Screen
        100 => (0x38, true), // right Alt
        102 => (0x47, true), // Home
        103 => (0x48, true), // Up
        104 => (0x49, true), // Page Up
        105 => (0x4B, true), // Left
        106 => (0x4D, true), // Right
        107 => (0x4F, true), // End
        108 => (0x50, true), // Down
        109 => (0x51, true), // Page Down
        110 => (0x52, true), // Insert
        111 => (0x53, true), // Delete
        125 => (0x5B, true), // left Windows
        126 => (0x5C, true), // right Windows
        127 => (0x5D, true), // Menu
        _ => return None,
    })
}

/// PC set 1 scan code to Linux evdev; None for unsupported positions.
pub fn evdev(scan: u32) -> Option<u32> {
    let prefix = scan >> 8;
    if !matches!(prefix, 0 | 0xe0) {
        return None;
    }
    (1..=127).find(|&code| scancode(code) == Some(((scan & 0xff) as u16, prefix == 0xe0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_positions_round_trip_without_losing_extended_keys() {
        for code in 1..=127 {
            if let Some((scan, extended)) = scancode(code) {
                assert_eq!(
                    evdev(u32::from(scan) | if extended { 0xe000 } else { 0 }),
                    Some(code)
                );
            }
        }
        assert_eq!(evdev(0xe01d), Some(97));
        assert_eq!(evdev(0), None);
    }
}
