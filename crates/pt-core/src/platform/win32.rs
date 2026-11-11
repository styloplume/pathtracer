//! Minimal Win32 window using raw FFI: create a window, pump messages (keyboard,
//! mouse, resize), and blit a 32-bit framebuffer with GDI, optionally with a text
//! overlay composed in an off-screen buffer to avoid flicker.

#![expect(unsafe_code, reason = "raw Win32 FFI; the only module allowed to use unsafe")]
#![expect(non_snake_case, clippy::upper_case_acronyms, reason = "mirror the Win32 SDK names verbatim")]

use super::{Event, Key, Modifiers, MouseButton};
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::{null, null_mut};

type HANDLE = *mut c_void;
type HWND = HANDLE;
type HDC = HANDLE;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type WNDPROC = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: u32,
    style: u32,
    lpfnWndProc: WNDPROC,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HANDLE,
    hIcon: HANDLE,
    hCursor: HANDLE,
    hbrBackground: HANDLE,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HANDLE,
}

#[repr(C)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wParam: WPARAM,
    lParam: LPARAM,
    time: u32,
    pt: POINT,
    lPrivate: u32,
}

#[repr(C)]
#[derive(Default)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: HWND,
        menu: HANDLE,
        instance: HANDLE,
        param: *mut c_void,
    ) -> HWND;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const MSG) -> i32;
    fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn AdjustWindowRect(rect: *mut RECT, style: u32, menu: i32) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn GetDC(hwnd: HWND) -> HDC;
    fn ReleaseDC(hwnd: HWND, dc: HDC) -> i32;
    fn LoadCursorW(instance: HANDLE, name: *const u16) -> HANDLE;
    fn SetWindowTextW(hwnd: HWND, text: *const u16) -> i32;
    fn ValidateRect(hwnd: HWND, rect: *const RECT) -> i32;
    fn SetProcessDPIAware() -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetKeyState(vk: i32) -> i16;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn StretchDIBits(
        dc: HDC,
        xd: i32,
        yd: i32,
        wd: i32,
        hd: i32,
        xs: i32,
        ys: i32,
        ws: i32,
        hs: i32,
        bits: *const c_void,
        bmi: *const BITMAPINFO,
        usage: u32,
        rop: u32,
    ) -> i32;
    fn SetStretchBltMode(dc: HDC, mode: i32) -> i32;
    fn CreateCompatibleDC(dc: HDC) -> HDC;
    fn CreateCompatibleBitmap(dc: HDC, w: i32, h: i32) -> HANDLE;
    fn SelectObject(dc: HDC, obj: HANDLE) -> HANDLE;
    fn DeleteObject(obj: HANDLE) -> i32;
    fn DeleteDC(dc: HDC) -> i32;
    fn BitBlt(dst: HDC, x: i32, y: i32, w: i32, h: i32, src: HDC, xs: i32, ys: i32, rop: u32) -> i32;
    fn SetBkMode(dc: HDC, mode: i32) -> i32;
    fn SetTextColor(dc: HDC, color: u32) -> u32;
    fn TextOutW(dc: HDC, x: i32, y: i32, text: *const u16, len: i32) -> i32;
    fn GetDeviceCaps(dc: HDC, index: i32) -> i32;
    fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike_out: u32,
        charset: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face: *const u16,
    ) -> HANDLE;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
}

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000u32 as i32;
const CS_HREDRAW: u32 = 0x0002;
const CS_VREDRAW: u32 = 0x0001;
const SW_SHOW: i32 = 5;
const PM_REMOVE: u32 = 1;
const IDC_ARROW: usize = 32512;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

const WM_SIZE: u32 = 0x0005;
const WM_KILLFOCUS: u32 = 0x0008;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;

const MK_LBUTTON: usize = 0x01;
const MK_RBUTTON: usize = 0x02;
const MK_MBUTTON: usize = 0x10;

const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;

const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const COLORONCOLOR: i32 = 3;
const TRANSPARENT: i32 = 1;
const LOGPIXELSY: i32 = 90;

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

fn push(e: Event) {
    EVENTS.with(|q| q.borrow_mut().push(e));
}

fn modifiers() -> Modifiers {
    // SAFETY: GetKeyState takes a plain integer and has no preconditions.
    let down = |vk| unsafe { GetKeyState(vk) } < 0;
    Modifiers { shift: down(VK_SHIFT), ctrl: down(VK_CONTROL), alt: down(VK_MENU) }
}

