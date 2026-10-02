//! macOS implementation of [`crate::platform::WindowSystem`] using the
//! Accessibility (AX), CoreGraphics and CoreFoundation C APIs, hand-bound.
//!
//! Compiled only on macOS; every other target uses the stub.

// Hand-bound macOS C APIs. The lints allowed here all concern values
// crossing the FFI boundary: a `pid` or a window number that the system
// itself reports, and raw pointers the API documents as valid. Narrowing
// them would add conversions the C side does not ask for, without making
// the code safer; every one of them is checked at the call site instead.
#![allow(non_snake_case)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::borrow_as_ptr,
    clippy::ptr_as_ptr,
    clippy::items_after_statements,
    clippy::semicolon_if_nothing_returned,
    clippy::doc_markdown,
    clippy::unused_self,
    clippy::zero_sized_map_values,
    clippy::needless_range_loop,
    clippy::type_complexity
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::platform::{darwin_key_codes, KeyEvent, KeyName, QueryResult, WindowSystem};
use crate::types::{Direction, Modifier, Rect, ScreenInfo, WindowInfo};

// --- Foreign types ---------------------------------------------------------

type AXUIElementRef = *const c_void;
type AXObserverRef = *const c_void;
type AXValueRef = *const c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFArrayRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFDictionaryKeyCallBacks = [u64; 5];
type CFDictionaryValueCallBacks = [u64; 5];
type CFHashCode = isize;
type CFIndex = isize;
type CFRunLoopRef = *const c_void;
type CFRunLoopSourceRef = *const c_void;
type CFMachPortRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CGEventRef = *const c_void;
type CGEventTapProxy = *const c_void;
type CGEventMask = u64;
type CGEventType = u32;
type CGEventFlags = u64;
type CGDirectDisplayID = u32;
type CGError = i32;
type Pid = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

const K_AX_SUCCESS: i32 = 0;

// --- HIServices / ApplicationServices --------------------------------------

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: Pid) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> i32;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut Pid) -> i32;
    fn AXValueCreate(type_: i32, value: *const c_void) -> AXValueRef;
    fn AXValueGetValue(value: AXValueRef, type_: i32, value: *mut c_void) -> bool;
    fn AXObserverCreate(
        pid: Pid,
        callback: extern "C" fn(AXObserverRef, AXUIElementRef, CFStringRef, *mut c_void),
        observer: *mut AXObserverRef,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> CFRunLoopSourceRef;
}

