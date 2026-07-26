//! GDK key events → libghostty keys.
//!
//! libghostty names keys by physical position — the W3C `KeyboardEvent.code`,
//! which is what the Kitty keyboard protocol reports and what a keybinding on a
//! non-US layout has to be resolved against. GDK hands over both a logical
//! keyval and the hardware keycode, and on Linux that keycode is the XKB one:
//! the evdev scancode plus eight, on Wayland and on X11 alike.
//!
//! So the scancode is the answer for the keys a layout moves around — the
//! letters, digits and punctuation the W3C calls the writing system keys. For
//! everything else the keyval wins, because that is the only way an XKB option
//! like `caps:swapescape` can take effect at all: XKB implements it by handing
//! out a different keyval for the same scancode. Ghostty draws the line in the
//! same place, and for the same reason.
//!
//! The scancode table is Ghostty's own — `src/input/keycodes.zig`, itself taken
//! from Chromium's `dom_code_data.inc` — reduced to the XKB column and the keys
//! this enum names.

use gtk::gdk;
use gtk::prelude::*;
use tuni_vt::{Key, KeyAction, Mods};

pub fn mods_from_state(state: gdk::ModifierType) -> Mods {
    let mut mods = Mods::empty();
    if state.contains(gdk::ModifierType::SHIFT_MASK) {
        mods |= Mods::SHIFT;
    }
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        mods |= Mods::CTRL;
    }
    if state.contains(gdk::ModifierType::ALT_MASK) {
        mods |= Mods::ALT;
    }
    if state.contains(gdk::ModifierType::SUPER_MASK) {
        mods |= Mods::SUPER;
    }
    if state.contains(gdk::ModifierType::LOCK_MASK) {
        mods |= Mods::CAPS_LOCK;
    }
    mods
}

/// The modifiers a key event carries.
///
/// GDK reports the state as it stood *before* the event, so a press of Ctrl
/// does not have Ctrl in its own mask and a release still does. The key that
/// moved settles that, and it is also the only thing that says which side of
/// the keyboard the modifier was on — the state mask does not distinguish
/// them, and the Kitty protocol reports it.
pub fn mods_for_key(
    state: gdk::ModifierType,
    key: Key,
    action: KeyAction,
    num_locked: bool,
) -> Mods {
    let mut mods = mods_from_state(state);
    mods.set(Mods::NUM_LOCK, num_locked);

    let (modifier, side, right) = match key {
        Key::ShiftLeft => (Mods::SHIFT, Mods::SHIFT_SIDE, false),
        Key::ShiftRight => (Mods::SHIFT, Mods::SHIFT_SIDE, true),
        Key::ControlLeft => (Mods::CTRL, Mods::CTRL_SIDE, false),
        Key::ControlRight => (Mods::CTRL, Mods::CTRL_SIDE, true),
        Key::AltLeft => (Mods::ALT, Mods::ALT_SIDE, false),
        Key::AltRight => (Mods::ALT, Mods::ALT_SIDE, true),
        Key::MetaLeft => (Mods::SUPER, Mods::SUPER_SIDE, false),
        Key::MetaRight => (Mods::SUPER, Mods::SUPER_SIDE, true),
        _ => return mods,
    };
    mods.set(modifier, !matches!(action, KeyAction::Release));
    mods.set(side, right);
    mods
}

/// What the key types with nothing held down at all, on the layout this event
/// arrived under.
///
/// The Kitty protocol reports it, and it is what lets `Ctrl`+`С` on a Cyrillic
/// layout arrive as `Ctrl+C`: level 0 of the event's own group is the same key
/// on the same layout with no modifier applied.
pub fn unshifted_codepoint(display: &gdk::Display, event: &gdk::KeyEvent) -> Option<char> {
    let group = event.layout() as i32;
    display
        .map_keycode(event.keycode())?
        .into_iter()
        .find(|(entry, _)| entry.group() == group && entry.level() == 0)
        .and_then(|(_, keyval)| keyval.to_unicode())
}

