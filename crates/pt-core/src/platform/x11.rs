//! X11 window through Xlib, loaded at run time with `dlopen` (raw FFI, no crates and no
//! link-time dependency: headless rendering works on machines without X11 libraries).
//!
//! The framebuffer is scaled to the window size on the CPU (nearest neighbour, as the
//! progressive preview's blocks want) and sent with `XPutImage`. With an overlay, image
//! and text are composed in a server-side pixmap first, then copied in one go (no flicker).

#![expect(unsafe_code, reason = "raw Xlib / libdl FFI; the platform layer is the only place allowed to use unsafe")]
#![expect(non_snake_case, reason = "mirror the Xlib names verbatim")]

use super::{Event, Key, Modifiers, MouseButton};
use std::collections::HashSet;
use std::ffi::{CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr::{null, null_mut};

type Display = c_void;
type XWindow = c_ulong;
type Atom = c_ulong;
type Drawable = c_ulong;
type Gc = *mut c_void;
type KeySym = c_ulong;

/// `XEvent` is a union padded to 24 longs.
#[repr(C)]
struct XEvent {
    pad: [c_long; 24],
}

impl XEvent {
    fn kind(&self) -> c_int {
        self.pad[0] as c_int
    }
}

/// Shared layout of `XKeyEvent`, `XButtonEvent` and `XMotionEvent` (`detail` is the
/// keycode, the button, or the motion hint flag).
#[repr(C)]
struct XInputEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    root: XWindow,
    subwindow: XWindow,
    time: c_ulong,
    x: c_int,
    y: c_int,
    x_root: c_int,
    y_root: c_int,
    state: c_uint,
    detail: c_uint,
    same_screen: c_int,
}

#[repr(C)]
struct XConfigureEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    event: XWindow,
    window: XWindow,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
}

#[repr(C)]
struct XExposeEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
    count: c_int,
}

#[repr(C)]
struct XClientMessageEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    message_type: Atom,
    format: c_int,
    data: [c_long; 5],
}

/// The leading fields of `Visual`.
#[repr(C)]
struct Visual {
    ext_data: *mut c_void,
    visualid: c_ulong,
    class: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
}

/// The leading fields of `XImage`.
#[repr(C)]
struct XImage {
    width: c_int,
    height: c_int,
    xoffset: c_int,
    format: c_int,
    data: *mut c_char,
    byte_order: c_int,
}

#[repr(C)]
struct XCharStruct {
    lbearing: i16,
    rbearing: i16,
    width: i16,
    ascent: i16,
    descent: i16,
    attributes: u16,
}

/// The leading fields of `XFontStruct`.
#[repr(C)]
struct XFontStruct {
    ext_data: *mut c_void,
    fid: c_ulong,
    direction: c_uint,
    min_char_or_byte2: c_uint,
    max_char_or_byte2: c_uint,
    min_byte1: c_uint,
    max_byte1: c_uint,
    all_chars_exist: c_int,
    default_char: c_uint,
    n_properties: c_int,
    properties: *mut c_void,
    min_bounds: XCharStruct,
    max_bounds: XCharStruct,
    per_char: *mut XCharStruct,
    ascent: c_int,
    descent: c_int,
}

const RTLD_NOW: c_int = 2;

unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

/// Declares the Xlib functions we use and a table holding them, resolved with `dlsym`.
macro_rules! xlib {
    ($($name:ident: fn($($arg:ty),*) -> $ret:ty;)*) => {
        struct Xlib {
            $($name: unsafe extern "C" fn($($arg),*) -> $ret,)*
        }

        impl Xlib {
            fn load() -> Result<Xlib, String> {
                // SAFETY: dlopen / dlsym get NUL-terminated names; every symbol is transmuted to
                // the signature Xlib's headers declare for it.
                unsafe {
                    let mut lib = dlopen(c"libX11.so.6".as_ptr(), RTLD_NOW);
                    if lib.is_null() {
                        lib = dlopen(c"libX11.so".as_ptr(), RTLD_NOW);
                    }
                    if lib.is_null() {
                        return Err("cannot load libX11 (is X11 installed?); use --headless".into());
                    }
                    Ok(Xlib {
                        $($name: {
                            let p = dlsym(lib, concat!(stringify!($name), "\0").as_ptr().cast());
                            if p.is_null() {
                                return Err(concat!("libX11 lacks ", stringify!($name)).into());
                            }
                            std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($arg),*) -> $ret>(p)
                        },)*
                    })
                }
            }
        }
    };
}