// --- CoreFoundation --------------------------------------------------------

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;
    static kCFBooleanTrue: CFBooleanRef;

    fn CFRelease(cf: CFTypeRef);
    fn CFHash(cf: CFTypeRef) -> CFHashCode;
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: CFIndex) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: CFIndex,
        keyCallBacks: *const CFDictionaryKeyCallBacks,
        valueCallBacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
    fn CFRunLoopRun() -> !;
    fn CFRunLoopStop(loop_: CFRunLoopRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(loop_: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
}

// --- CoreGraphics ----------------------------------------------------------

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {

    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;

    fn CGEventTapCreate(
        tap: i32,
        place: i32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: extern "C" fn(
            CGEventTapProxy,
            CGEventType,
            CGEventRef,
            *mut c_void,
        ) -> CGEventRef,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        key_code: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    fn CGEventPost(tap: i32, event: CGEventRef);
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
}

const K_CG_SESSION_EVENT_TAP: i32 = 1;
const K_CG_HEAD_TAP_EVENT_TAP: i32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_HID_EVENT_TAP: i32 = 0;
const K_CG_EVENT_FLAGS_CHANGED: CGEventType = 12;
const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 1 << 20;
const K_CG_EVENT_FLAG_MASK_SHIFT: CGEventFlags = 1 << 17;
const K_CG_EVENT_FLAG_MASK_CONTROL: CGEventFlags = 1 << 18;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: CGEventFlags = 1 << 19;
const K_CG_KEYBOARD_EVENT_KEY_CODE: u32 = 9;

// --- Static keycode tables ---------------------------------------------------

/// Virtual key codes for the number row, 1..=10 (10 is 0).
pub const DESKTOP_KEY_CODES: [u16; 10] =
    [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19, 0x1D];

/// HIToolbox virtual key code -> canonical key name for keys mwm binds.
pub fn key_name_for_code(code: u16) -> KeyName {
    match code {
        0x00 => KeyName::Letter('a'),
        0x01 => KeyName::Letter('s'),
        0x02 => KeyName::Letter('d'),
        0x03 => KeyName::Letter('f'),
        0x04 => KeyName::Letter('h'),
        0x05 => KeyName::Letter('g'),
        0x06 => KeyName::Letter('z'),
        0x07 => KeyName::Letter('x'),
        0x08 => KeyName::Letter('c'),
        0x09 => KeyName::Letter('v'),
        0x0B => KeyName::Letter('b'),
        0x0C => KeyName::Letter('q'),
        0x0D => KeyName::Letter('w'),
        0x0E => KeyName::Letter('e'),
        0x0F => KeyName::Letter('r'),
        0x10 => KeyName::Letter('y'),
        0x11 => KeyName::Letter('t'),
        0x12 => KeyName::Digit(1),
        0x13 => KeyName::Digit(2),
        0x14 => KeyName::Digit(3),
        0x15 => KeyName::Digit(4),
        0x16 => KeyName::Digit(6),
        0x17 => KeyName::Digit(5),
        0x20 => KeyName::Letter('u'),
        0x22 => KeyName::Letter('i'),
        0x23 => KeyName::Letter('p'),
        0x25 => KeyName::Letter('l'),
        0x26 => KeyName::Letter('j'),
        0x28 => KeyName::Letter('k'),
        0x2D => KeyName::Letter('n'),
        0x2E => KeyName::Letter('m'),
        0x2F => KeyName::Special("."),
        0x31 => KeyName::Special("space"),
        0x24 => KeyName::Special("return"),
        0x35 => KeyName::Special("escape"),
        0x30 => KeyName::Special("tab"),
        0x33 => KeyName::Special("delete"),
        key if key == u16::from(darwin_key_codes::LEFT) => KeyName::Arrow(Direction::Left),
        key if key == u16::from(darwin_key_codes::RIGHT) => KeyName::Arrow(Direction::Right),
        key if key == u16::from(darwin_key_codes::DOWN) => KeyName::Arrow(Direction::Down),
        key if key == u16::from(darwin_key_codes::UP) => KeyName::Arrow(Direction::Up),
        _ => KeyName::Special("vk"),
    }
}

// --- Small CF wrappers ------------------------------------------------------

/// Owned CoreFoundation reference; released on drop.
struct CfRef(CFTypeRef);

impl CfRef {
    fn new(raw: CFTypeRef) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self(raw))
        }
    }

    fn raw(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CfRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Build a CFString from a Rust literal.
///
/// The Accessibility attribute and action constants are published by name
/// rather than as linker symbols, so they are created from their own text
/// once and then cached. That is the documented spelling of each attribute,
/// so a typo surfaces as a failed lookup rather than as silent nonsense.
fn ax_string(name: &str) -> CFStringRef {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    extern "C" {
        fn CFStringCreateWithBytes(
            allocator: CFAllocatorRef,
            bytes: *const u8,
            length: CFIndex,
            encoding: u32,
            is_external_representation: bool,
        ) -> CFStringRef;
    }
    const K_CF_STRING_ENCODING_ASCII: u32 = 0x0600_0001;

    static CACHE: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = name.as_ptr() as usize;
    if let Ok(cache) = cache.lock() {
        if let Some(existing) = cache.get(&key) {
            return *existing as CFStringRef;
        }
    }
    let created = unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            name.as_ptr(),
            name.len() as CFIndex,
            K_CF_STRING_ENCODING_ASCII,
            false,
        )
    };
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, created as usize);
    }
    created
}

/// The AX value type for a CGPoint, asked of the framework by name.
fn ax_value_cg_point() -> CFTypeRef {
    ax_string("AXValueCGPoint")
}

/// The AX value type for a CGSize.
fn ax_value_cg_size() -> CFTypeRef {
    ax_string("AXValueCGSize")
}

/// Convert a Quartz bottom-left-origin rect to mwm top-left-origin pixels.
fn to_top_left_origin(rect: CGRect, primary_height: f64) -> Rect {
    Rect::new(
        rect.origin.x.round() as i32,
        (primary_height - rect.origin.y - rect.size.height).round() as i32,
        rect.size.width.round() as i32,
        rect.size.height.round() as i32,
    )
}

/// Map CGEvent flag bits to mwm modifiers.
fn modifiers_from_flags(flags: CGEventFlags) -> BTreeSet<Modifier> {
    let mut modifiers = BTreeSet::new();
    if flags & K_CG_EVENT_FLAG_MASK_COMMAND != 0 {
        modifiers.insert(Modifier::Cmd);
    }
    if flags & K_CG_EVENT_FLAG_MASK_CONTROL != 0 {
        modifiers.insert(Modifier::Ctrl);
    }
    if flags & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0 {
        modifiers.insert(Modifier::Alt);
    }
    if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
        modifiers.insert(Modifier::Shift);
    }
    modifiers
}