fn translate_key(vk: usize) -> Key {
    match vk {
        0x1B => Key::Escape,
        0x20 => Key::Space,
        0x24 => Key::Home,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x10 => Key::Shift,
        0x11 => Key::Ctrl,
        0xBB | 0x6B => Key::Plus,  // VK_OEM_PLUS, VK_ADD
        0xBD | 0x6D => Key::Minus, // VK_OEM_MINUS, VK_SUBTRACT
        0x70..=0x87 => Key::F((vk - 0x70 + 1) as u8),
        c @ 0x30..=0x5A => Key::Char((c as u8 as char).to_ascii_lowercase()),
        other => Key::Other(other as u32),
    }
}

/// Signed client coordinates packed in an LPARAM.
fn mouse_pos(lparam: LPARAM) -> (i32, i32) {
    (i32::from((lparam & 0xFFFF) as u16 as i16), i32::from(((lparam >> 16) & 0xFFFF) as u16 as i16))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            push(Event::Close);
            0
        }
        WM_SIZE => {
            let (w, h) = ((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32);
            if w > 0 && h > 0 {
                push(Event::Resized(w, h));
            }
            0
        }
        WM_PAINT => {
            // SAFETY: `hwnd` is the live window this message was dispatched to; null means the whole client area.
            unsafe { ValidateRect(hwnd, null()) };
            push(Event::Paint);
            0
        }
        WM_ERASEBKGND => 1, // we always cover the whole client area; avoid flicker
        WM_KILLFOCUS => {
            push(Event::FocusLost);
            0
        }
        WM_KEYDOWN => {
            let repeat = lparam & (1 << 30) != 0;
            push(Event::KeyDown { key: translate_key(wparam), mods: modifiers(), repeat });
            0
        }
        WM_KEYUP => {
            push(Event::KeyUp { key: translate_key(wparam), mods: modifiers() });
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lparam);
            push(Event::MouseMove { x, y, mods: modifiers() });
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let button = match msg {
                WM_LBUTTONDOWN => MouseButton::Left,
                WM_RBUTTONDOWN => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            // Keep receiving mouse moves while dragging outside the window.
            // SAFETY: `hwnd` is the live window this message was dispatched to.
            unsafe { SetCapture(hwnd) };
            let (x, y) = mouse_pos(lparam);
            push(Event::MouseDown { button, x, y, mods: modifiers() });
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let button = match msg {
                WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONUP => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            if wparam & (MK_LBUTTON | MK_RBUTTON | MK_MBUTTON) == 0 {
                // SAFETY: no arguments; releasing capture we may not hold is a harmless no-op.
                unsafe { ReleaseCapture() };
            }
            let (x, y) = mouse_pos(lparam);
            push(Event::MouseUp { button, x, y, mods: modifiers() });
            0
        }
        WM_MOUSEWHEEL => {
            let delta = f32::from(((wparam >> 16) & 0xFFFF) as u16 as i16) / 120.0;
            push(Event::Wheel { delta, mods: modifiers() });
            0
        }
        // SAFETY: forwards the exact arguments Windows passed to this window procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Window {
    hwnd: HWND,
    font: HANDLE,
    line_height: i32,
}

impl Window {
    /// Creates a window whose client area is `w`x`h` pixels, shrunk (keeping the aspect
    /// ratio) if it would not fit on the screen.
    pub fn new(title: &str, w: u32, h: u32) -> Result<Window, String> {
        // SAFETY: every pointer passed below refers to a local (`wc`, `r`, and the NUL-terminated
        // `class`/`title`/`face` buffers) that outlives the call using it; handles come from the
        // preceding calls and are checked where Win32 reports failure through them.
        unsafe {
            SetProcessDPIAware();
            let instance = GetModuleHandleW(null());
            let class = wide("PathTracerWindowClass");
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: wndproc,
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW as *const u16),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
                hIconSm: null_mut(),
            };
            if RegisterClassExW(&raw const wc) == 0 {
                return Err("RegisterClassExW failed".into());
            }

            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN) as f32, GetSystemMetrics(SM_CYSCREEN) as f32);
            let fit = (0.9 * sw / w as f32).min(0.85 * sh / h as f32).min(1.0);
            let (cw, ch) = ((w as f32 * fit) as i32, (h as f32 * fit) as i32);
            let style = WS_OVERLAPPEDWINDOW;
            let mut r = RECT { left: 0, top: 0, right: cw.max(1), bottom: ch.max(1) };
            AdjustWindowRect(&raw mut r, style, 0);

            let title = wide(title);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                r.right - r.left,
                r.bottom - r.top,
                null_mut(),
                null_mut(),
                instance,
                null_mut(),
            );
            if hwnd.is_null() {
                return Err("CreateWindowExW failed".into());
            }

            // Monospace overlay font scaled for the display DPI.
            let dc = GetDC(hwnd);
            let dpi = GetDeviceCaps(dc, LOGPIXELSY).max(96);
            ReleaseDC(hwnd, dc);
            let px = 15 * dpi / 96;
            let face = wide("Consolas");
            // weight 400, DEFAULT_CHARSET, ANTIALIASED_QUALITY, FIXED_PITCH | FF_MODERN
            let font = CreateFontW(-px, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 4, 0x31, face.as_ptr());

            ShowWindow(hwnd, SW_SHOW);
            Ok(Window { hwnd, font, line_height: px + px / 4 })
        }
    }

    // `&mut self` (unused) ties message pumping to the window's owner thread.
    pub fn poll_events(&mut self) -> Vec<Event> {
        // SAFETY: MSG is plain old data, so all-zero is valid; `msg` outlives each call that borrows it.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&raw mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
        EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        // SAFETY: `self.hwnd` stays valid until Drop; `r` is a live, writable RECT.
        unsafe { GetClientRect(self.hwnd, &raw mut r) };
        ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
    }

    /// Draws a `w`x`h` buffer of 0x00RRGGBB pixels stretched over the client area,
    /// with `overlay` lines of text in the top-left corner.
    pub fn present(&mut self, pixels: &[u32], w: usize, h: usize, overlay: &[String]) {
        if w == 0 || h == 0 || pixels.len() < w * h {
            return;
        }
        let (cw, ch) = self.client_size();
        let (cw, ch) = (cw as i32, ch as i32);
        if cw == 0 || ch == 0 {
            return;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // negative: top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0],
        };
        // SAFETY: `pixels` holds at least `w * h` pixels (checked above) as described by `bmi`; the
        // DC and the off-screen bitmap/DC are released before returning and the old GDI objects
        // are reselected first; text buffers outlive the TextOutW calls.
        unsafe {
            let dc = GetDC(self.hwnd);
            let blit = |target: HDC| {
                SetStretchBltMode(target, COLORONCOLOR);
                StretchDIBits(
                    target,
                    0,
                    0,
                    cw,
                    ch,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    pixels.as_ptr().cast::<c_void>(),
                    &raw const bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            };
            if overlay.is_empty() {
                blit(dc);
            } else {
                // Compose image + text off-screen, then copy in one go (no flicker).
                let mem = CreateCompatibleDC(dc);
                let bmp = CreateCompatibleBitmap(dc, cw, ch);
                let old_bmp = SelectObject(mem, bmp);
                blit(mem);
                let old_font = SelectObject(mem, self.font);
                SetBkMode(mem, TRANSPARENT);
                let pad = self.line_height / 2;
                for (i, line) in overlay.iter().enumerate() {
                    let text: Vec<u16> = line.encode_utf16().collect();
                    let y = pad + i as i32 * self.line_height;
                    // Drop shadow keeps the text readable on any background.
                    SetTextColor(mem, 0x00_00_00);
                    TextOutW(mem, pad + 1, y + 1, text.as_ptr(), text.len() as i32);
                    SetTextColor(mem, 0xFF_FF_FF);
                    TextOutW(mem, pad, y, text.as_ptr(), text.len() as i32);
                }
                SelectObject(mem, old_font);
                BitBlt(dc, 0, 0, cw, ch, mem, 0, 0, SRCCOPY);
                SelectObject(mem, old_bmp);
                DeleteObject(bmp);
                DeleteDC(mem);
            }
            ReleaseDC(self.hwnd, dc);
        }
    }

    pub fn set_title(&self, title: &str) {
        let t = wide(title);
        // SAFETY: `self.hwnd` stays valid until Drop; `t` is NUL-terminated and outlives the call.
        unsafe { SetWindowTextW(self.hwnd, t.as_ptr()) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: both handles were created in `new`, are owned solely by this Window, and are
        // destroyed exactly once here.
        unsafe {
            DestroyWindow(self.hwnd);
            if !self.font.is_null() {
                DeleteObject(self.font);
            }
        }
    }
}