/// The key a press was on.
///
/// A key outside the writing system is whatever the keymap says it is, because
/// swapping such a key is a thing users ask their keymap for and XKB grants it
/// by changing the keyval alone. Inside the writing system it is the other way
/// round: a layout moves the letters about, the position stays put, and the
/// position is what a binding resolves against and what the Kitty protocol
/// reports.
pub fn key_from_event(keyval: gdk::Key, keycode: u32) -> Key {
    let remapped = key_from_keyval(keyval);
    if remapped != Key::Unidentified && should_be_remappable(remapped) {
        return remapped;
    }
    match key_from_scancode(keycode) {
        Key::Unidentified => remapped,
        key => key,
    }
}

/// Whether the keymap has the last word on this key.
///
/// False for the writing system keys of the W3C's §3.1.1, which are the ones
/// that "change meaning based on the current locale and keyboard layout" and
/// so have to be named by position; true for everything else, which is
/// harmless to let the user move around.
fn should_be_remappable(key: Key) -> bool {
    !matches!(
        key,
        Key::Backquote
            | Key::Backslash
            | Key::BracketLeft
            | Key::BracketRight
            | Key::Comma
            | Key::Digit0
            | Key::Digit1
            | Key::Digit2
            | Key::Digit3
            | Key::Digit4
            | Key::Digit5
            | Key::Digit6
            | Key::Digit7
            | Key::Digit8
            | Key::Digit9
            | Key::Equal
            | Key::IntlBackslash
            | Key::IntlRo
            | Key::IntlYen
            | Key::A
            | Key::B
            | Key::C
            | Key::D
            | Key::E
            | Key::F
            | Key::G
            | Key::H
            | Key::I
            | Key::J
            | Key::K
            | Key::L
            | Key::M
            | Key::N
            | Key::O
            | Key::P
            | Key::Q
            | Key::R
            | Key::S
            | Key::T
            | Key::U
            | Key::V
            | Key::W
            | Key::X
            | Key::Y
            | Key::Z
            | Key::Minus
            | Key::Period
            | Key::Quote
            | Key::Semicolon
            | Key::Slash
    )
}

