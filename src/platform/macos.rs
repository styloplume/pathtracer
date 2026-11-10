//! macOS window through Cocoa, driven with raw calls into the Objective-C runtime
//! (`objc_msgSend`), no crates.
//!
//! The framebuffer becomes a `CGImage` set as the contents of the content view's layer;
//! Core Animation stretches it over the window (nearest-neighbour filtering, as the
//! progressive preview's blocks want). The text overlay is a `CATextLayer` on top.
//! Events are pumped without blocking from `poll_events`; the window delegate (a small
//! class registered at run time) reports close requests and focus loss.

#![expect(
    unsafe_code,
    reason = "raw Objective-C runtime / CoreGraphics FFI; the platform layer is the only place allowed to use unsafe"
)]
#![expect(non_upper_case_globals, reason = "mirror the Cocoa constant names")]

use super::{Event, Key, Modifiers, MouseButton};
use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

type Id = *mut c_void;
type Sel = *mut c_void;
type Bool = i8;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, extra: usize) -> Id;
    fn class_addMethod(cls: Id, name: Sel, imp: *const c_void, types: *const c_char) -> Bool;
    fn objc_registerClassPair(cls: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

// Linked for their Objective-C classes (NSWindow, NSString, CATextLayer, ...).
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, len: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *mut c_void;
    fn CGDataProviderRelease(provider: *mut c_void);
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        interpolate: bool,
        intent: i32,
    ) -> *mut c_void;
    fn CGImageRelease(image: *mut c_void);
    fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> *mut c_void;
    fn CGColorRelease(color: *mut c_void);
}

/// Sends `$sel` to `$obj` with typed arguments: `msg![ret; obj, c"sel:", arg => Type, ...]`.
/// `objc_msgSend` must be called through a pointer of the method's exact signature.
macro_rules! msg {
    ($ret:ty; $obj:expr, $sel:expr $(, $arg:expr => $t:ty)* $(,)?) => {{
        let f = std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(Id, Sel $(, $t)*) -> $ret>(
            objc_msgSend,
        );
        f($obj, sel($sel) $(, $arg)*)
    }};
}

fn sel(name: &CStr) -> Sel {
    // SAFETY: `name` is NUL-terminated; registering a selector has no other preconditions.
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &CStr) -> Id {
    // SAFETY: `name` is NUL-terminated.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// A method returning an `NSRect`: on x86-64 such structs come back through memory.
///
/// # Safety
/// `obj` must respond to `sel` with a method returning `NSRect` and taking no arguments.
unsafe fn msg_rect(obj: Id, name: &CStr) -> NSRect {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: per the caller's contract; `_stret` takes the return slot as a hidden first argument.
    unsafe {
        let f = std::mem::transmute::<unsafe extern "C" fn(), unsafe extern "C" fn(*mut NSRect, Id, Sel)>(
            objc_msgSend_stret,
        );
        let mut r = NSRect::default();
        f(&raw mut r, obj, sel(name));
        r
    }
    #[cfg(not(target_arch = "x86_64"))]
    // SAFETY: per the caller's contract.
    unsafe {
        msg![NSRect; obj, name]
    }
}

/// An autoreleased `NSString`.
///
/// # Safety
/// Must run inside an autorelease pool.
unsafe fn ns_string(s: &str) -> Id {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    // SAFETY: `c` is NUL-terminated UTF-8 and outlives the call, which copies it.
    unsafe { msg![Id; class(c"NSString"), c"stringWithUTF8String:", c.as_ptr() => *const c_char] }
}

/// Runs `f` inside an autorelease pool.
fn with_pool<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: push and pop are paired on this thread.
    let pool = unsafe { objc_autoreleasePoolPush() };
    let r = f();
    // SAFETY: `pool` was pushed above and nothing pushed after it is still open.
    unsafe { objc_autoreleasePoolPop(pool) };
    r
}

static CLOSE_REQUESTED: AtomicBool = AtomicBool::new(false);
static FOCUS_LOST: AtomicBool = AtomicBool::new(false);

extern "C" fn window_should_close(_this: Id, _cmd: Sel, _sender: Id) -> Bool {
    CLOSE_REQUESTED.store(true, Ordering::Relaxed);
    0 // the application decides when to close
}

extern "C" fn window_did_resign_key(_this: Id, _cmd: Sel, _notification: Id) {
    FOCUS_LOST.store(true, Ordering::Relaxed);
}