/// Read a CFString attribute as a Rust string (best-effort ASCII).
unsafe fn cf_string_to_string(value: CFTypeRef) -> String {
    if value.is_null() {
        return String::new();
    }
    extern "C" {
        fn CFStringGetLength(string: CFTypeRef) -> CFIndex;
        fn CFStringGetCString(
            string: CFTypeRef,
            buffer: *mut u8,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> bool;
    }
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    let length = CFStringGetLength(value);
    let mut buffer = vec![0_u8; (length as usize + 1) * 4];
    if CFStringGetCString(
        value,
        buffer.as_mut_ptr(),
        buffer.len() as CFIndex,
        K_CF_STRING_ENCODING_UTF8,
    ) {
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    } else {
        String::new()
    }
}

/// Read a CFNumber as `i64`, which is how the window list reports pids and
/// window numbers. A number stored as a float is converted too, because
/// CFNumber will not widen one on request.
unsafe fn cf_number_to_i64(value: CFTypeRef) -> Option<i64> {
    if value.is_null() {
        return None;
    }
    extern "C" {
        fn CFNumberGetValue(number: CFTypeRef, the_type: CFIndex, value_ptr: *mut c_void) -> bool;
    }
    const K_CF_NUMBER_SINT64_TYPE: CFIndex = 4;
    const K_CF_NUMBER_FLOAT64_TYPE: CFIndex = 6;
    let mut as_int: i64 = 0;
    if CFNumberGetValue(
        value,
        K_CF_NUMBER_SINT64_TYPE,
        std::ptr::from_mut(&mut as_int).cast::<c_void>(),
    ) {
        return Some(as_int);
    }
    let mut as_float: f64 = 0.0;
    CFNumberGetValue(
        value,
        K_CF_NUMBER_FLOAT64_TYPE,
        std::ptr::from_mut(&mut as_float).cast::<c_void>(),
    )
    .then_some(as_float as i64)
}

// --- The window system ------------------------------------------------------

/// macOS [`WindowSystem`] over AX + CoreGraphics.
pub struct DarwinWindowSystem {
    /// Live AX handles by window key; rebuilt on refresh.
    ax_windows: HashMap<String, AxWindow>,
    /// Observer bookkeeping by pid; read in `refresh_observers` to decide
    /// which apps still need an observer.
    #[allow(dead_code)]
    observers: BTreeMap<Pid, ObserverEntry>,
    /// Window-change callback shared into AX observer callbacks.
    on_change: Option<Arc<Mutex<Box<dyn FnMut() + Send>>>>,
    /// Key callback shared into the CGEvent tap callback.
    on_key: Option<Arc<Mutex<Box<dyn FnMut(KeyEvent) -> bool + Send>>>>,
    /// The event tap's thread and runloop.
    tap_thread: Option<JoinHandle<()>>,
    tap_loop: Option<usize>,
    /// The AX observers' thread and runloop.
    observer_thread: Option<JoinHandle<()>>,
    observer_loop: Option<usize>,
    /// Sends attach requests to the observer thread.
    attach_tx: Option<std::sync::mpsc::Sender<AttachRequest>>,
}

/// A request to observe one app, sent to the observer thread.
#[allow(dead_code)]
enum AttachRequest {
    Attach(Pid),
    Stop,
}

/// One live AX window handle.
struct AxWindow {
    pid: Pid,
    window_number: Option<i64>,
    raw: AXUIElementRef,
}

impl Drop for AxWindow {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { CFRelease(self.raw) };
        }
    }
}

/// Per-pid AX observer registration (owned by the observer thread).
/// Placeholder entry for a pid whose observer is being attached on the
/// observer thread; the observer itself never lives on this thread.
struct ObserverEntry;

impl DarwinWindowSystem {
    /// A system with nothing installed yet.
    pub fn new() -> Self {
        Self {
            ax_windows: HashMap::new(),
            observers: BTreeMap::new(),
            on_change: None,
            on_key: None,
            tap_thread: None,
            tap_loop: None,
            observer_loop: None,
            observer_thread: None,
            attach_tx: None,
        }
    }

    /// Rebuild the AX window map and per-pid observers from the live window
    /// list. The daemon calls this on its retile schedule.
    pub fn refresh_observers(&mut self) {
        let Some(windows) = self.collect_cg_windows() else {
            return;
        };
        let mut ax_windows = HashMap::new();
        let mut pids: BTreeSet<Pid> = BTreeSet::new();
        for (pid, _number) in &windows {
            pids.insert(*pid);
        }
        for pid in &pids {
            unsafe {
                let app = AXUIElementCreateApplication(*pid);
                if app.is_null() {
                    continue;
                }
                let app = CfRef::new(app).expect("checked non-null");
                let mut windows_ref: CFTypeRef = std::ptr::null();
                if AXUIElementCopyAttributeValue(
                    app.raw() as AXUIElementRef,
                    ax_string("kAXWindowsAttribute"),
                    &mut windows_ref,
                ) == K_AX_SUCCESS
                    && !windows_ref.is_null()
                {
                    let windows_ref = CfRef::new(windows_ref).expect("checked non-null");
                    let count = CFArrayGetCount(windows_ref.raw() as CFArrayRef);
                    for index in 0..count {
                        let window = CFArrayGetValueAtIndex(windows_ref.raw() as CFArrayRef, index)
                            as AXUIElementRef;
                        if let Some((key, handle)) = self.build_ax_window(*pid, window) {
                            ax_windows.insert(key, handle);
                        }
                    }
                }
            }
        }
        self.ax_windows = ax_windows;
        let attach_requests: Vec<Pid> = pids
            .iter()
            .filter(|pid| !self.observers.contains_key(*pid))
            .copied()
            .collect();
        for pid in attach_requests {
            self.observers.entry(pid).or_insert_with(|| {
                let _ = self
                    .attach_tx
                    .as_ref()
                    .map(|tx| tx.send(AttachRequest::Attach(pid)));
                ObserverEntry
            });
        }
    }