/// XKB keycode → physical key. An unknown code is `Unidentified`, which sends
/// the caller to the keyval.
fn key_from_scancode(keycode: u32) -> Key {
    match keycode {
        9 => Key::Escape,
        10 => Key::Digit1,
        11 => Key::Digit2,
        12 => Key::Digit3,
        13 => Key::Digit4,
        14 => Key::Digit5,
        15 => Key::Digit6,
        16 => Key::Digit7,
        17 => Key::Digit8,
        18 => Key::Digit9,
        19 => Key::Digit0,
        20 => Key::Minus,
        21 => Key::Equal,
        22 => Key::Backspace,
        23 => Key::Tab,
        24 => Key::Q,
        25 => Key::W,
        26 => Key::E,
        27 => Key::R,
        28 => Key::T,
        29 => Key::Y,
        30 => Key::U,
        31 => Key::I,
        32 => Key::O,
        33 => Key::P,
        34 => Key::BracketLeft,
        35 => Key::BracketRight,
        36 => Key::Enter,
        37 => Key::ControlLeft,
        38 => Key::A,
        39 => Key::S,
        40 => Key::D,
        41 => Key::F,
        42 => Key::G,
        43 => Key::H,
        44 => Key::J,
        45 => Key::K,
        46 => Key::L,
        47 => Key::Semicolon,
        48 => Key::Quote,
        49 => Key::Backquote,
        50 => Key::ShiftLeft,
        51 => Key::Backslash,
        52 => Key::Z,
        53 => Key::X,
        54 => Key::C,
        55 => Key::V,
        56 => Key::B,
        57 => Key::N,
        58 => Key::M,
        59 => Key::Comma,
        60 => Key::Period,
        61 => Key::Slash,
        62 => Key::ShiftRight,
        63 => Key::NumpadMultiply,
        64 => Key::AltLeft,
        65 => Key::Space,
        66 => Key::CapsLock,
        67 => Key::F1,
        68 => Key::F2,
        69 => Key::F3,
        70 => Key::F4,
        71 => Key::F5,
        72 => Key::F6,
        73 => Key::F7,
        74 => Key::F8,
        75 => Key::F9,
        76 => Key::F10,
        77 => Key::NumLock,
        78 => Key::ScrollLock,
        79 => Key::Numpad7,
        80 => Key::Numpad8,
        81 => Key::Numpad9,
        82 => Key::NumpadSubtract,
        83 => Key::Numpad4,
        84 => Key::Numpad5,
        85 => Key::Numpad6,
        86 => Key::NumpadAdd,
        87 => Key::Numpad1,
        88 => Key::Numpad2,
        89 => Key::Numpad3,
        90 => Key::Numpad0,
        91 => Key::NumpadDecimal,
        94 => Key::IntlBackslash,
        95 => Key::F11,
        96 => Key::F12,
        97 => Key::IntlRo,
        100 => Key::Convert,
        101 => Key::KanaMode,
        102 => Key::NonConvert,
        104 => Key::NumpadEnter,
        105 => Key::ControlRight,
        106 => Key::NumpadDivide,
        107 => Key::PrintScreen,
        108 => Key::AltRight,
        110 => Key::Home,
        111 => Key::ArrowUp,
        112 => Key::PageUp,
        113 => Key::ArrowLeft,
        114 => Key::ArrowRight,
        115 => Key::End,
        116 => Key::ArrowDown,
        117 => Key::PageDown,
        118 => Key::Insert,
        119 => Key::Delete,
        121 => Key::AudioVolumeMute,
        122 => Key::AudioVolumeDown,
        123 => Key::AudioVolumeUp,
        124 => Key::Power,
        125 => Key::NumpadEqual,
        127 => Key::Pause,
        129 => Key::NumpadComma,
        132 => Key::IntlYen,
        133 => Key::MetaLeft,
        134 => Key::MetaRight,
        135 => Key::ContextMenu,
        136 => Key::BrowserStop,
        141 => Key::Copy,
        143 => Key::Paste,
        145 => Key::Cut,
        146 => Key::Help,
        148 => Key::LaunchApp2,
        150 => Key::Sleep,
        151 => Key::WakeUp,
        152 => Key::LaunchApp1,
        163 => Key::LaunchMail,
        164 => Key::BrowserFavorites,
        166 => Key::BrowserBack,
        167 => Key::BrowserForward,
        169 => Key::Eject,
        171 => Key::MediaTrackNext,
        172 => Key::MediaPlayPause,
        173 => Key::MediaTrackPrevious,
        174 => Key::MediaStop,
        179 => Key::MediaSelect,
        180 => Key::BrowserHome,
        181 => Key::BrowserRefresh,
        187 => Key::NumpadParenLeft,
        188 => Key::NumpadParenRight,
        191 => Key::F13,
        192 => Key::F14,
        193 => Key::F15,
        194 => Key::F16,
        195 => Key::F17,
        196 => Key::F18,
        197 => Key::F19,
        198 => Key::F20,
        199 => Key::F21,
        200 => Key::F22,
        201 => Key::F23,
        202 => Key::F24,
        225 => Key::BrowserSearch,
        _ => Key::Unidentified,
    }
}

