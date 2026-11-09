//! Native windowing. Only a Win32 backend exists so far (raw FFI, no crates).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    Close,
    /// New client-area size in physical pixels.
    Resized(u32, u32),
    /// The window content was invalidated and must be redrawn.
    Paint,
    KeyDown(Key),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Escape,
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
    pub fn present(&self, _pixels: &[u32], _w: usize, _h: usize) {}
    pub fn set_title(&self, _title: &str) {}
}
