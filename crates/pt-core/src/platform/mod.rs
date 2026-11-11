//! Native windowing through raw FFI, no crates: Win32 (GDI), X11 (Xlib, loaded at run
//! time) and macOS (Cocoa through the Objective-C runtime). Each backend provides a
//! `Window` with the same methods: `new`, `poll_events`, `client_size`, `present`,
//! `set_title`, and for UIs `scale_factor`, `set_cursor`, `clipboard_text`,
//! `set_clipboard_text` and `double_click_time`.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    /// Control; on macOS also Command, the key shortcuts use there.
    pub ctrl: bool,
    pub alt: bool,
}

/// Mouse pointer shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cursor {
    #[default]
    Arrow,
    /// The I-beam over editable text.
    Text,
    /// Resizing left and right (a vertical splitter).
    ResizeH,
    /// Resizing up and down (a horizontal splitter).
    ResizeV,
    /// A pointing hand over links.
    Hand,
    /// Moving in every direction (dragging a number).
    Move,
}

/// How quickly a second click must follow for a double click, where the platform does not say.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Close,
    /// New client-area size in physical pixels.
    Resized(u32, u32),
    /// The window content was invalidated and must be redrawn.
    Paint,
    /// The window lost keyboard focus; held keys and buttons should be considered released.
    FocusLost,
    KeyDown {
        key: Key,
        mods: Modifiers,
        repeat: bool,
    },
    KeyUp {
        key: Key,
        mods: Modifiers,
    },
    /// Client-area coordinates in physical pixels (may be outside the window while captured).
    MouseDown {
        button: MouseButton,
        x: i32,
        y: i32,
        mods: Modifiers,
    },
    MouseUp {
        button: MouseButton,
        x: i32,
        y: i32,
        mods: Modifiers,
    },
    MouseMove {
        x: i32,
        y: i32,
        mods: Modifiers,
    },
    /// Wheel rotation in notches; positive = away from the user.
    Wheel {
        delta: f32,
        mods: Modifiers,
    },
    /// A character typed, after keyboard layout, dead keys and input methods (never a
    /// control character). Comes in addition to the `KeyDown` of the key.
    Text(char),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Escape,
    Space,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Plus,
    Minus,
    Up,
    Down,
    Left,
    Right,
    Shift,
    Ctrl,
    /// Function key F1..F24.
    F(u8),
    /// Letters (lowercase) and digits.
    Char(char),
    Other(u32),
}

#[cfg(windows)]
mod win32;
#[cfg(windows)]
pub use win32::Window;

#[cfg(all(unix, not(target_os = "macos")))]
mod x11;
#[cfg(all(unix, not(target_os = "macos")))]
pub use x11::Window;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::Window;

#[cfg(not(any(windows, unix)))]
pub struct Window;

#[cfg(not(any(windows, unix)))]
impl Window {
    pub fn new(_title: &str, _w: u32, _h: u32) -> Result<Window, String> {
        Err("windowed mode is not implemented on this platform; use --headless".into())
    }
    pub fn poll_events(&mut self) -> Vec<Event> {
        Vec::new()
    }
    pub fn client_size(&self) -> (u32, u32) {
        (0, 0)
    }
    pub fn present(&mut self, _pixels: &[u32], _w: usize, _h: usize, _overlay: &[String]) {}
    pub fn set_title(&self, _title: &str) {}
    pub fn scale_factor(&self) -> f32 {
        1.0
    }
    pub fn set_cursor(&mut self, _cursor: Cursor) {}
    pub fn clipboard_text(&mut self) -> Option<String> {
        None
    }
    pub fn set_clipboard_text(&mut self, _text: &str) {}
    pub fn double_click_time(&self) -> Duration {
        DOUBLE_CLICK
    }
}
