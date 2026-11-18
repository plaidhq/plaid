// Clipboard paste with restore, ported from ClipboardPaste in Paste.swift. Main thread only.
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

/// Plain data copy of every item and type, so it can cross threads while the restore timer runs.
pub type Snapshot = Vec<Vec<(String, Vec<u8>)>>;

pub fn snapshot(b: &NSPasteboard) -> Snapshot {
    let Some(items) = b.pasteboardItems() else { return vec![] };
    items
        .iter()
        .map(|item| item.types().iter().filter_map(|t| item.dataForType(&t).map(|d| (t.to_string(), d.to_vec()))).collect())
        .collect()
}

/// Returns the change count this write owns.
pub fn write_text(b: &NSPasteboard, text: &str) -> isize {
    b.clearContents();
    b.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    b.changeCount()
}

/// Restore only while the pasteboard still holds our write; a newer copy by the user wins.
pub fn restore(b: &NSPasteboard, snap: &Snapshot, owned: isize) -> bool {
    if b.changeCount() != owned {
        return false;
    }
    b.clearContents();
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = snap
        .iter()
        .map(|types| {
            let item = NSPasteboardItem::new();
            for (t, d) in types {
                item.setData_forType(&NSData::with_bytes(d), &NSString::from_str(t));
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    if !items.is_empty() {
        b.writeObjects(&NSArray::from_retained_slice(&items));
    }
    true
}

/// ⌘V to the frontmost app. Needs Accessibility.
pub fn post_cmd_v() {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else { return };
    for down in [true, false] {
        if let Ok(e) = CGEvent::new_keyboard_event(src.clone(), 9, down) {
            e.set_flags(CGEventFlags::CGEventFlagCommand);
            e.post(CGEventTapLocation::HID);
        }
    }
}

pub fn general() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}

/// Exercise snapshot/write/restore on a private pasteboard; never touches the general one or the keyboard.
pub fn self_test() -> Result<(), String> {
    let b = NSPasteboard::pasteboardWithUniqueName();
    let custom = "dev.neo.plaid.spike.custom";
    b.clearContents();
    let item = NSPasteboardItem::new();
    item.setString_forType(&NSString::from_str("original"), unsafe { NSPasteboardTypeString });
    item.setData_forType(&NSData::with_bytes(&[1, 2, 3]), &NSString::from_str(custom));
    b.writeObjects(&NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]));

    let snap = snapshot(&b);
    let owned = write_text(&b, "dictated 文本");
    let now = b.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string());
    if now.as_deref() != Some("dictated 文本") {
        return Err(format!("write: {now:?}"));
    }
    if !restore(&b, &snap, owned) {
        return Err("restore refused".into());
    }
    let s = b.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string());
    let d = b.dataForType(&NSString::from_str(custom)).map(|d| d.to_vec());
    if s.as_deref() != Some("original") || d.as_deref() != Some(&[1, 2, 3][..]) {
        return Err(format!("restored {s:?} {d:?}"));
    }

    // The user copies something after our write: restore must leave it alone.
    let owned = write_text(&b, "dictated");
    write_text(&b, "user copy");
    if restore(&b, &snap, owned) {
        return Err("restored over a newer copy".into());
    }
    let s = b.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string());
    let _: () = unsafe { objc2::msg_send![&*b, releaseGlobally] };
    if s.as_deref() != Some("user copy") { Err(format!("after user copy: {s:?}")) } else { Ok(()) }
}