/// Registers (once) and instantiates the window delegate class.
///
/// # Safety
/// Must run on the main thread.
unsafe fn make_delegate() -> Id {
    // SAFETY: class creation follows the runtime's documented sequence; the method
    // implementations match the type encodings given for them.
    unsafe {
        let mut cls = class(c"PathTracerWindowDelegate");
        if cls.is_null() {
            cls = objc_allocateClassPair(class(c"NSObject"), c"PathTracerWindowDelegate".as_ptr(), 0);
            let should_close: extern "C" fn(Id, Sel, Id) -> Bool = window_should_close;
            let resign: extern "C" fn(Id, Sel, Id) = window_did_resign_key;
            class_addMethod(cls, sel(c"windowShouldClose:"), should_close as *const c_void, c"c@:@".as_ptr());
            class_addMethod(cls, sel(c"windowDidResignKey:"), resign as *const c_void, c"v@:@".as_ptr());
            objc_registerClassPair(cls);
        }
        msg![Id; msg![Id; cls, c"alloc"], c"init"]
    }
}

// NSEventType values.
const LEFT_MOUSE_DOWN: usize = 1;
const LEFT_MOUSE_UP: usize = 2;
const RIGHT_MOUSE_DOWN: usize = 3;
const RIGHT_MOUSE_UP: usize = 4;
const MOUSE_MOVED: usize = 5;
const LEFT_MOUSE_DRAGGED: usize = 6;
const RIGHT_MOUSE_DRAGGED: usize = 7;
const KEY_DOWN: usize = 10;
const KEY_UP: usize = 11;
const FLAGS_CHANGED: usize = 12;
const SCROLL_WHEEL: usize = 22;
const OTHER_MOUSE_DOWN: usize = 25;
const OTHER_MOUSE_UP: usize = 26;
const OTHER_MOUSE_DRAGGED: usize = 27;

// NSEventModifierFlags.
const NSEventModifierFlagShift: usize = 1 << 17;
const NSEventModifierFlagControl: usize = 1 << 18;
const NSEventModifierFlagOption: usize = 1 << 19;

const NSWindowStyleMaskTitledClosableMiniaturizableResizable: usize = 1 | 2 | 4 | 8;
const NSBackingStoreBuffered: usize = 2;
const NSApplicationActivationPolicyRegular: isize = 0;
/// kCGImageAlphaNoneSkipFirst | kCGBitmapByteOrder32Little: 0x00RRGGBB native words.
const BITMAP_XRGB32_LITTLE: u32 = 6 | (2 << 12);

fn modifiers(flags: usize) -> Modifiers {
    Modifiers {
        shift: flags & NSEventModifierFlagShift != 0,
        ctrl: flags & NSEventModifierFlagControl != 0,
        alt: flags & NSEventModifierFlagOption != 0,
    }
}

/// Keys with fixed virtual key codes; the others come from the typed character.
fn special_key(code: u16) -> Option<Key> {
    Some(match code {
        0x35 => Key::Escape,
        0x31 => Key::Space,
        0x73 => Key::Home,
        0x7B => Key::Left,
        0x7C => Key::Right,
        0x7D => Key::Down,
        0x7E => Key::Up,
        0x45 => Key::Plus,  // keypad +
        0x4E => Key::Minus, // keypad -
        0x7A => Key::F(1),
        0x78 => Key::F(2),
        0x63 => Key::F(3),
        0x76 => Key::F(4),
        0x60 => Key::F(5),
        0x61 => Key::F(6),
        0x62 => Key::F(7),
        0x64 => Key::F(8),
        0x65 => Key::F(9),
        0x6D => Key::F(10),
        0x67 => Key::F(11),
        0x6F => Key::F(12),
        _ => return None,
    })
}

fn char_key(c: char, code: u16) -> Key {
    match c.to_ascii_lowercase() {
        // '=' is the unshifted '+' key, as on Windows.
        '+' | '=' => Key::Plus,
        '-' => Key::Minus,
        c if c.is_ascii_alphanumeric() => Key::Char(c),
        _ => Key::Other(u32::from(code)),
    }
}

pub struct Window {
    nswin: Id,
    view: Id,
    text: Id,
    delegate: Id,
    /// Modifier state of the last `FlagsChanged` event (to emit Shift / Ctrl key events).
    flags: usize,
    /// Whether the first `Paint` was reported (Core Animation keeps the contents after that).
    painted: bool,
}

