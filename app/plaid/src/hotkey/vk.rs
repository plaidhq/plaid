//! Windows virtual keys in macOS key codes (the settings' namespace). Pure, so it is tested on any host.
use super::{ENTER, MODIFIERS};

/// (virtual key, macOS key code). The low-level hook reports left/right-specific modifier keys.
const KEYS: [(u32, i32); 30] = [
    (0x5C, 54), // VK_RWIN → right ⌘
    (0x5B, 55), // VK_LWIN → left ⌘
    (0xA0, 56), // VK_LSHIFT
    (0xA1, 60), // VK_RSHIFT
    (0xA4, 58), // VK_LMENU → left ⌥
    (0xA5, 61), // VK_RMENU → right ⌥
    (0xA2, 59), // VK_LCONTROL
    (0xA3, 62), // VK_RCONTROL
    (0x1B, 53), // VK_ESCAPE
    (0x0D, 36), // VK_RETURN (main block)
    // F1–F20.
    (0x70, 122),
    (0x71, 120),
    (0x72, 99),
    (0x73, 118),
    (0x74, 96),
    (0x75, 97),
    (0x76, 98),
    (0x77, 100),
    (0x78, 101),
    (0x79, 109),
    (0x7A, 103),
    (0x7B, 111),
    (0x7C, 105),
    (0x7D, 107),
    (0x7E, 113),
    (0x7F, 106),
    (0x80, 64),
    (0x81, 79),
    (0x82, 80),
    (0x83, 90),
];

/// Unmapped keys get codes past macOS's range, so they still count as "another key" in the gesture.
const UNMAPPED: i32 = 0x1000;
pub const LCONTROL: u32 = 0xA2;
/// AltGr layouts send a fake left Ctrl with this scan code before right Alt.
pub const ALTGR_CTRL_SCAN: u32 = 0x21D;
/// Unassigned virtual key: sent around the hotkey's Alt/Win press so its release doesn't open the menu bar or Start.
pub const MASK_KEY: u16 = 0xE8;

pub fn code(vk: u32, extended: bool) -> i32 {
    if vk == 0x0D && extended {
        return ENTER;
    }
    KEYS.iter()
        .find(|k| k.0 == vk)
        .map_or(UNMAPPED + vk as i32, |k| k.1)
}

/// A modifier other than key `code` is held; `held(vk)` reads the key state outside the current event.
pub fn other(code: i32, held: impl Fn(u32) -> bool) -> bool {
    KEYS.iter()
        .any(|&(vk, k)| k != code && MODIFIERS.contains(&k) && held(vk))
}

/// Alt and Win released alone trigger the menu bar / Start menu.
pub fn needs_mask(code: i32) -> bool {
    matches!(code, 54 | 55 | 58 | 61)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_virtual_keys() {
        assert_eq!(code(0xA5, true), 61, "right Alt");
        assert_eq!(code(0x0D, false), 36);
        assert_eq!(code(0x0D, true), ENTER, "numpad Enter");
        assert_eq!(code(0x7C, false), 105, "F13");
        assert_eq!(code(0x41, false), UNMAPPED + 0x41);
        assert!(other(61, |vk| vk == 0xA0), "left Shift held");
        assert!(other(61, |vk| vk == 0xA4), "the other Alt counts");
        assert!(!other(61, |vk| vk == 0xA5), "own key");
        assert!(!other(61, |vk| vk == 0x41), "letters are not modifiers");
        assert!(needs_mask(61) && !needs_mask(62));
    }
}