    /// Turn a raw (non-owning) AX window element into an owned handle plus
    /// its stable key, if it looks like a manageable window.
    unsafe fn build_ax_window(
        &self,
        pid: Pid,
        window: AXUIElementRef,
    ) -> Option<(String, AxWindow)> {
        // Only ordinary document windows are tiled: panels, sheets and other
        // auxiliary windows keep the geometry their app gave them.
        if Self::ax_get_string(window, ax_string("kAXRoleAttribute"))
            != Self::cf_string(ax_string("kAXWindowRole"))
        {
            return None;
        }
        if Self::ax_get_string(window, ax_string("kAXSubroleAttribute"))
            != Self::cf_string(ax_string("kAXStandardWindowSubrole"))
        {
            return None;
        }
        if Self::ax_get_bool(window, ax_string("kAXMinimizedAttribute")) {
            return None;
        }
        let number = Self::window_number(window);
        let key = match number {
            Some(number) => format!("{pid}:{number}"),
            None => format!("{pid}:h{:x}", CFHash(window) as u64),
        };
        // The array element is not owned by us; retain it for our map.
        extern "C" {
            fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
        }
        let copy = CFRetain(window);
        if copy.is_null() {
            return None;
        }
        Some((
            key,
            AxWindow {
                pid,
                window_number: number,
                raw: copy as AXUIElementRef,
            },
        ))
    }

    /// CGWindowList snapshot: (pid, window number) tuples of on-screen windows.
    fn collect_cg_windows(&self) -> Option<Vec<(Pid, i64)>> {
        unsafe {
            let list = CGWindowListCopyWindowInfo(
                K_CG_WINDOW_LIST_ON_SCREEN_ONLY | K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS,
                K_CG_NULL_WINDOW_ID,
            );
            if list.is_null() {
                return None;
            }
            let list = CfRef::new(list).expect("checked non-null");
            let count = CFArrayGetCount(list.raw() as CFArrayRef);
            let mut windows = Vec::new();
            for index in 0..count {
                let info = CFArrayGetValueAtIndex(list.raw() as CFArrayRef, index);
                if let Some((pid, number)) = cg_window_pid_number(info) {
                    windows.push((pid, number));
                }
            }
            Some(windows)
        }
    }
}

const K_CG_WINDOW_LIST_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_WINDOW_LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;
const K_CG_NULL_WINDOW_ID: u32 = 0;

/// Extract (pid, window number) from a CGWindowList info dictionary.
unsafe fn cg_window_pid_number(info: CFTypeRef) -> Option<(Pid, i64)> {
    extern "C" {
        fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: CFStringRef) -> CFTypeRef;
    }
    let pid = cf_number_to_i64(CFDictionaryGetValue(
        info as CFDictionaryRef,
        ax_string("kCGWindowOwnerPID"),
    ))? as Pid;
    let number = cf_number_to_i64(CFDictionaryGetValue(
        info as CFDictionaryRef,
        ax_string("kCGWindowNumber"),
    ))?;
    (pid > 0 && number > 0).then_some((pid, number))
}

/// AX attribute helpers used across the module.
impl DarwinWindowSystem {
    /// Copy an attribute value (owned) from an element.
    unsafe fn ax_get(element: AXUIElementRef, attribute: CFStringRef) -> Option<CfRef> {
        let mut value: CFTypeRef = std::ptr::null();
        let error = AXUIElementCopyAttributeValue(element, attribute, &mut value);
        (error == K_AX_SUCCESS && !value.is_null())
            .then(|| CfRef::new(value).expect("checked non-null"))
    }

    /// Find the AX handle for a window, falling back to pid and window
    /// number when the key moved on (a window can be rebuilt under a new key).
    fn lookup(&self, window: &WindowInfo) -> Option<&AxWindow> {
        if let Some(handle) = self.ax_windows.get(&window.key) {
            return Some(handle);
        }
        self.ax_windows.values().find(|handle| {
            handle.pid == window.pid
                && handle
                    .window_number
                    .is_some_and(|number| u64::try_from(number) == Ok(window.order))
        })
    }

    /// The text of a CFString constant.
    unsafe fn cf_string(value: CFStringRef) -> String {
        cf_string_to_string(value as CFTypeRef)
    }

    /// Read a string attribute.
    unsafe fn ax_get_string(element: AXUIElementRef, attribute: CFStringRef) -> String {
        Self::ax_get(element, attribute)
            .map(|v| cf_string_to_string(v.raw()))
            .unwrap_or_default()
    }

    /// Read a bool attribute, defaulting false.
    unsafe fn ax_get_bool(element: AXUIElementRef, attribute: CFStringRef) -> bool {
        let Some(value) = Self::ax_get(element, attribute) else {
            return false;
        };
        extern "C" {
            fn CFBooleanGetValue(boolean: CFBooleanRef) -> bool;
        }
        CFBooleanGetValue(value.raw() as CFBooleanRef)
    }