xlib! {
    XOpenDisplay: fn(*const c_char) -> *mut Display;
    XCloseDisplay: fn(*mut Display) -> c_int;
    XDefaultScreen: fn(*mut Display) -> c_int;
    XRootWindow: fn(*mut Display, c_int) -> XWindow;
    XDefaultVisual: fn(*mut Display, c_int) -> *mut Visual;
    XDefaultDepth: fn(*mut Display, c_int) -> c_int;
    XDisplayWidth: fn(*mut Display, c_int) -> c_int;
    XDisplayHeight: fn(*mut Display, c_int) -> c_int;
    XCreateSimpleWindow: fn(*mut Display, XWindow, c_int, c_int, c_uint, c_uint, c_uint, c_ulong, c_ulong) -> XWindow;
    XDestroyWindow: fn(*mut Display, XWindow) -> c_int;
    XSelectInput: fn(*mut Display, XWindow, c_long) -> c_int;
    XInternAtom: fn(*mut Display, *const c_char, c_int) -> Atom;
    XSetWMProtocols: fn(*mut Display, XWindow, *mut Atom, c_int) -> c_int;
    XStoreName: fn(*mut Display, XWindow, *const c_char) -> c_int;
    XChangeProperty: fn(*mut Display, XWindow, Atom, Atom, c_int, c_int, *const u8, c_int) -> c_int;
    XMapWindow: fn(*mut Display, XWindow) -> c_int;
    XFlush: fn(*mut Display) -> c_int;
    XPending: fn(*mut Display) -> c_int;
    XNextEvent: fn(*mut Display, *mut XEvent) -> c_int;
    XLookupKeysym: fn(*mut XInputEvent, c_int) -> KeySym;
    XkbSetDetectableAutoRepeat: fn(*mut Display, c_int, *mut c_int) -> c_int;
    XCreateGC: fn(*mut Display, Drawable, c_ulong, *mut c_void) -> Gc;
    XFreeGC: fn(*mut Display, Gc) -> c_int;
    XSetForeground: fn(*mut Display, Gc, c_ulong) -> c_int;
    XSetFont: fn(*mut Display, Gc, c_ulong) -> c_int;
    XLoadQueryFont: fn(*mut Display, *const c_char) -> *mut XFontStruct;
    XFreeFont: fn(*mut Display, *mut XFontStruct) -> c_int;
    XDrawString: fn(*mut Display, Drawable, Gc, c_int, c_int, *const c_char, c_int) -> c_int;
    XCreateImage: fn(*mut Display, *mut Visual, c_uint, c_int, c_int, *mut c_char, c_uint, c_uint, c_int, c_int) -> *mut XImage;
    XPutImage: fn(*mut Display, Drawable, Gc, *mut XImage, c_int, c_int, c_int, c_int, c_uint, c_uint) -> c_int;
    XFree: fn(*mut c_void) -> c_int;
    XCreatePixmap: fn(*mut Display, Drawable, c_uint, c_uint, c_uint) -> c_ulong;
    XFreePixmap: fn(*mut Display, c_ulong) -> c_int;
    XCopyArea: fn(*mut Display, Drawable, Drawable, Gc, c_int, c_int, c_uint, c_uint, c_int, c_int) -> c_int;
}

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const FOCUS_OUT: c_int = 10;
const EXPOSE: c_int = 12;
const CONFIGURE_NOTIFY: c_int = 22;
const CLIENT_MESSAGE: c_int = 33;

const KEY_PRESS_MASK: c_long = 1 << 0;
const KEY_RELEASE_MASK: c_long = 1 << 1;
const BUTTON_PRESS_MASK: c_long = 1 << 2;
const BUTTON_RELEASE_MASK: c_long = 1 << 3;
const POINTER_MOTION_MASK: c_long = 1 << 6;
const EXPOSURE_MASK: c_long = 1 << 15;
const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
const FOCUS_CHANGE_MASK: c_long = 1 << 21;

const SHIFT_MASK: c_uint = 1;
const CONTROL_MASK: c_uint = 4;
const MOD1_MASK: c_uint = 8;

const Z_PIXMAP: c_int = 2;
const LSB_FIRST: c_int = 0;
const PROP_MODE_REPLACE: c_int = 0;

