//! macOS: frontmost app, focused-field reads and paste-target checks over the Accessibility API
//! (ports of Field.focused/read and PasteTarget in the Swift app), plus the event clock.
use core_foundation::base::{CFType, CFTypeID, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::NSWorkspace;
use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::Mutex;

type AXUIElementRef = CFTypeRef;

#[repr(C)]
#[derive(Default)]
struct CFRange {
    location: isize,
    length: isize,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementSetMessagingTimeout(el: AXUIElementRef, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        el: AXUIElementRef,
        attr: CFStringRef,
        value: *mut CFTypeRef,
    ) -> i32;
    fn AXUIElementSetAttributeValue(el: AXUIElementRef, attr: CFStringRef, value: CFTypeRef)
    -> i32;
    fn AXUIElementGetTypeID() -> CFTypeID;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: core_foundation::dictionary::CFDictionaryRef)
    -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

const AX_VALUE_CF_RANGE: u32 = 4;
/// Longer values (huge documents) are skipped: copying them over AX is slow.
const MAX_LENGTH: usize = 200_000;

/// Non-prompting Accessibility check (key tap and posting ⌘V).
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Accessibility check that shows the system prompt when missing. Only on a normal (user-started) launch.
pub fn request_trust() -> bool {
    use core_foundation::dictionary::CFDictionary;
    unsafe {
        let opts = CFDictionary::from_CFType_pairs(&[(
            CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt),
            CFBoolean::true_value(),
        )]);
        AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef())
    }
}

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyProxies(
        store: *const c_void,
    ) -> core_foundation::dictionary::CFDictionaryRef;
}

/// HTTP(S) proxies from Network settings (what URLSession used). PAC files and SOCKS-only setups are not followed.
pub fn proxy() -> Option<plaid_core::llm::SystemProxy> {
    use core_foundation::array::CFArray;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    let raw = unsafe { SCDynamicStoreCopyProxies(std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    let d: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(raw) };
    let get = |k: &str| d.find(CFString::new(k)).map(|v| v.clone());
    let num = |k: &str| {
        get(k)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(0)
    };
    let server = |p: &str| {
        let host = get(&format!("{p}Proxy"))
            .and_then(|v| v.downcast::<CFString>())
            .map(|s| s.to_string())
            .filter(|h| !h.is_empty())?;
        (num(&format!("{p}Enable")) == 1).then(|| format!("{host}:{}", num(&format!("{p}Port"))))
    };
    let mut bypass: Vec<String> = get("ExceptionsList")
        .and_then(|v| v.downcast::<CFArray>())
        .map(|a| {
            a.iter()
                .filter_map(|v| unsafe { CFType::wrap_under_get_rule(*v) }.downcast::<CFString>())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    if num("ExcludeSimpleHostnames") == 1 {
        bypass.push("<local>".into());
    }
    Some(plaid_core::llm::SystemProxy {
        http: server("HTTP"),
        https: server("HTTPS"),
        bypass,
    })
}

/// Name, bundle id and pid of the frontmost app.
pub fn front() -> Option<(String, Option<String>, i32)> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let name = app
        .localizedName()
        .map(|n| n.to_string())
        .unwrap_or_default();
    Some((
        name,
        app.bundleIdentifier().map(|b| b.to_string()),
        app.processIdentifier(),
    ))
}

/// An AX element. AX calls are thread-safe; elements only cross to the AX worker thread.
#[derive(Clone)]
pub struct Element(CFType);
unsafe impl Send for Element {}

impl PartialEq for Element {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0
    }
}

impl Element {
    fn app(pid: i32, timeout: f32) -> Option<Element> {
        let r = unsafe { AXUIElementCreateApplication(pid) };
        if r.is_null() {
            return None;
        }
        unsafe { AXUIElementSetMessagingTimeout(r, timeout) };
        Some(Element(unsafe { CFType::wrap_under_create_rule(r) }))
    }

    fn attr(&self, name: &str) -> Option<CFType> {
        let mut v: CFTypeRef = std::ptr::null();
        let ok = unsafe {
            AXUIElementCopyAttributeValue(
                self.0.as_CFTypeRef(),
                CFString::new(name).as_concrete_TypeRef(),
                &mut v,
            )
        } == 0;
        (ok && !v.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(v) })
    }

    fn element(&self, name: &str, timeout: f32) -> Option<Element> {
        let v = self.attr(name)?;
        if v.type_of() != unsafe { AXUIElementGetTypeID() } {
            return None;
        }
        unsafe { AXUIElementSetMessagingTimeout(v.as_CFTypeRef(), timeout) };
        Some(Element(v))
    }

    fn string(&self, name: &str) -> Option<String> {
        self.attr(name)?
            .downcast::<CFString>()
            .map(|s| s.to_string())
    }
}

/// Where the utterance started: the frontmost app and, when AX can tell, its focused window and element.
pub struct Target {
    pub pid: i32,
    window: Option<Element>,
    element: Option<Element>,
    /// Focused window's title (refine's app context).
    pub title: Option<String>,
    /// The focused text field and its full value at key-down (edit learning's baseline).
    pub field: Option<Field>,
}

#[derive(Clone)]
pub struct Field {
    element: Element,
    pub value: String,
}

impl Field {
    /// Current value; None once the field is gone, secure or too long.
    pub fn reread(&self) -> Option<String> {
        value(&self.element)
    }
}

