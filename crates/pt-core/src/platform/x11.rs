//! X11 window through Xlib, loaded at run time with `dlopen` (raw FFI, no crates and no
//! link-time dependency: headless rendering works on machines without X11 libraries).
//!
//! The framebuffer is scaled to the window size on the CPU (nearest neighbour, as the
//! progressive preview's blocks want) and sent with `XPutImage`. With an overlay, image
//! and text are composed in a server-side pixmap first, then copied in one go (no flicker).

#![expect(unsafe_code, reason = "raw Xlib / libdl FFI; the platform layer is the only place allowed to use unsafe")]
#![expect(non_snake_case, reason = "mirror the Xlib names verbatim")]

use super::{Cursor, Event, Key, Modifiers, MouseButton};
use std::collections::HashSet;
use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

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
struct XSelectionRequestEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    owner: XWindow,
    requestor: XWindow,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: c_ulong,
}

#[repr(C)]
struct XSelectionClearEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    window: XWindow,
    selection: Atom,
    time: c_ulong,
}

#[repr(C)]
struct XSelectionEvent {
    kind: c_int,
    serial: c_ulong,
    send_event: c_int,
    display: *mut Display,
    requestor: XWindow,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: c_ulong,
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
#[cfg(target_os = "linux")]
const LC_CTYPE: c_int = 0;
#[cfg(not(target_os = "linux"))]
const LC_CTYPE: c_int = 2;

unsafe extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

/// `XCreateIC` takes a NULL-terminated list of name / value pairs.
type CreateIc = unsafe extern "C" fn(*mut c_void, ...) -> *mut c_void;

/// Declares the Xlib functions we use and a table holding them, resolved with `dlsym`.
macro_rules! xlib {
    ($($name:ident: fn($($arg:ty),*) -> $ret:ty;)*) => {
        struct Xlib {
            lib: *mut c_void,
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
                        lib,
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
    XFilterEvent: fn(*mut XEvent, XWindow) -> c_int;
    XLookupString: fn(*mut XInputEvent, *mut c_char, c_int, *mut KeySym, *mut c_void) -> c_int;
    Xutf8LookupString: fn(*mut c_void, *mut XInputEvent, *mut c_char, c_int, *mut KeySym, *mut c_int) -> c_int;
    XSetLocaleModifiers: fn(*const c_char) -> *mut c_char;
    XOpenIM: fn(*mut Display, *mut c_void, *mut c_char, *mut c_char) -> *mut c_void;
    XCloseIM: fn(*mut c_void) -> c_int;
    XDestroyIC: fn(*mut c_void) -> ();
    XSetICFocus: fn(*mut c_void) -> ();
    XSetSelectionOwner: fn(*mut Display, Atom, XWindow, c_ulong) -> c_int;
    XGetSelectionOwner: fn(*mut Display, Atom) -> XWindow;
    XConvertSelection: fn(*mut Display, Atom, Atom, Atom, XWindow, c_ulong) -> c_int;
    XCheckTypedWindowEvent: fn(*mut Display, XWindow, c_int, *mut XEvent) -> c_int;
    XGetWindowProperty: fn(
        *mut Display, XWindow, Atom, c_long, c_long, c_int, Atom,
        *mut Atom, *mut c_int, *mut c_ulong, *mut c_ulong, *mut *mut u8
    ) -> c_int;
    XSendEvent: fn(*mut Display, XWindow, c_int, c_long, *mut XEvent) -> c_int;
    XCreateFontCursor: fn(*mut Display, c_uint) -> c_ulong;
    XDefineCursor: fn(*mut Display, XWindow, c_ulong) -> c_int;
    XFreeCursor: fn(*mut Display, c_ulong) -> c_int;
    XResourceManagerString: fn(*mut Display) -> *mut c_char;
    XSetErrorHandler: fn(Option<ErrorHandler>) -> *mut c_void;
}

type ErrorHandler = unsafe extern "C" fn(*mut Display, *mut c_void) -> c_int;

/// Protocol errors are not fatal for us (the default handler exits the process): the
/// usual one is a clipboard requestor whose window is gone before our answer arrives.
unsafe extern "C" fn ignore_x_error(_display: *mut Display, _event: *mut c_void) -> c_int {
    0
}

const KEY_PRESS: c_int = 2;
const KEY_RELEASE: c_int = 3;
const BUTTON_PRESS: c_int = 4;
const BUTTON_RELEASE: c_int = 5;
const MOTION_NOTIFY: c_int = 6;
const FOCUS_IN: c_int = 9;
const FOCUS_OUT: c_int = 10;
const EXPOSE: c_int = 12;
const CONFIGURE_NOTIFY: c_int = 22;
const SELECTION_CLEAR: c_int = 29;
const SELECTION_REQUEST: c_int = 30;
const SELECTION_NOTIFY: c_int = 31;
const CLIENT_MESSAGE: c_int = 33;

const XA_ATOM: Atom = 4;
const XA_STRING: Atom = 31;
const CURRENT_TIME: c_ulong = 0;
const XIM_PREEDIT_NOTHING: c_long = 0x0008;
const XIM_STATUS_NOTHING: c_long = 0x0400;
const XLOOKUP_CHARS: c_int = 2;
const XLOOKUP_BOTH: c_int = 4;

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
        0xFF0D | 0xFF8D => Key::Enter,
        0xFF09 | 0xFE20 => Key::Tab,
        0xFF08 => Key::Backspace,
        0xFFFF | 0xFF9F => Key::Delete,
        0xFF63 | 0xFF9E => Key::Insert,
        0xFF57 | 0xFF9C => Key::End,
        0xFF55 | 0xFF9A => Key::PageUp,
        0xFF56 | 0xFF9B => Key::PageDown,
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
    /// Input method and context for typed text (null: Latin-1 through `XLookupString`).
    im: *mut c_void,
    ic: *mut c_void,
    clipboard: Atom,
    targets: Atom,
    text_atom: Atom,
    /// The property our clipboard requests are delivered to.
    transfer: Atom,
    /// What we offer as the CLIPBOARD selection while we own it.
    clip: Option<String>,
    cursors: Vec<(Cursor, c_ulong)>,
    cursor: Cursor,
    scale: f32,
}

/// `Xft.dpi` from the resource database string (what desktops set for high-DPI screens), over 96.
fn xft_scale(resources: &str) -> Option<f32> {
    let line = resources.lines().find_map(|l| l.strip_prefix("Xft.dpi:"))?;
    let dpi: f32 = line.trim().parse().ok()?;
    (dpi > 0.0).then_some(dpi / 96.0)
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
            (x.XSetErrorHandler)(Some(ignore_x_error));
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
            // Typed text: an input method in the user's locale (dead keys, compose, IMEs).
            let ctype = setlocale(LC_CTYPE, null());
            if ctype.is_null() || CStr::from_ptr(ctype).to_bytes() == b"C" {
                setlocale(LC_CTYPE, c"".as_ptr());
            }
            (x.XSetLocaleModifiers)(c"".as_ptr());
            let im = (x.XOpenIM)(display, null_mut(), null_mut(), null_mut());
            let mut ic = null_mut();
            let create_ic = dlsym(x.lib, c"XCreateIC".as_ptr());
            if !im.is_null() && !create_ic.is_null() {
                let create_ic = std::mem::transmute::<*mut c_void, CreateIc>(create_ic);
                ic = create_ic(
                    im,
                    c"inputStyle".as_ptr(),
                    XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
                    c"clientWindow".as_ptr(),
                    window,
                    c"focusWindow".as_ptr(),
                    window,
                    null::<c_char>(),
                );
                if !ic.is_null() {
                    (x.XSetICFocus)(ic);
                }
            }
            let resources = (x.XResourceManagerString)(display);
            let scale =
                if resources.is_null() { None } else { xft_scale(&CStr::from_ptr(resources).to_string_lossy()) };
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
            let clipboard = (x.XInternAtom)(display, c"CLIPBOARD".as_ptr(), 0);
            let targets = (x.XInternAtom)(display, c"TARGETS".as_ptr(), 0);
            let text_atom = (x.XInternAtom)(display, c"TEXT".as_ptr(), 0);
            let transfer = (x.XInternAtom)(display, c"PATHTRACER_CLIPBOARD".as_ptr(), 0);
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
                im,
                ic,
                clipboard,
                targets,
                text_atom,
                transfer,
                clip: None,
                cursors: Vec::new(),
                cursor: Cursor::Arrow,
                scale: scale.unwrap_or(1.0),
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
                // Keys the input method consumes (dead keys, compose sequences).
                if (self.x.XFilterEvent)(&raw mut ev, 0) != 0 {
                    continue;
                }
                let p = (&raw mut ev).cast::<XInputEvent>();
                match ev.kind() {
                    KEY_PRESS | KEY_RELEASE => {
                        let key = translate_key((self.x.XLookupKeysym)(p, 0));
                        let mods = modifiers((*p).state);
                        if ev.kind() == KEY_PRESS {
                            let repeat = !self.down.insert(key);
                            out.push(Event::KeyDown { key, mods, repeat });
                            for c in self.typed(p).chars().filter(|c| !c.is_control()) {
                                out.push(Event::Text(c));
                            }
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
                    FOCUS_IN => {
                        if !self.ic.is_null() {
                            (self.x.XSetICFocus)(self.ic);
                        }
                    }
                    FOCUS_OUT => {
                        self.down.clear();
                        out.push(Event::FocusLost);
                    }
                    SELECTION_REQUEST => self.answer_selection(&*(&raw const ev).cast::<XSelectionRequestEvent>()),
                    SELECTION_CLEAR => {
                        let e = &*(&raw const ev).cast::<XSelectionClearEvent>();
                        if e.selection == self.clipboard {
                            self.clip = None;
                        }
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

    /// The text a key press types (empty for keys that type nothing).
    ///
    /// # Safety
    /// `p` must point to a live key press event of this window.
    unsafe fn typed(&self, p: *mut XInputEvent) -> String {
        let mut buf = [0u8; 64];
        let mut sym: KeySym = 0;
        // SAFETY: per the contract; `buf` is writable for its whole length.
        unsafe {
            if self.ic.is_null() {
                let n =
                    (self.x.XLookupString)(p, buf.as_mut_ptr().cast(), buf.len() as c_int, &raw mut sym, null_mut());
                // Latin-1 without an input method.
                buf[..n.clamp(0, 64) as usize].iter().map(|&b| char::from(b)).collect()
            } else {
                let mut status = 0;
                let n = (self.x.Xutf8LookupString)(
                    self.ic,
                    p,
                    buf.as_mut_ptr().cast(),
                    buf.len() as c_int,
                    &raw mut sym,
                    &raw mut status,
                );
                if status == XLOOKUP_CHARS || status == XLOOKUP_BOTH {
                    String::from_utf8_lossy(&buf[..n.clamp(0, 64) as usize]).into_owned()
                } else {
                    String::new()
                }
            }
        }
    }

    /// Another client asks for our CLIPBOARD contents: write them to its property and tell it.
    ///
    /// # Safety
    /// `r` must be a selection request delivered to this window.
    unsafe fn answer_selection(&self, r: &XSelectionRequestEvent) {
        // Obsolete clients leave the property empty and mean the target.
        let property = if r.property == 0 { r.target } else { r.property };
        let mut reply = XSelectionEvent {
            kind: SELECTION_NOTIFY,
            serial: 0,
            send_event: 1,
            display: self.display,
            requestor: r.requestor,
            selection: r.selection,
            target: r.target,
            property: 0,
            time: r.time,
        };
        // SAFETY: the property data outlives the XChangeProperty calls; the reply is copied
        // into a full-size XEvent for XSendEvent.
        unsafe {
            if let (true, Some(text)) = (r.selection == self.clipboard, &self.clip) {
                if r.target == self.targets {
                    let atoms = [self.targets, self.utf8_string, XA_STRING];
                    (self.x.XChangeProperty)(
                        self.display,
                        r.requestor,
                        property,
                        XA_ATOM,
                        32,
                        PROP_MODE_REPLACE,
                        atoms.as_ptr().cast(),
                        atoms.len() as c_int,
                    );
                    reply.property = property;
                } else if r.target == self.utf8_string || r.target == self.text_atom || r.target == XA_STRING {
                    let bytes: Vec<u8> = if r.target == XA_STRING {
                        text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect()
                    } else {
                        text.as_bytes().to_vec()
                    };
                    let ty = if r.target == XA_STRING { XA_STRING } else { self.utf8_string };
                    (self.x.XChangeProperty)(
                        self.display,
                        r.requestor,
                        property,
                        ty,
                        8,
                        PROP_MODE_REPLACE,
                        bytes.as_ptr(),
                        bytes.len() as c_int,
                    );
                    reply.property = property;
                }
            }
            let mut ev = XEvent { pad: [0; 24] };
            std::ptr::write((&raw mut ev).cast::<XSelectionEvent>(), reply);
            (self.x.XSendEvent)(self.display, r.requestor, 0, 0, &raw mut ev);
            (self.x.XFlush)(self.display);
        }
    }

    /// The CLIPBOARD selection as text: ours, or asked from its owner (waiting up to half a
    /// second for the answer).
    pub fn clipboard_text(&mut self) -> Option<String> {
        // SAFETY: Xlib calls on this window's display; the property data is copied and then
        // released with XFree; `ev` is a full-size XEvent.
        unsafe {
            let owner = (self.x.XGetSelectionOwner)(self.display, self.clipboard);
            if owner == self.win {
                return self.clip.clone();
            }
            if owner == 0 {
                return None;
            }
            (self.x.XConvertSelection)(
                self.display,
                self.clipboard,
                self.utf8_string,
                self.transfer,
                self.win,
                CURRENT_TIME,
            );
            (self.x.XFlush)(self.display);
            let start = Instant::now();
            let mut ev = XEvent { pad: [0; 24] };
            loop {
                if (self.x.XCheckTypedWindowEvent)(self.display, self.win, SELECTION_NOTIFY, &raw mut ev) != 0 {
                    break;
                }
                if start.elapsed() > Duration::from_millis(500) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if (*(&raw const ev).cast::<XSelectionEvent>()).property == 0 {
                return None; // the owner cannot give text
            }
            let (mut ty, mut format, mut items, mut after) = (0, 0, 0, 0);
            let mut data: *mut u8 = null_mut();
            (self.x.XGetWindowProperty)(
                self.display,
                self.win,
                self.transfer,
                0,
                1 << 24,
                1,
                0,
                &raw mut ty,
                &raw mut format,
                &raw mut items,
                &raw mut after,
                &raw mut data,
            );
            if data.is_null() {
                return None;
            }
            let text = (format == 8)
                .then(|| String::from_utf8_lossy(std::slice::from_raw_parts(data, items as usize)).into_owned());
            (self.x.XFree)(data.cast());
            text
        }
    }

    /// Owns the CLIPBOARD selection with `text`; other clients ask us for it.
    pub fn set_clipboard_text(&mut self, text: &str) {
        self.clip = Some(text.to_owned());
        // SAFETY: plain Xlib calls on this window.
        unsafe {
            (self.x.XSetSelectionOwner)(self.display, self.clipboard, self.win, CURRENT_TIME);
            (self.x.XFlush)(self.display);
        }
    }

    pub fn set_cursor(&mut self, cursor: Cursor) {
        if cursor == self.cursor {
            return;
        }
        self.cursor = cursor;
        let shape = match cursor {
            Cursor::Arrow => 68,    // XC_left_ptr
            Cursor::Text => 152,    // XC_xterm
            Cursor::ResizeH => 108, // XC_sb_h_double_arrow
            Cursor::ResizeV => 116, // XC_sb_v_double_arrow
            Cursor::Hand => 60,     // XC_hand2
            Cursor::Move => 52,     // XC_fleur
        };
        // SAFETY: font cursors are created once per shape, kept until Drop, and defined on
        // this window.
        unsafe {
            let known = self.cursors.iter().find(|(k, _)| *k == cursor).map(|&(_, c)| c);
            let c = known.unwrap_or_else(|| {
                let c = (self.x.XCreateFontCursor)(self.display, shape);
                self.cursors.push((cursor, c));
                c
            });
            (self.x.XDefineCursor)(self.display, self.win, c);
            (self.x.XFlush)(self.display);
        }
    }

    /// `Xft.dpi` / 96, as desktops set it for high-DPI screens (1 when unset).
    pub fn scale_factor(&self) -> f32 {
        self.scale
    }

    pub fn double_click_time(&self) -> Duration {
        super::DOUBLE_CLICK
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
            for &(_, c) in &self.cursors {
                (self.x.XFreeCursor)(self.display, c);
            }
            if !self.ic.is_null() {
                (self.x.XDestroyIC)(self.ic);
            }
            if !self.im.is_null() {
                (self.x.XCloseIM)(self.im);
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