/// Stretches a `w`x`h` image over `cw`x`ch` pixels into `out` (resized as needed),
/// nearest neighbour: sharp edges for the preview's blocks and for upscaled renders.
fn scale_nearest(src: &[u32], w: usize, h: usize, out: &mut Vec<u32>, cw: usize, ch: usize) {
    out.resize(cw * ch, 0);
    let xs: Vec<usize> = (0..cw).map(|x| (x * w / cw).min(w - 1)).collect();
    for (y, row) in out.chunks_exact_mut(cw).enumerate() {
        let sy = (y * h / ch).min(h - 1);
        let src_row = &src[sy * w..(sy + 1) * w];
        for (d, &sx) in row.iter_mut().zip(&xs) {
            *d = src_row[sx];
        }
    }
}

fn modifiers(state: c_uint) -> Modifiers {
    Modifiers { shift: state & SHIFT_MASK != 0, ctrl: state & CONTROL_MASK != 0, alt: state & MOD1_MASK != 0 }
}

fn translate_key(sym: KeySym) -> Key {
    match sym {
        0xFF1B => Key::Escape,
        0x20 => Key::Space,
        0xFF50 | 0xFF95 => Key::Home,
        0xFF51 | 0xFF96 => Key::Left,
        0xFF52 | 0xFF97 => Key::Up,
        0xFF53 | 0xFF98 => Key::Right,
        0xFF54 | 0xFF99 => Key::Down,
        0xFFE1 | 0xFFE2 => Key::Shift,
        0xFFE3 | 0xFFE4 => Key::Ctrl,
        // '=' is the unshifted '+' key (VK_OEM_PLUS on Windows); keypad +/-.
        0x2B | 0x3D | 0xFFAB => Key::Plus,
        0x2D | 0xFFAD => Key::Minus,
        0xFFBE..=0xFFD5 => Key::F((sym - 0xFFBE + 1) as u8),
        c @ (0x30..=0x39 | 0x61..=0x7A) => Key::Char(c as u8 as char),
        c @ 0x41..=0x5A => Key::Char((c as u8 as char).to_ascii_lowercase()),
        other => Key::Other(other as u32),
    }
}

pub struct Window {
    x: Xlib,
    display: *mut Display,
    win: XWindow,
    visual: *mut Visual,
    depth: c_uint,
    gc: Gc,
    font: *mut XFontStruct,
    wm_delete: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    size: (u32, u32),
    /// Keys currently held (X reports auto-repeat as repeated presses once it is detectable).
    down: HashSet<Key>,
    /// Client-size pixels when the framebuffer has to be scaled.
    scaled: Vec<u32>,
    /// Off-screen pixmap for composing the overlay, and its size.
    pixmap: Option<(c_ulong, u32, u32)>,
}

impl Window {
    pub fn new(title: &str, w: u32, h: u32) -> Result<Window, String> {
        let x = Xlib::load()?;
        // SAFETY: calls follow Xlib's documented usage on the display / window created here;
        // every pointer passed refers to a live local or NUL-terminated buffer.
        unsafe {
            let display = (x.XOpenDisplay)(null());
            if display.is_null() {
                return Err("cannot open the X display (is DISPLAY set?); use --headless".into());
            }
            let screen = (x.XDefaultScreen)(display);
            let visual = (x.XDefaultVisual)(display, screen);
            let depth = (x.XDefaultDepth)(display, screen);
            let v = &*visual;
            if depth < 24 || v.red_mask != 0xFF_0000 || v.green_mask != 0xFF00 || v.blue_mask != 0xFF {
                (x.XCloseDisplay)(display);
                return Err(format!("unsupported X visual (depth {depth}); a 24-bit TrueColor display is needed"));
            }

            let (sw, sh) = ((x.XDisplayWidth)(display, screen) as f32, (x.XDisplayHeight)(display, screen) as f32);
            let fit = (0.9 * sw / w as f32).min(0.85 * sh / h as f32).min(1.0);
            let (cw, ch) = (((w as f32 * fit) as u32).max(1), ((h as f32 * fit) as u32).max(1));
            let root = (x.XRootWindow)(display, screen);
            let window = (x.XCreateSimpleWindow)(display, root, 0, 0, cw, ch, 0, 0, 0);
            (x.XSelectInput)(
                display,
                window,
                KEY_PRESS_MASK
                    | KEY_RELEASE_MASK
                    | BUTTON_PRESS_MASK
                    | BUTTON_RELEASE_MASK
                    | POINTER_MOTION_MASK
                    | EXPOSURE_MASK
                    | STRUCTURE_NOTIFY_MASK
                    | FOCUS_CHANGE_MASK,
            );
            let mut wm_delete = (x.XInternAtom)(display, c"WM_DELETE_WINDOW".as_ptr(), 0);
            (x.XSetWMProtocols)(display, window, &raw mut wm_delete, 1);
            (x.XkbSetDetectableAutoRepeat)(display, 1, null_mut());

            let gc = (x.XCreateGC)(display, window, 0, null_mut());
            let mut font = null_mut();
            for name in [c"-misc-fixed-medium-r-normal--18-*-*-*-*-*-iso10646-1", c"9x15", c"fixed"] {
                font = (x.XLoadQueryFont)(display, name.as_ptr());
                if !font.is_null() {
                    (x.XSetFont)(display, gc, (*font).fid);
                    break;
                }
            }

            let net_wm_name = (x.XInternAtom)(display, c"_NET_WM_NAME".as_ptr(), 0);
            let utf8_string = (x.XInternAtom)(display, c"UTF8_STRING".as_ptr(), 0);
            let win = Window {
                x,
                display,
                win: window,
                visual,
                depth: depth as c_uint,
                gc,
                font,
                wm_delete,
                net_wm_name,
                utf8_string,
                size: (cw, ch),
                down: HashSet::new(),
                scaled: Vec::new(),
                pixmap: None,
            };
            win.set_title(title);
            (win.x.XMapWindow)(display, window);
            (win.x.XFlush)(display);
            Ok(win)
        }
    }