impl Target {
    /// Still focused where the utterance started. Missing window/element facts don't block the paste.
    pub fn matches(&self) -> bool {
        let Some(app) = Element::app(self.pid, 0.12) else {
            return false;
        };
        let same = |mine: &Option<Element>, name| {
            mine.as_ref()
                .is_none_or(|m| app.element(name, 0.12).is_some_and(|c| c == *m))
        };
        same(&self.window, "AXFocusedWindow") && same(&self.element, "AXFocusedUIElement")
    }
}

static MANUAL_AX: Mutex<Option<HashSet<i32>>> = Mutex::new(None);

/// Paste target and up to `n` UTF-16 units before the caret in the focused field of `pid`. AX worker thread.
pub fn focus(pid: i32, n: usize) -> (Target, Option<String>) {
    let mut t = Target {
        pid,
        window: None,
        element: None,
        title: None,
        field: None,
    };
    if pid == std::process::id() as i32 {
        return (t, None);
    }
    let Some(app) = Element::app(pid, 0.15) else {
        return (t, None);
    };
    // Electron apps only build their AX tree when asked.
    if MANUAL_AX
        .lock()
        .unwrap()
        .get_or_insert_default()
        .insert(pid)
    {
        unsafe {
            AXUIElementSetAttributeValue(
                app.0.as_CFTypeRef(),
                CFString::new("AXManualAccessibility").as_concrete_TypeRef(),
                CFBoolean::true_value().as_CFTypeRef(),
            )
        };
    }
    t.window = app.element("AXFocusedWindow", 0.15);
    t.title = t
        .window
        .as_ref()
        .and_then(|w| w.string("AXTitle"))
        .filter(|s| !s.is_empty());
    t.element = app.element("AXFocusedUIElement", 0.15);
    let Some(el) = t.element.clone() else {
        return (t, None);
    };
    let Some(v) = value(&el) else {
        return (t, None);
    };
    let before = before(&el, &v, n);
    t.field = Some(Field {
        element: el,
        value: v,
    });
    (t, Some(before))
}

fn value(el: &Element) -> Option<String> {
    if el.string("AXSubrole").as_deref() == Some("AXSecureTextField") {
        return None;
    }
    el.string("AXValue")
        .filter(|v| v.encode_utf16().count() <= MAX_LENGTH)
}

fn before(el: &Element, v: &str, n: usize) -> String {
    let u: Vec<u16> = v.encode_utf16().collect();
    let mut caret = None;
    if let Some(r) = el
        .attr("AXSelectedTextRange")
        .filter(|r| r.type_of() == unsafe { AXValueGetTypeID() })
    {
        let mut range = CFRange::default();
        if unsafe { AXValueGetValue(r.as_CFTypeRef(), AX_VALUE_CF_RANGE, (&raw mut range).cast()) }
        {
            caret = usize::try_from(range.location).ok();
        }
    }
    let end = caret.unwrap_or(u.len()).min(u.len());
    let s = String::from_utf16_lossy(&u[end.saturating_sub(n)..end]);
    plaid_core::text::trim_ws_nl(&s).to_owned()
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut [u32; 2]) -> i32;
}

fn timebase() -> (u64, u64) {
    let mut tb = [1u32, 1];
    unsafe { mach_timebase_info(&mut tb) };
    (tb[0] as u64, tb[1] as u64)
}

/// Seconds since boot on the mach clock (CLOCK_UPTIME_RAW); the gesture compares event times against it.
pub fn now() -> f64 {
    let (n, d) = timebase();
    (unsafe { mach_absolute_time() } as u128 * n as u128 / d as u128) as f64 / 1e9
}

/// CGEventGetTimestamp, in `now()` seconds. It is documented as nanoseconds but carries mach ticks on some
/// hardware; the reading closer to the current time wins (the units differ ~40× on Apple silicon). 0 → now.
pub fn event_time(ts: u64) -> f64 {
    let (n, d) = timebase();
    let ticks = unsafe { mach_absolute_time() };
    if ts == 0 {
        return now();
    }
    let now_ns = (ticks as u128 * n as u128 / d as u128) as u64;
    let ns = if ticks.abs_diff(ts) < now_ns.abs_diff(ts) {
        (ts as u128 * n as u128 / d as u128) as u64
    } else {
        ts
    };
    ns as f64 / 1e9
}

#[cfg(test)]
mod tests {
    //! Machine-dependent (a GUI session, Network settings): `cargo test -p plaid -- --ignored`.

    #[test]
    #[ignore]
    fn reads_front_app() {
        let (name, bundle, pid) = super::front().expect("frontmost app");
        eprintln!("{name} {bundle:?} {pid}");
        assert!(pid > 0 && bundle.is_some_and(|b| b.contains('.')));
    }

    /// Read-only: compare with `scutil --proxy`.
    #[test]
    #[ignore]
    fn reads_system_proxy() {
        let p = super::proxy().expect("proxy dictionary");
        eprintln!("{p:?}");
        if let Some(h) = &p.https {
            assert!(h.contains(':'), "{h}");
        }
    }
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: &'static objc2_foundation::NSString;
}

/// Microphone permission without prompting: Some(true) allowed, Some(false) denied or restricted, None not asked
/// yet (the first recording asks).
pub fn mic_allowed() -> Option<bool> {
    let cls = objc2::runtime::AnyClass::get(c"AVCaptureDevice")?;
    // SAFETY: class method taking an AVMediaType string and returning AVAuthorizationStatus (NSInteger).
    let status: isize =
        unsafe { objc2::msg_send![cls, authorizationStatusForMediaType: AVMediaTypeAudio] };
    match status {
        0 => None,
        3 => Some(true),
        _ => Some(false),
    }
}