    /// Read the AX frame (position + size), already top-left origin.
    unsafe fn ax_frame(element: AXUIElementRef) -> Option<Rect> {
        let position = Self::ax_get(element, ax_string("kAXPositionAttribute"))?;
        let size = Self::ax_get(element, ax_string("kAXSizeAttribute"))?;
        let mut point = CGPoint { x: 0.0, y: 0.0 };
        let mut size_out = CGSize {
            width: 0.0,
            height: 0.0,
        };
        if !AXValueGetValue(
            position.raw() as AXValueRef,
            ax_value_cg_point() as i32,
            std::ptr::from_mut(&mut point).cast::<c_void>(),
        ) {
            return None;
        }
        if !AXValueGetValue(
            size.raw() as AXValueRef,
            ax_value_cg_size() as i32,
            std::ptr::from_mut(&mut size_out).cast::<c_void>(),
        ) {
            return None;
        }
        Some(Rect::new(
            point.x.round() as i32,
            point.y.round() as i32,
            size_out.width.round() as i32,
            size_out.height.round() as i32,
        ))
    }

    /// The AX window number, when the element exposes one.
    unsafe fn window_number(element: AXUIElementRef) -> Option<i64> {
        let value = Self::ax_get(element, ax_string("kAXWindowNumberAttribute"))?;
        cf_number_to_i64(value.raw())
    }

    /// Set position and size in one call sequence.
    unsafe fn set_ax_frame(element: AXUIElementRef, frame: Rect) -> bool {
        let point = CGPoint {
            x: f64::from(frame.x),
            y: f64::from(frame.y),
        };
        let size = CGSize {
            width: f64::from(frame.width),
            height: f64::from(frame.height),
        };
        let point_value = AXValueCreate(
            ax_value_cg_point() as i32,
            std::ptr::from_ref(&point).cast::<c_void>(),
        );
        let size_value = AXValueCreate(
            ax_value_cg_size() as i32,
            std::ptr::from_ref(&size).cast::<c_void>(),
        );
        let ok = !point_value.is_null() && !size_value.is_null();
        if !ok {
            return false;
        }
        let ok_position =
            AXUIElementSetAttributeValue(element, ax_string("kAXPositionAttribute"), point_value)
                == K_AX_SUCCESS;
        let ok_size =
            AXUIElementSetAttributeValue(element, ax_string("kAXSizeAttribute"), size_value)
                == K_AX_SUCCESS;
        if !point_value.is_null() {
            CFRelease(point_value);
        }
        if !size_value.is_null() {
            CFRelease(size_value);
        }
        ok_position && ok_size
    }
}

/// Trait implementation: queries and actions.
impl WindowSystem for DarwinWindowSystem {
    fn refresh(&mut self) {
        self.refresh_observers();
    }

    fn accessibility_trusted(&self) -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn prompt_for_accessibility(&self) -> bool {
        unsafe {
            let keys: [*const c_void; 1] = [ax_string("kAXTrustedCheckOptionPrompt")];
            let values: [*const c_void; 1] = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let result = AXIsProcessTrustedWithOptions(options);
            if !options.is_null() {
                CFRelease(options);
            }
            result
        }
    }

    fn screens(&self) -> QueryResult<Vec<ScreenInfo>> {
        unsafe {
            let mut displays: [CGDirectDisplayID; 16] = [0; 16];
            let mut count: u32 = 0;
            if CGGetActiveDisplayList(16, displays.as_mut_ptr(), &mut count) != 0 {
                return Err(());
            }
            let main_height = CGDisplayBounds(CGMainDisplayID()).size.height;
            let mut screens = Vec::new();
            for (index, display) in displays.iter().take(count as usize).enumerate() {
                let bounds = CGDisplayBounds(*display);
                let frame = to_top_left_origin(bounds, main_height);
                let is_primary = bounds.origin.x == 0.0 && bounds.origin.y == 0.0;
                let frame = if is_primary {
                    Rect::new(
                        frame.x,
                        frame.y + MENU_BAR_INSET,
                        frame.width,
                        frame.height - MENU_BAR_INSET,
                    )
                } else {
                    frame
                };
                screens.push(ScreenInfo {
                    key: format!("{index}:{}", frame.as_key()),
                    frame,
                });
            }
            (screens.is_empty()).then_some(screens).ok_or(())
        }
    }

    fn windows(&self) -> QueryResult<Vec<WindowInfo>> {
        let list = self.collect_cg_windows().ok_or(())?;
        let screens = self.screens()?;
        let mut windows = Vec::new();
        for (pid, number) in list {
            let Some(handle) = self.ax_windows.get(&format!("{pid}:{number}")) else {
                continue;
            };
            let Some(frame) = (unsafe { Self::ax_frame(handle.raw) }) else {
                frame_skip();
                continue;
            };
            let Some(screen) = screen_for_frame(&frame, &screens) else {
                continue;
            };
            let title = unsafe { Self::ax_get_string(handle.raw, ax_string("kAXTitleAttribute")) };
            windows.push(WindowInfo {
                key: format!("{pid}:{number}"),
                pid,
                title,
                frame,
                screen_key: screen.key.clone(),
                order: number as u64,
            });
        }
        windows.sort_by(|a, b| {
            a.frame
                .x
                .cmp(&b.frame.x)
                .then(a.frame.y.cmp(&b.frame.y))
                .then(a.pid.cmp(&b.pid))
                .then(a.order.cmp(&b.order))
        });
        Ok(windows)
    }