    pub fn poll_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        // SAFETY: `ev` is a full-size XEvent filled by XNextEvent; it is reinterpreted only as
        // the event struct its `type` field announces, all of which fit inside it.
        unsafe {
            while (self.x.XPending)(self.display) > 0 {
                let mut ev = XEvent { pad: [0; 24] };
                (self.x.XNextEvent)(self.display, &raw mut ev);
                let p = (&raw mut ev).cast::<XInputEvent>();
                match ev.kind() {
                    KEY_PRESS | KEY_RELEASE => {
                        let key = translate_key((self.x.XLookupKeysym)(p, 0));
                        let mods = modifiers((*p).state);
                        if ev.kind() == KEY_PRESS {
                            let repeat = !self.down.insert(key);
                            out.push(Event::KeyDown { key, mods, repeat });
                        } else {
                            self.down.remove(&key);
                            out.push(Event::KeyUp { key, mods });
                        }
                    }
                    BUTTON_PRESS | BUTTON_RELEASE => {
                        let e = &*p;
                        let mods = modifiers(e.state);
                        let button = match e.detail {
                            1 => MouseButton::Left,
                            2 => MouseButton::Middle,
                            3 => MouseButton::Right,
                            4 | 5 => {
                                // Wheel notches arrive as button 4 (up) / 5 (down) presses.
                                if ev.kind() == BUTTON_PRESS {
                                    let delta = if e.detail == 4 { 1.0 } else { -1.0 };
                                    out.push(Event::Wheel { delta, mods });
                                }
                                continue;
                            }
                            _ => continue,
                        };
                        let (x, y) = (e.x, e.y);
                        out.push(if ev.kind() == BUTTON_PRESS {
                            Event::MouseDown { button, x, y, mods }
                        } else {
                            Event::MouseUp { button, x, y, mods }
                        });
                    }
                    MOTION_NOTIFY => {
                        let e = &*p;
                        out.push(Event::MouseMove { x: e.x, y: e.y, mods: modifiers(e.state) });
                    }
                    FOCUS_OUT => {
                        self.down.clear();
                        out.push(Event::FocusLost);
                    }
                    EXPOSE => {
                        if (*(&raw const ev).cast::<XExposeEvent>()).count == 0 {
                            out.push(Event::Paint);
                        }
                    }
                    CONFIGURE_NOTIFY => {
                        let e = &*(&raw const ev).cast::<XConfigureEvent>();
                        let size = (e.width.max(0) as u32, e.height.max(0) as u32);
                        if size != self.size && size.0 > 0 && size.1 > 0 {
                            self.size = size;
                            out.push(Event::Resized(size.0, size.1));
                        }
                    }
                    CLIENT_MESSAGE => {
                        let e = &*(&raw const ev).cast::<XClientMessageEvent>();
                        if e.data[0] as Atom == self.wm_delete {
                            out.push(Event::Close);
                        }
                    }
                    _ => {}
                }
            }
        }
        out
    }

    pub fn client_size(&self) -> (u32, u32) {
        self.size
    }

    /// Draws a `w`x`h` buffer of 0x00RRGGBB pixels stretched over the window, with
    /// `overlay` lines of text in the top-left corner.
    pub fn present(&mut self, pixels: &[u32], w: usize, h: usize, overlay: &[String]) {
        let (cw, ch) = (self.size.0 as usize, self.size.1 as usize);
        if w == 0 || h == 0 || pixels.len() < w * h || cw == 0 || ch == 0 {
            return;
        }
        let src = if (w, h) == (cw, ch) {
            &pixels[..w * h]
        } else {
            scale_nearest(pixels, w, h, &mut self.scaled, cw, ch);
            &self.scaled[..]
        };
        let (cw32, ch32) = (cw as c_uint, ch as c_uint);
        // SAFETY: the XImage wraps `src` (cw x ch 32-bit pixels, alive for the whole block) and is
        // freed with XFree, which releases only the struct, never our buffer. Drawables and the
        // GC belong to this window; text buffers outlive the XDrawString calls.
        unsafe {
            let image = (self.x.XCreateImage)(
                self.display,
                self.visual,
                self.depth,
                Z_PIXMAP,
                0,
                src.as_ptr().cast_mut().cast(),
                cw32,
                ch32,
                32,
                (cw * 4) as c_int,
            );
            if image.is_null() {
                return;
            }
            // Our pixels are native-endian u32s; Xlib converts if the server differs.
            (*image).byte_order = if cfg!(target_endian = "little") { LSB_FIRST } else { 1 };
            if overlay.is_empty() || self.font.is_null() {
                (self.x.XPutImage)(self.display, self.win, self.gc, image, 0, 0, 0, 0, cw32, ch32);
            } else {
                let pixmap = match self.pixmap {
                    Some((p, pw, ph)) if (pw, ph) == self.size => p,
                    old => {
                        if let Some((p, ..)) = old {
                            (self.x.XFreePixmap)(self.display, p);
                        }
                        let p = (self.x.XCreatePixmap)(self.display, self.win, cw32, ch32, self.depth);
                        self.pixmap = Some((p, self.size.0, self.size.1));
                        p
                    }
                };
                (self.x.XPutImage)(self.display, pixmap, self.gc, image, 0, 0, 0, 0, cw32, ch32);
                let f = &*self.font;
                let line = f.ascent + f.descent + (f.ascent + f.descent) / 4;
                let pad = line / 2;
                for (i, text) in overlay.iter().enumerate() {
                    // Core fonts take Latin-1; replace anything else.
                    let bytes: Vec<u8> = text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect();
                    let y = pad + f.ascent + i as c_int * line;
                    for (dx, color) in [(1, 0x00_0000), (0, 0xFF_FFFF)] {
                        (self.x.XSetForeground)(self.display, self.gc, color);
                        (self.x.XDrawString)(
                            self.display,
                            pixmap,
                            self.gc,
                            pad + dx,
                            y + dx,
                            bytes.as_ptr().cast(),
                            bytes.len() as c_int,
                        );
                    }
                }
                (self.x.XCopyArea)(self.display, pixmap, self.win, self.gc, 0, 0, cw32, ch32, 0, 0);
            }
            (self.x.XFree)(image.cast());
            (self.x.XFlush)(self.display);
        }
    }

    pub fn set_title(&self, title: &str) {
        let ascii: String = title.chars().map(|c| if c.is_ascii() { c } else { '-' }).collect();
        let ascii = CString::new(ascii).unwrap_or_default();
        // SAFETY: the window is live; `title` / `ascii` outlive the calls that read them.
        unsafe {
            (self.x.XStoreName)(self.display, self.win, ascii.as_ptr());
            (self.x.XChangeProperty)(
                self.display,
                self.win,
                self.net_wm_name,
                self.utf8_string,
                8,
                PROP_MODE_REPLACE,
                title.as_ptr(),
                title.len() as c_int,
            );
            (self.x.XFlush)(self.display);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: every resource was created in `new` / `present`, is owned by this Window and is
        // released exactly once, before the display connection is closed.
        unsafe {
            if let Some((p, ..)) = self.pixmap {
                (self.x.XFreePixmap)(self.display, p);
            }
            if !self.font.is_null() {
                (self.x.XFreeFont)(self.display, self.font);
            }
            (self.x.XFreeGC)(self.display, self.gc);
            (self.x.XDestroyWindow)(self.display, self.win);
            (self.x.XCloseDisplay)(self.display);
        }
    }
}