impl Window {
    pub fn new(title: &str, w: u32, h: u32) -> Result<Window, String> {
        with_pool(|| {
            // SAFETY: standard Cocoa set-up on the main thread (where `app::run` calls this);
            // every message matches the receiver's documented signature.
            unsafe {
                let app = msg![Id; class(c"NSApplication"), c"sharedApplication"];
                if app.is_null() {
                    return Err("cannot initialize NSApplication".to_string());
                }
                msg![Bool; app, c"setActivationPolicy:", NSApplicationActivationPolicyRegular => isize];

                // Fit the requested pixel size (in points: divide by the backing scale) on screen.
                let screen = msg![Id; class(c"NSScreen"), c"mainScreen"];
                let (scale, visible) = if screen.is_null() {
                    (1.0, NSRect { origin: NSPoint::default(), size: NSSize { width: 1e9, height: 1e9 } })
                } else {
                    (msg![f64; screen, c"backingScaleFactor"], msg_rect(screen, c"visibleFrame"))
                };
                let (pw, ph) = (f64::from(w) / scale, f64::from(h) / scale);
                let fit = (0.9 * visible.size.width / pw).min(0.85 * visible.size.height / ph).min(1.0);
                let rect = NSRect { origin: NSPoint::default(), size: NSSize { width: pw * fit, height: ph * fit } };

                let window = msg![Id; msg![Id; class(c"NSWindow"), c"alloc"],
                    c"initWithContentRect:styleMask:backing:defer:",
                    rect => NSRect,
                    NSWindowStyleMaskTitledClosableMiniaturizableResizable => usize,
                    NSBackingStoreBuffered => usize,
                    false => bool];
                if window.is_null() {
                    return Err("cannot create NSWindow".to_string());
                }
                msg![(); window, c"setReleasedWhenClosed:", false => bool];
                msg![(); window, c"setAcceptsMouseMovedEvents:", true => bool];
                let delegate = make_delegate();
                msg![(); window, c"setDelegate:", delegate => Id];
                msg![(); window, c"center"];

                // Layer-backed content view: the image is the layer's contents, scaled by Core Animation.
                let view = msg![Id; window, c"contentView"];
                msg![(); view, c"setWantsLayer:", true => bool];
                let layer = msg![Id; view, c"layer"];
                let nearest = ns_string("nearest");
                msg![(); layer, c"setMagnificationFilter:", nearest => Id];
                msg![(); layer, c"setMinificationFilter:", nearest => Id];
                let black = CGColorCreateGenericRGB(0.0, 0.0, 0.0, 1.0);
                msg![(); layer, c"setBackgroundColor:", black => *mut c_void];
                CGColorRelease(black);

                // Overlay text: white monospace with a dark shadow, anchored top-left.
                let text = msg![Id; msg![Id; class(c"CATextLayer"), c"alloc"], c"init"];
                msg![(); text, c"setFont:", ns_string("Menlo") => Id];
                msg![(); text, c"setFontSize:", 13.0 => f64];
                let white = CGColorCreateGenericRGB(1.0, 1.0, 1.0, 1.0);
                msg![(); text, c"setForegroundColor:", white => *mut c_void];
                CGColorRelease(white);
                msg![(); text, c"setShadowOpacity:", 1.0f32 => f32];
                msg![(); text, c"setShadowRadius:", 1.0 => f64];
                msg![(); text, c"setShadowOffset:", NSSize { width: 1.0, height: -1.0 } => NSSize];
                msg![(); text, c"setContentsScale:", scale => f64];
                msg![(); layer, c"addSublayer:", text => Id];

                msg![(); window, c"setTitle:", ns_string(title) => Id];
                msg![(); window, c"makeKeyAndOrderFront:", null_mut::<c_void>() => Id];
                msg![(); app, c"finishLaunching"];
                msg![(); app, c"activateIgnoringOtherApps:", true => bool];
                Ok(Window { nswin: window, view, text, delegate, flags: 0, painted: false })
            }
        })
    }

    /// Backing scale (physical pixels per point) of the window's screen.
    fn scale(&self) -> f64 {
        // SAFETY: `self.nswin` is a live NSWindow.
        let s = unsafe { msg![f64; self.nswin, c"backingScaleFactor"] };
        if s > 0.0 { s } else { 1.0 }
    }

    /// Content area in points.
    fn bounds(&self) -> NSRect {
        // SAFETY: `self.view` is the live content view; `bounds` returns an NSRect.
        unsafe { msg_rect(self.view, c"bounds") }
    }

    pub fn client_size(&self) -> (u32, u32) {
        let (b, s) = (self.bounds(), self.scale());
        ((b.size.width * s).round().max(0.0) as u32, (b.size.height * s).round().max(0.0) as u32)
    }