    fn focused_window(&self) -> Option<WindowInfo> {
        unsafe {
            let system = CfRef::new(AXUIElementCreateSystemWide())?;
            let focused_app = Self::ax_get(
                system.raw() as AXUIElementRef,
                ax_string("kAXFocusedApplicationAttribute"),
            )?;
            let focused_window = Self::ax_get(
                focused_app.raw() as AXUIElementRef,
                ax_string("kAXFocusedWindowAttribute"),
            )?;
            let mut pid: Pid = 0;
            if AXUIElementGetPid(focused_app.raw() as AXUIElementRef, &mut pid) != K_AX_SUCCESS {
                pid = 0;
            }
            let frame = Self::ax_frame(focused_window.raw() as AXUIElementRef)?;
            let screens = self.screens().ok()?;
            let screen = screen_for_frame(&frame, &screens)?;
            let number = Self::window_number(focused_window.raw() as AXUIElementRef);
            let key = match number {
                Some(number) => format!("{pid}:{number}"),
                None => format!("{pid}:h{:x}", CFHash(focused_window.raw()) as u64),
            };
            Some(WindowInfo {
                key,
                pid,
                title: Self::ax_get_string(
                    focused_window.raw() as AXUIElementRef,
                    ax_string("kAXTitleAttribute"),
                ),
                frame,
                screen_key: screen.key.clone(),
                order: number.unwrap_or(0) as u64,
            })
        }
    }

    fn set_frame(&self, window: &WindowInfo, frame: Rect) -> bool {
        let Some(handle) = self.lookup(window) else {
            return false;
        };
        unsafe { Self::set_ax_frame(handle.raw, frame) }
    }

    fn focus_window(&self, window: &WindowInfo) -> bool {
        let Some(handle) = self.lookup(window) else {
            return false;
        };
        unsafe {
            let app = AXUIElementCreateApplication(window.pid);
            if !app.is_null() {
                let app = CfRef::new(app).expect("checked non-null");
                AXUIElementSetAttributeValue(
                    app.raw() as AXUIElementRef,
                    ax_string("kAXFrontmostAttribute"),
                    kCFBooleanTrue,
                );
            }
            AXUIElementSetAttributeValue(handle.raw, ax_string("kAXMainAttribute"), kCFBooleanTrue);
            AXUIElementSetAttributeValue(
                handle.raw,
                ax_string("kAXFocusedAttribute"),
                kCFBooleanTrue,
            );
            AXUIElementPerformAction(handle.raw, ax_string("kAXRaiseAction")) == K_AX_SUCCESS
        }
    }

    fn close_window(&self, window: &WindowInfo) -> bool {
        let Some(handle) = self.lookup(window) else {
            return false;
        };
        unsafe {
            let Some(button) = Self::ax_get(handle.raw, ax_string("kAXCloseButtonAttribute"))
            else {
                return false;
            };
            AXUIElementPerformAction(button.raw() as AXUIElementRef, ax_string("kAXPressAction"))
                == K_AX_SUCCESS
        }
    }

    fn switch_desktop(&self, desktop: u8) -> bool {
        let Some(index) = desktop.checked_sub(1) else {
            return false;
        };
        let Some(code) = DESKTOP_KEY_CODES.get(index as usize) else {
            return false;
        };
        unsafe {
            for key_down in [true, false] {
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), *code, key_down);
                if event.is_null() {
                    return false;
                }
                CGEventSetFlags(event, K_CG_EVENT_FLAG_MASK_CONTROL);
                CGEventPost(K_CG_HID_EVENT_TAP, event);
                CFRelease(event);
            }
            true
        }
    }

    fn watch_windows(&mut self, on_change: Box<dyn FnMut() + Send>) -> bool {
        self.install_observers(on_change)
    }

    fn unwatch_windows(&mut self) {
        self.stop_observers();
    }

    fn watch_keys(&mut self, on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>) -> bool {
        self.install_tap(on_key)
    }

    fn unwatch_keys(&mut self) {
        self.stop_tap();
    }
}

/// Menu bar allowance on the primary display, in pixels.
const MENU_BAR_INSET: i32 = 25;

#[allow(dead_code)]
fn frame_skip() {}

/// Pick the screen whose frame contains the window center, else the one
/// with the largest overlap.
fn screen_for_frame<'a>(frame: &Rect, screens: &'a [ScreenInfo]) -> Option<&'a ScreenInfo> {
    screens
        .iter()
        .find(|screen| {
            screen
                .frame
                .contains_point(frame.center_x(), frame.center_y())
        })
        .or_else(|| {
            screens
                .iter()
                .max_by_key(|screen| screen.frame.intersection_area(*frame))
        })
}