/// The key the keymap says this is. Authoritative for everything outside the
/// writing system, and the fallback for a backend that reports no usable
/// keycode.
fn key_from_keyval(keyval: gdk::Key) -> Key {
    use gdk::Key as K;

    match keyval {
        K::Return | K::ISO_Enter => Key::Enter,
        K::BackSpace => Key::Backspace,
        K::Tab | K::ISO_Left_Tab => Key::Tab,
        K::Escape => Key::Escape,
        K::space => Key::Space,
        K::Delete => Key::Delete,
        K::Insert => Key::Insert,
        K::Home => Key::Home,
        K::End => Key::End,
        K::Page_Up => Key::PageUp,
        K::Page_Down => Key::PageDown,
        K::Up => Key::ArrowUp,
        K::Down => Key::ArrowDown,
        K::Left => Key::ArrowLeft,
        K::Right => Key::ArrowRight,

        K::F1 => Key::F1,
        K::F2 => Key::F2,
        K::F3 => Key::F3,
        K::F4 => Key::F4,
        K::F5 => Key::F5,
        K::F6 => Key::F6,
        K::F7 => Key::F7,
        K::F8 => Key::F8,
        K::F9 => Key::F9,
        K::F10 => Key::F10,
        K::F11 => Key::F11,
        K::F12 => Key::F12,

        K::Shift_L => Key::ShiftLeft,
        K::Shift_R => Key::ShiftRight,
        K::Control_L => Key::ControlLeft,
        K::Control_R => Key::ControlRight,
        K::Alt_L => Key::AltLeft,
        K::Alt_R => Key::AltRight,
        K::Super_L => Key::MetaLeft,
        K::Super_R => Key::MetaRight,
        K::Caps_Lock => Key::CapsLock,
        K::Num_Lock => Key::NumLock,
        K::Scroll_Lock => Key::ScrollLock,
        K::Print => Key::PrintScreen,
        K::Pause => Key::Pause,
        K::Menu => Key::ContextMenu,

        // The keypad, which is where the keymap earns its keep: one scancode
        // types with Num Lock on and navigates with it off, and the keyval is
        // what tells the two apart.
        K::KP_0 => Key::Numpad0,
        K::KP_1 => Key::Numpad1,
        K::KP_2 => Key::Numpad2,
        K::KP_3 => Key::Numpad3,
        K::KP_4 => Key::Numpad4,
        K::KP_5 => Key::Numpad5,
        K::KP_6 => Key::Numpad6,
        K::KP_7 => Key::Numpad7,
        K::KP_8 => Key::Numpad8,
        K::KP_9 => Key::Numpad9,
        K::KP_Enter => Key::NumpadEnter,
        K::KP_Add => Key::NumpadAdd,
        K::KP_Subtract => Key::NumpadSubtract,
        K::KP_Multiply => Key::NumpadMultiply,
        K::KP_Divide => Key::NumpadDivide,
        K::KP_Decimal => Key::NumpadDecimal,
        K::KP_Separator => Key::NumpadSeparator,
        K::KP_Equal => Key::NumpadEqual,
        K::KP_Home => Key::NumpadHome,
        K::KP_End => Key::NumpadEnd,
        K::KP_Up => Key::NumpadUp,
        K::KP_Down => Key::NumpadDown,
        K::KP_Left => Key::NumpadLeft,
        K::KP_Right => Key::NumpadRight,
        K::KP_Page_Up => Key::NumpadPageUp,
        K::KP_Page_Down => Key::NumpadPageDown,
        K::KP_Insert => Key::NumpadInsert,
        K::KP_Delete => Key::NumpadDelete,
        K::KP_Begin => Key::NumpadBegin,

        other => other.to_unicode().map_or(Key::Unidentified, key_from_char),
    }
}

