//! Minimal Win32 window using raw FFI: create a window, pump messages, and blit a
//! 32-bit framebuffer with GDI's StretchDIBits.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use super::{Event, Key};
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
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_KEYDOWN: u32 = 0x0100;
const VK_ESCAPE: usize = 0x1B;
const DIB_RGB_COLORS: u32 = 0;
const SRCCOPY: u32 = 0x00CC_0020;
const COLORONCOLOR: i32 = 3;

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

fn push(e: Event) {
    EVENTS.with(|q| q.borrow_mut().push(e));
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
            unsafe { ValidateRect(hwnd, null()) };
            push(Event::Paint);
            0
        }
        WM_ERASEBKGND => 1, // we always cover the whole client area; avoid flicker
        WM_KEYDOWN => {
            let key = match wparam {
                VK_ESCAPE => Key::Escape,
                c @ 0x30..=0x5A => Key::Char((c as u8 as char).to_ascii_lowercase()),
                other => Key::Other(other as u32),
            };
            push(Event::KeyDown(key));
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Window {
    hwnd: HWND,
}

impl Window {
    /// Creates a window whose client area is `w`x`h` pixels, shrunk (keeping the aspect
    /// ratio) if it would not fit on the screen.
    pub fn new(title: &str, w: u32, h: u32) -> Result<Window, String> {
        unsafe {
            SetProcessDPIAware();
            let instance = GetModuleHandleW(null());
            let class = wide("PathTracerWindowClass");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
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
            if RegisterClassExW(&wc) == 0 {
                return Err("RegisterClassExW failed".into());
            }

            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN) as f32, GetSystemMetrics(SM_CYSCREEN) as f32);
            let fit = (0.9 * sw / w as f32).min(0.85 * sh / h as f32).min(1.0);
            let (cw, ch) = ((w as f32 * fit) as i32, (h as f32 * fit) as i32);
            let style = WS_OVERLAPPEDWINDOW;
            let mut r = RECT { left: 0, top: 0, right: cw.max(1), bottom: ch.max(1) };
            AdjustWindowRect(&mut r, style, 0);

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
            ShowWindow(hwnd, SW_SHOW);
            Ok(Window { hwnd })
        }
    }

    pub fn poll_events(&mut self) -> Vec<Event> {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut r) };
        ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
    }

    /// Draws a `w`x`h` buffer of 0x00RRGGBB pixels stretched over the client area.
    pub fn present(&self, pixels: &[u32], w: usize, h: usize) {
        if w == 0 || h == 0 || pixels.len() < w * h {
            return;
        }
        let (cw, ch) = self.client_size();
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
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
        unsafe {
            let dc = GetDC(self.hwnd);
            SetStretchBltMode(dc, COLORONCOLOR);
            StretchDIBits(
                dc,
                0,
                0,
                cw as i32,
                ch as i32,
                0,
                0,
                w as i32,
                h as i32,
                pixels.as_ptr() as *const c_void,
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            ReleaseDC(self.hwnd, dc);
        }
    }

    pub fn set_title(&self, title: &str) {
        let t = wide(title);
        unsafe { SetWindowTextW(self.hwnd, t.as_ptr()) };
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.hwnd) };
    }
}