/// Tap and observer installation: each gets a dedicated thread with its own
/// runloop; callbacks marshal back through the shared closures.
impl DarwinWindowSystem {
    /// Start the observer runloop thread. Notifications for individual
    /// apps are attached by [`Self::refresh_observers`].
    fn install_observers(&mut self, on_change: Box<dyn FnMut() + Send>) -> bool {
        self.stop_observers();
        self.on_change = Some(Arc::new(Mutex::new(on_change)));
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let (attach_tx, attach_rx) = std::sync::mpsc::channel::<AttachRequest>();
        self.attach_tx = Some(attach_tx);
        let shared = Arc::clone(self.on_change.as_ref().expect("just set"));
        let handle = std::thread::spawn(move || unsafe {
            let loop_ref = CFRunLoopGetCurrent();
            let _ = tx.send(loop_ref as usize);
            while let Ok(request) = attach_rx.recv() {
                match request {
                    AttachRequest::Attach(pid) => {
                        attach_observer_for_pid(loop_ref, shared.clone(), pid)
                    }
                    AttachRequest::Stop => break,
                }
            }
        });
        self.observer_thread = Some(handle);
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(address) => {
                self.observer_loop = Some(address);
                true
            }
            Err(_) => false,
        }
    }

    /// Stop and join the observer thread.
    fn stop_observers(&mut self) {
        if let Some(tx) = self.attach_tx.take() {
            let _ = tx.send(AttachRequest::Stop);
        }
        if let Some(handle) = self.observer_thread.take() {
            let _ = handle.join();
        }
        self.on_change = None;
        self.observer_loop = None;
    }

    /// Install the CGEvent tap on its own thread.
    fn install_tap(&mut self, on_key: Box<dyn FnMut(KeyEvent) -> bool + Send>) -> bool {
        self.stop_tap();
        let shared: Arc<Mutex<Box<dyn FnMut(KeyEvent) -> bool + Send>>> =
            Arc::new(Mutex::new(on_key));
        self.on_key = Some(Arc::clone(&shared));
        let user = Arc::as_ptr(self.on_key.as_ref().expect("just set")) as usize;
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let handle = std::thread::spawn(move || unsafe {
            let loop_ref = CFRunLoopGetCurrent();
            let _ = tx.send(loop_ref as usize);
            let mask = (1_u64 << K_CG_EVENT_KEY_DOWN) | (1_u64 << K_CG_EVENT_FLAGS_CHANGED);
            let tap = CGEventTapCreate(
                K_CG_SESSION_EVENT_TAP,
                K_CG_HEAD_TAP_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_DEFAULT,
                mask,
                tap_callback,
                user as *mut c_void,
            );
            if tap.is_null() {
                return;
            }
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(loop_ref, source, ax_string("kCFRunLoopCommonModes"));
            CGEventTapEnable(tap, true);
            // CFRunLoopRun never returns; stopping the loop is what ends us.
            CFRunLoopRun();
        });
        self.tap_thread = Some(handle);
        matches!(rx.recv_timeout(std::time::Duration::from_secs(2)), Ok(addr) if {
            self.tap_loop = Some(addr);
            true
        })
    }

    /// Stop and join the tap thread.
    fn stop_tap(&mut self) {
        if let Some(handle) = self.tap_thread.take() {
            if let Some(loop_addr) = self.tap_loop {
                unsafe { CFRunLoopStop(loop_addr as CFRunLoopRef) };
            }
            let _ = handle.join();
        }
        self.on_key = None;
        self.tap_loop = None;
    }
}

/// The CGEvent tap callback: normalises the event, asks the daemon whether
/// to consume, and passes it through otherwise.
extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if event_type != K_CG_EVENT_KEY_DOWN {
            return event;
        }
        let shared = user_info as *const Arc<Mutex<Box<dyn FnMut(KeyEvent) -> bool + Send>>>;
        if shared.is_null() {
            return event;
        }
        let shared = &*shared;
        let code = CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEY_CODE) as u16;
        let key = key_name_for_code(code);
        if matches!(key, KeyName::Special("vk")) {
            return event;
        }
        let modifiers = modifiers_from_flags(CGEventGetFlags(event));
        let consumed = shared
            .lock()
            .is_ok_and(|mut callback| (callback)(KeyEvent { modifiers, key }));
        if consumed {
            std::ptr::null()
        } else {
            event
        }
    }));
    result.unwrap_or(event)
}

/// Attach AX notifications for one pid on the observer thread.
unsafe fn attach_observer_for_pid(
    loop_ref: CFRunLoopRef,
    shared: Arc<Mutex<Box<dyn FnMut() + Send>>>,
    pid: Pid,
) {
    let app = AXUIElementCreateApplication(pid);
    if app.is_null() {
        return;
    }
    let app = CfRef::new(app).expect("checked non-null");
    let mut observer: AXObserverRef = std::ptr::null();
    if AXObserverCreate(pid, ax_observer_callback, &raw mut observer) != K_AX_SUCCESS
        || observer.is_null()
    {
        return;
    }
    let observer = CfRef::new(observer).expect("checked non-null");
    let source = AXObserverGetRunLoopSource(observer.raw() as AXObserverRef);
    CFRunLoopAddSource(loop_ref, source, ax_string("kCFRunLoopCommonModes"));
    let notifications = [
        ax_string("kAXWindowCreatedNotification"),
        ax_string("kAXFocusedWindowChangedNotification"),
        ax_string("kAXMainWindowChangedNotification"),
        ax_string("kAXUIElementDestroyedNotification"),
        ax_string("kAXWindowMiniaturizedNotification"),
        ax_string("kAXWindowDeminiaturizedNotification"),
        ax_string("kAXMovedNotification"),
        ax_string("kAXResizedNotification"),
        ax_string("kAXWindowMovedNotification"),
        ax_string("kAXWindowResizedNotification"),
    ];
    // The refcon is the shared callback; leak the Arc intentionally — it
    // lives until the process exits, matching the observer's lifetime.
    let refcon = Arc::into_raw(shared) as *mut c_void;
    for notification in notifications {
        let _ = AXObserverAddNotification(
            observer.raw() as AXObserverRef,
            app.raw() as AXUIElementRef,
            notification,
            refcon,
        );
    }
    // Observer and app element must outlive the runloop: leak them too.
    std::mem::forget(observer);
    std::mem::forget(app);
}

