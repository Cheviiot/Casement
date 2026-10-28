//! Physical key translation: evdev+8 on Linux, Win32 VK on Windows.

use gtk::gdk::Key;

pub fn evdev(key: Key, hardware: u32) -> Option<u32> {
    #[cfg(windows)]
    return windows_evdev(key, hardware, |vk| {
        #[link(name = "user32")]
        extern "system" {
            #[link_name = "GetKeyboardLayout"]
            fn keyboard_layout(thread: u32) -> *mut std::ffi::c_void;
            #[link_name = "MapVirtualKeyExW"]
            fn virtual_key(vk: u32, mode: u32, layout: *mut std::ffi::c_void) -> u32;
        }
        // GDK already distinguishes right-hand modifiers. Use this GTK thread's
        // active layout; mode4 preserves E0/E1 in the returned scan code.
        unsafe { virtual_key(vk, 4, keyboard_layout(0)) }
    });
    #[cfg(not(windows))]
    {
        let _ = key;
        hardware.checked_sub(8)
    }
}

#[cfg(any(windows, test))]
fn windows_evdev(key: Key, hardware: u32, scan: impl FnOnce(u32) -> u32) -> Option<u32> {
    // VK_RETURN and keypad navigation VKs lose their location. GDK retains it in
    // the keypad keyval, including when Num Lock is off.
    let keypad = match key {
        Key::KP_0 | Key::KP_Insert => Some(82),
        Key::KP_1 | Key::KP_End => Some(79),
        Key::KP_2 | Key::KP_Down => Some(80),
        Key::KP_3 | Key::KP_Page_Down => Some(81),
        Key::KP_4 | Key::KP_Left => Some(75),
        Key::KP_5 | Key::KP_Begin => Some(76),
        Key::KP_6 | Key::KP_Right => Some(77),
        Key::KP_7 | Key::KP_Home => Some(71),
        Key::KP_8 | Key::KP_Up => Some(72),
        Key::KP_9 | Key::KP_Page_Up => Some(73),
        Key::KP_Decimal | Key::KP_Delete => Some(83),
        Key::KP_Enter => Some(96),
        Key::KP_Divide => Some(98),
        Key::KP_Multiply => Some(55),
        Key::KP_Subtract => Some(74),
        Key::KP_Add => Some(78),
        Key::KP_Equal => Some(117),
        _ => None,
    };
    keypad.or_else(|| match hardware {
        0x13 => Some(119), // Pause uses E1, not the Num Lock scan at the same low byte.
        0x90 => Some(69),  // Num Lock may be reported as an extended scan by Win32.
        0x2c => Some(99),  // Print Screen.
        _ => casement_media::keyboard::evdev(scan(hardware)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_vk_letters_and_right_modifiers_use_physical_scans() {
        assert_eq!(
            windows_evdev(Key::a, 0x41, |vk| {
                assert_eq!(vk, 0x41);
                0x1e
            }),
            Some(30)
        );
        assert_eq!(windows_evdev(Key::Control_R, 0xa3, |_| 0xe01d), Some(97));
        assert_eq!(windows_evdev(Key::Shift_R, 0xa1, |_| 0x36), Some(54));
        assert_eq!(windows_evdev(Key::Alt_R, 0xa5, |_| 0xe038), Some(100));
    }

    #[test]
    fn windows_keypad_keeps_its_location_with_num_lock_off() {
        assert_eq!(windows_evdev(Key::KP_Enter, 0x0d, |_| 0x1c), Some(96));
        assert_eq!(windows_evdev(Key::KP_End, 0x23, |_| 0xe04f), Some(79));
        assert_eq!(windows_evdev(Key::KP_Delete, 0x2e, |_| 0xe053), Some(83));
        assert_eq!(windows_evdev(Key::Num_Lock, 0x90, |_| 0xe045), Some(69));
    }
}