    pub fn poll_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        let size_before = self.client_size();
        with_pool(|| {
            // SAFETY: event pumping on the main thread; every message matches the documented
            // signature of NSApplication / NSEvent / NSDate.
            unsafe {
                let app = msg![Id; class(c"NSApplication"), c"sharedApplication"];
                let past = msg![Id; class(c"NSDate"), c"distantPast"];
                let mode = ns_string("kCFRunLoopDefaultMode");
                loop {
                    let ev = msg![Id; app, c"nextEventMatchingMask:untilDate:inMode:dequeue:",
                        usize::MAX => usize, past => Id, mode => Id, true => bool];
                    if ev.is_null() {
                        break;
                    }
                    // Key presses are ours (forwarding them would make AppKit beep).
                    if !self.translate(ev, &mut out) {
                        msg![(); app, c"sendEvent:", ev => Id];
                    }
                }
                msg![(); app, c"updateWindows"];
            }
        });
        if CLOSE_REQUESTED.swap(false, Ordering::Relaxed) {
            out.push(Event::Close);
        }
        if FOCUS_LOST.swap(false, Ordering::Relaxed) {
            self.flags = 0;
            out.push(Event::FocusLost);
        }
        let size = self.client_size();
        if size != size_before && size.0 > 0 && size.1 > 0 {
            out.push(Event::Resized(size.0, size.1));
        }
        if !self.painted {
            self.painted = true;
            out.push(Event::Paint);
        }
        out
    }

    /// Appends the events `ev` stands for; returns true if it was consumed (not to be
    /// forwarded to `AppKit`).
    ///
    /// # Safety
    /// `ev` must be a live `NSEvent`; must run inside an autorelease pool.
    unsafe fn translate(&mut self, ev: Id, out: &mut Vec<Event>) -> bool {
        // SAFETY: per the contract; messages match NSEvent's documented signatures.
        unsafe {
            let kind = msg![usize; ev, c"type"];
            let flags = msg![usize; ev, c"modifierFlags"];
            let mods = modifiers(flags);
            match kind {
                KEY_DOWN | KEY_UP => {
                    if msg![Id; ev, c"window"] != self.nswin {
                        return false;
                    }
                    let code = msg![u16; ev, c"keyCode"];
                    let key = special_key(code).unwrap_or_else(|| {
                        let chars = msg![Id; ev, c"charactersIgnoringModifiers"];
                        let utf8 =
                            if chars.is_null() { std::ptr::null() } else { msg![*const c_char; chars, c"UTF8String"] };
                        let c = if utf8.is_null() {
                            None
                        } else {
                            CStr::from_ptr(utf8).to_str().ok().and_then(|s| s.chars().next())
                        };
                        c.map_or(Key::Other(u32::from(code)), |c| char_key(c, code))
                    });
                    if kind == KEY_DOWN {
                        let repeat = msg![Bool; ev, c"isARepeat"] != 0;
                        out.push(Event::KeyDown { key, mods, repeat });
                    } else {
                        out.push(Event::KeyUp { key, mods });
                    }
                    true
                }
                FLAGS_CHANGED => {
                    for (mask, key) in [(NSEventModifierFlagShift, Key::Shift), (NSEventModifierFlagControl, Key::Ctrl)]
                    {
                        match (self.flags & mask != 0, flags & mask != 0) {
                            (false, true) => out.push(Event::KeyDown { key, mods, repeat: false }),
                            (true, false) => out.push(Event::KeyUp { key, mods }),
                            _ => {}
                        }
                    }
                    self.flags = flags;
                    false
                }
                LEFT_MOUSE_DOWN | LEFT_MOUSE_UP | RIGHT_MOUSE_DOWN | RIGHT_MOUSE_UP | OTHER_MOUSE_DOWN
                | OTHER_MOUSE_UP | MOUSE_MOVED | LEFT_MOUSE_DRAGGED | RIGHT_MOUSE_DRAGGED | OTHER_MOUSE_DRAGGED => {
                    if msg![Id; ev, c"window"] != self.nswin {
                        return false;
                    }
                    // Window coordinates (points, origin bottom-left) to content pixels, origin top-left.
                    let p = msg![NSPoint; ev, c"locationInWindow"];
                    let (b, s) = (self.bounds(), self.scale());
                    let (x, y) = ((p.x * s).round() as i32, ((b.size.height - p.y) * s).round() as i32);
                    let inside = p.x >= 0.0 && p.y >= 0.0 && p.x < b.size.width && p.y < b.size.height;
                    let button = match kind {
                        LEFT_MOUSE_DOWN | LEFT_MOUSE_UP => MouseButton::Left,
                        RIGHT_MOUSE_DOWN | RIGHT_MOUSE_UP => MouseButton::Right,
                        _ => MouseButton::Middle,
                    };
                    match kind {
                        // Presses on the title bar belong to AppKit (moving the window).
                        LEFT_MOUSE_DOWN | RIGHT_MOUSE_DOWN | OTHER_MOUSE_DOWN if inside => {
                            out.push(Event::MouseDown { button, x, y, mods });
                        }
                        LEFT_MOUSE_UP | RIGHT_MOUSE_UP | OTHER_MOUSE_UP => {
                            out.push(Event::MouseUp { button, x, y, mods });
                        }
                        MOUSE_MOVED | LEFT_MOUSE_DRAGGED | RIGHT_MOUSE_DRAGGED | OTHER_MOUSE_DRAGGED => {
                            out.push(Event::MouseMove { x, y, mods });
                        }
                        _ => {}
                    }
                    false
                }
                SCROLL_WHEEL => {
                    let dy = msg![f64; ev, c"scrollingDeltaY"];
                    // Trackpads report points; a mouse wheel reports lines (notches).
                    let precise = msg![Bool; ev, c"hasPreciseScrollingDeltas"] != 0;
                    let delta = if precise { dy / 10.0 } else { dy };
                    if delta != 0.0 {
                        out.push(Event::Wheel { delta: delta as f32, mods });
                    }
                    false
                }
                _ => false,
            }
        }
    }

    /// Shows a `w`x`h` buffer of 0x00RRGGBB pixels stretched over the window, with
    /// `overlay` lines of text in the top-left corner.
    pub fn present(&mut self, pixels: &[u32], w: usize, h: usize, overlay: &[String]) {
        if w == 0 || h == 0 || pixels.len() < w * h {
            return;
        }
        with_pool(|| {
            // SAFETY: the CFData copies the pixels, so the CGImage owns its memory; every
            // CoreFoundation object created here is released once, after the layer retained
            // what it keeps. Messages match the documented CALayer / CATransaction signatures.
            unsafe {
                let data = CFDataCreate(std::ptr::null(), pixels.as_ptr().cast(), (w * h * 4) as isize);
                let provider = CGDataProviderCreateWithCFData(data);
                let space = CGColorSpaceCreateDeviceRGB();
                let image = CGImageCreate(
                    w,
                    h,
                    8,
                    32,
                    w * 4,
                    space,
                    BITMAP_XRGB32_LITTLE,
                    provider,
                    std::ptr::null(),
                    false,
                    0,
                );
                CGColorSpaceRelease(space);
                CGDataProviderRelease(provider);
                CFRelease(data);
                if image.is_null() {
                    return;
                }

                let transaction = class(c"CATransaction");
                msg![(); transaction, c"begin"];
                // Contents changes would otherwise cross-fade.
                msg![(); transaction, c"setDisableActions:", true => bool];
                let layer = msg![Id; self.view, c"layer"];
                msg![(); layer, c"setContents:", image => *mut c_void];
                let b = self.bounds();
                let pad = 8.0;
                let frame = NSRect {
                    origin: NSPoint { x: pad, y: 0.0 },
                    size: NSSize { width: (b.size.width - 2.0 * pad).max(0.0), height: (b.size.height - pad).max(0.0) },
                };
                msg![(); self.text, c"setFrame:", frame => NSRect];
                msg![(); self.text, c"setString:", ns_string(&overlay.join("\n")) => Id];
                msg![(); self.text, c"setContentsScale:", self.scale() => f64];
                msg![(); transaction, c"commit"];
                CGImageRelease(image);
            }
        });
    }

    pub fn set_title(&self, title: &str) {
        with_pool(|| {
            // SAFETY: `self.nswin` is live; the NSString is autoreleased into this pool.
            unsafe { msg![(); self.nswin, c"setTitle:", ns_string(title) => Id] }
        });
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        with_pool(|| {
            // SAFETY: the window, text layer and delegate were created (+1) in `new` and are
            // released exactly once; the delegate is detached before it is released.
            unsafe {
                msg![(); self.nswin, c"setDelegate:", null_mut::<c_void>() => Id];
                msg![(); self.nswin, c"close"];
                msg![(); self.nswin, c"release"];
                msg![(); self.text, c"release"];
                msg![(); self.delegate, c"release"];
            }
        });
    }
}
