//! Native windowing. Only a Win32 backend exists so far (raw FFI, no crates).

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Escape,
    Space,
    Home,
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

#[cfg(not(windows))]
pub struct Window;

#[cfg(not(windows))]
impl Window {
    pub fn new(_title: &str, _w: u32, _h: u32) -> Result<Window, String> {
        Err("windowed mode is only implemented on Windows so far; use --headless".into())
    }
    pub fn poll_events(&mut self) -> Vec<Event> {
        Vec::new()
    }
    pub fn client_size(&self) -> (u32, u32) {
        (0, 0)
    }
    pub fn present(&mut self, _pixels: &[u32], _w: usize, _h: usize, _overlay: &[String]) {}
    pub fn set_title(&self, _title: &str) {}
}