fn key_from_char(ch: char) -> Key {
    match ch.to_ascii_lowercase() {
        'a' => Key::A,
        'b' => Key::B,
        'c' => Key::C,
        'd' => Key::D,
        'e' => Key::E,
        'f' => Key::F,
        'g' => Key::G,
        'h' => Key::H,
        'i' => Key::I,
        'j' => Key::J,
        'k' => Key::K,
        'l' => Key::L,
        'm' => Key::M,
        'n' => Key::N,
        'o' => Key::O,
        'p' => Key::P,
        'q' => Key::Q,
        'r' => Key::R,
        's' => Key::S,
        't' => Key::T,
        'u' => Key::U,
        'v' => Key::V,
        'w' => Key::W,
        'x' => Key::X,
        'y' => Key::Y,
        'z' => Key::Z,
        '0' => Key::Digit0,
        '1' => Key::Digit1,
        '2' => Key::Digit2,
        '3' => Key::Digit3,
        '4' => Key::Digit4,
        '5' => Key::Digit5,
        '6' => Key::Digit6,
        '7' => Key::Digit7,
        '8' => Key::Digit8,
        '9' => Key::Digit9,
        '-' | '_' => Key::Minus,
        '=' | '+' => Key::Equal,
        '[' | '{' => Key::BracketLeft,
        ']' | '}' => Key::BracketRight,
        '\\' | '|' => Key::Backslash,
        ';' | ':' => Key::Semicolon,
        '\'' | '"' => Key::Quote,
        ',' | '<' => Key::Comma,
        '.' | '>' => Key::Period,
        '/' | '?' => Key::Slash,
        '`' | '~' => Key::Backquote,
        ' ' => Key::Space,
        _ => Key::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scancode is what a key is, whatever the layout put on it: on a
    /// French layout the key where a US keyboard has Q types an "a", and the
    /// Kitty protocol still has to call it KeyQ.
    #[test]
    fn a_key_is_named_by_where_it_is_not_by_what_it_types() {
        assert_eq!(key_from_event(gdk::Key::a, 24), Key::Q);
        assert_eq!(key_from_event(gdk::Key::q, 24), Key::Q);
    }

    #[test]
    fn a_keycode_the_table_does_not_know_falls_back_to_the_keyval() {
        assert_eq!(key_from_event(gdk::Key::Escape, 0), Key::Escape);
        assert_eq!(key_from_event(gdk::Key::a, 9999), Key::A);
    }

    #[test]
    fn the_keypad_navigates_when_num_lock_is_off_and_types_when_it_is_on() {
        // One scancode, the 7 on the keypad, under both lock states.
        assert_eq!(key_from_event(gdk::Key::KP_Home, 79), Key::NumpadHome);
        assert_eq!(key_from_event(gdk::Key::KP_7, 79), Key::Numpad7);
    }

    #[test]
    fn the_sides_of_a_modifier_are_told_apart() {
        assert_eq!(key_from_event(gdk::Key::Control_L, 37), Key::ControlLeft);
        assert_eq!(key_from_event(gdk::Key::Control_R, 105), Key::ControlRight);
    }

    /// `caps:swapescape` is XKB handing out Escape for the Caps Lock scancode
    /// and Caps Lock for Escape's. A user who asked for that meant it.
    #[test]
    fn a_key_the_keymap_moved_stays_moved() {
        assert_eq!(key_from_event(gdk::Key::Escape, 66), Key::Escape);
        assert_eq!(key_from_event(gdk::Key::Caps_Lock, 9), Key::CapsLock);
    }

    /// The same option applied to a letter is the bug it guards against: a
    /// Cyrillic layout hands out `Cyrillic_es` for the key where C is, and
    /// `Ctrl+C` still has to be `Ctrl+C`.
    #[test]
    fn a_letter_the_layout_moved_is_still_named_by_position() {
        assert_eq!(key_from_event(gdk::Key::Cyrillic_es, 54), Key::C);
    }

    /// GDK reports the state before the event, so the modifier that moved is
    /// missing from a press and still present on a release.
    #[test]
    fn the_modifier_that_moved_is_in_the_press_and_out_of_the_release() {
        let none = gdk::ModifierType::empty();
        let held = gdk::ModifierType::CONTROL_MASK;

        let press = mods_for_key(none, Key::ControlRight, KeyAction::Press, false);
        assert!(press.contains(Mods::CTRL));
        assert!(press.contains(Mods::CTRL_SIDE));

        let release = mods_for_key(held, Key::ControlLeft, KeyAction::Release, false);
        assert!(!release.contains(Mods::CTRL));
        assert!(!release.contains(Mods::CTRL_SIDE));
    }

    #[test]
    fn num_lock_is_reported_because_the_keypad_encoding_turns_on_it() {
        let none = gdk::ModifierType::empty();
        assert!(mods_for_key(none, Key::Numpad7, KeyAction::Press, true).contains(Mods::NUM_LOCK));
        assert!(!mods_for_key(none, Key::NumpadHome, KeyAction::Press, false).contains(Mods::NUM_LOCK));
    }
}
