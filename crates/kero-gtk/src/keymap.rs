//! GDK keyval → libghostty key mapping.
//!
//! libghostty names keys by physical position (W3C `KeyboardEvent.code`), while
//! GDK hands us a logical keyval. Deriving the physical key would mean an
//! evdev-scancode table; for now the logical mapping is enough, because the
//! encoder only needs `key` for named keys and for the Kitty protocol — plain
//! text rides along in the event's UTF-8 field.
//!
//! Etap 1 replaces this with a real scancode table so that non-US layouts
//! report the physical key the Kitty protocol expects.

use gtk::gdk;
use kero_vt::{Key, Mods};

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

pub fn key_from_keyval(keyval: gdk::Key) -> Key {
    use gdk::Key as K;

    match keyval {
        K::Return | K::KP_Enter | K::ISO_Enter => Key::Enter,
        K::BackSpace => Key::Backspace,
        K::Tab | K::ISO_Left_Tab => Key::Tab,
        K::Escape => Key::Escape,
        K::space => Key::Space,
        K::Delete | K::KP_Delete => Key::Delete,
        K::Insert | K::KP_Insert => Key::Insert,
        K::Home | K::KP_Home => Key::Home,
        K::End | K::KP_End => Key::End,
        K::Page_Up | K::KP_Page_Up => Key::PageUp,
        K::Page_Down | K::KP_Page_Down => Key::PageDown,
        K::Up | K::KP_Up => Key::ArrowUp,
        K::Down | K::KP_Down => Key::ArrowDown,
        K::Left | K::KP_Left => Key::ArrowLeft,
        K::Right | K::KP_Right => Key::ArrowRight,

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