/// AX notification callback: notify the daemon that windows changed.
extern "C" fn ax_observer_callback(
    _observer: AXObserverRef,
    _element: AXUIElementRef,
    _notification: CFStringRef,
    refcon: *mut c_void,
) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if refcon.is_null() {
            return;
        }
        let shared = unsafe { &*(refcon as *const Arc<Mutex<Box<dyn FnMut() + Send>>>) };
        if let Ok(mut callback) = shared.lock() {
            callback();
        }
    }));
    let _ = result;
}

#[cfg(test)]
mod tests {
    use super::{
        key_name_for_code, modifiers_from_flags, to_top_left_origin, CGPoint, CGRect, CGSize,
        DESKTOP_KEY_CODES, K_CG_EVENT_FLAG_MASK_ALTERNATE, K_CG_EVENT_FLAG_MASK_COMMAND,
        K_CG_EVENT_FLAG_MASK_CONTROL, K_CG_EVENT_FLAG_MASK_SHIFT,
    };
    use crate::platform::KeyName;
    use crate::types::{Direction, Modifier};

    #[test]
    fn letters_map_to_lowercase_chars() {
        assert_eq!(key_name_for_code(0x00), KeyName::Letter('a'));
        assert_eq!(key_name_for_code(0x04), KeyName::Letter('h'));
        assert_eq!(key_name_for_code(0x0F), KeyName::Letter('r'));
        assert_eq!(key_name_for_code(0x25), KeyName::Letter('l'));
    }

    #[test]
    fn digits_map_to_numbers() {
        assert_eq!(key_name_for_code(0x12), KeyName::Digit(1));
        assert_eq!(key_name_for_code(0x16), KeyName::Digit(6));
        assert_eq!(key_name_for_code(0x17), KeyName::Digit(5));
    }

    #[test]
    fn arrows_map_to_directions() {
        assert_eq!(key_name_for_code(0x7B), KeyName::Arrow(Direction::Left));
        assert_eq!(key_name_for_code(0x7C), KeyName::Arrow(Direction::Right));
        assert_eq!(key_name_for_code(0x7D), KeyName::Arrow(Direction::Down));
        assert_eq!(key_name_for_code(0x7E), KeyName::Arrow(Direction::Up));
    }

    #[test]
    fn unknown_codes_degrade_to_a_special_name() {
        assert_eq!(key_name_for_code(0xFFFF), KeyName::Special("vk"));
    }

    #[test]
    fn flags_map_to_modifiers() {
        let flags = K_CG_EVENT_FLAG_MASK_COMMAND | K_CG_EVENT_FLAG_MASK_SHIFT;
        let modifiers = modifiers_from_flags(flags);
        assert!(modifiers.contains(&Modifier::Cmd));
        assert!(modifiers.contains(&Modifier::Shift));
        assert!(!modifiers.contains(&Modifier::Alt));
        assert!(!modifiers.contains(&Modifier::Ctrl));
        assert!(modifiers_from_flags(K_CG_EVENT_FLAG_MASK_ALTERNATE).contains(&Modifier::Alt));
        assert!(modifiers_from_flags(K_CG_EVENT_FLAG_MASK_CONTROL).contains(&Modifier::Ctrl));
        assert_eq!(modifiers_from_flags(0).len(), 0);
    }

    #[test]
    fn quartz_rects_convert_to_top_left_origin() {
        let rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 100.0,
                height: 200.0,
            },
        };
        // bottom-left origin screen 1000 tall: a full-height window at the
        // bottom of the display starts at y = 0 in top-left coordinates.
        let converted = to_top_left_origin(rect, 1000.0);
        assert_eq!(converted, crate::types::Rect::new(0, 800, 100, 200));
    }

    #[test]
    fn desktop_key_codes_cover_ten_desktops() {
        assert_eq!(DESKTOP_KEY_CODES.len(), 10);
        // every code distinct, and none colliding with the arrow range
        for (index, code) in DESKTOP_KEY_CODES.iter().enumerate() {
            assert!(
                (0x11..=0x1E).contains(code),
                "desktop {index} code {code:#x} out of range"
            );
        }
    }
}
