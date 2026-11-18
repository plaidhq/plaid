//! Clipboard paste with restore (ClipboardPaste in Paste.swift). Main thread only.
//! The previous clipboard comes back 0.5 s after ⌘V, but only while the pasteboard still holds this write:
//! a newer copy by the user wins, and an overlapping paste restores the first one's snapshot first.
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

pub const RESTORE_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

/// Every item and type as plain data.
type Snapshot = Vec<Vec<(String, Vec<u8>)>>;

pub struct Clipboard {
    board: Retained<NSPasteboard>,
    /// Snapshot and the change count this write owns.
    pending: Option<(Snapshot, isize)>,
    post: fn(),
}

impl Default for Clipboard {
    fn default() -> Self {
        Clipboard {
            board: NSPasteboard::generalPasteboard(),
            pending: None,
            post: post_cmd_v,
        }
    }
}

impl Clipboard {
    /// Writes `text`, posts ⌘V and returns the change count to hand back to `restore` after RESTORE_AFTER.
    /// Always Some on macOS (the Windows clipboard can be busy).
    pub fn paste(&mut self, text: &str) -> Option<isize> {
        self.restore(None);
        let b = &self.board;
        let snap: Snapshot = b
            .pasteboardItems()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        item.types()
                            .iter()
                            .filter_map(|t| {
                                item.dataForType(&t).map(|d| (t.to_string(), d.to_vec()))
                            })
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default();
        b.clearContents();
        b.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
        let owned = b.changeCount();
        self.pending = Some((snap, owned));
        (self.post)();
        Some(owned)
    }

    /// `only`: restore just the write with this change count (the timer of an older paste is a no-op).
    pub fn restore(&mut self, only: Option<isize>) {
        if only.is_some_and(|c| self.pending.as_ref().is_none_or(|p| p.1 != c)) {
            return;
        }
        let Some((snap, owned)) = self.pending.take() else {
            return;
        };
        let b = &self.board;
        if b.changeCount() != owned {
            return;
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
    }
}

/// ⌘V to the frontmost app. Needs Accessibility.
fn post_cmd_v() {
    let Ok(src) = CGEventSource::new(CGEventSourceStateID::CombinedSessionState) else {
        return;
    };
    for down in [true, false] {
        if let Ok(e) = CGEvent::new_keyboard_event(src.clone(), 9, down) {
            e.set_flags(CGEventFlags::CGEventFlagCommand);
            e.post(CGEventTapLocation::HID);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Private pasteboard and a no-op key poster: never touches the user's clipboard or keyboard.
    #[test]
    fn restores_unless_user_copied() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let custom = NSString::from_str("dev.neo.plaid.test.custom");
        let item = NSPasteboardItem::new();
        item.setString_forType(&NSString::from_str("original"), unsafe {
            NSPasteboardTypeString
        });
        item.setData_forType(&NSData::with_bytes(&[1, 2, 3]), &custom);
        board.clearContents();
        board.writeObjects(&NSArray::from_retained_slice(&[
            ProtocolObject::from_retained(item),
        ]));
        let mut c = Clipboard {
            board: board.clone(),
            pending: None,
            post: || {},
        };
        let s = |b: &NSPasteboard| {
            b.stringForType(unsafe { NSPasteboardTypeString })
                .map(|s| s.to_string())
        };

        let first = c.paste("一").unwrap();
        assert_eq!(s(&board).as_deref(), Some("一"));
        // Overlapping paste: the first snapshot is restored before the second write, so the original survives.
        let second = c.paste("二").unwrap();
        c.restore(Some(first));
        assert_eq!(
            s(&board).as_deref(),
            Some("二"),
            "stale timer must not restore"
        );
        c.restore(Some(second));
        assert_eq!(s(&board).as_deref(), Some("original"));
        assert_eq!(
            board.dataForType(&custom).map(|d| d.to_vec()),
            Some(vec![1, 2, 3])
        );

        let owned = c.paste("dictated").unwrap();
        board.clearContents();
        board.setString_forType(&NSString::from_str("user copy"), unsafe {
            NSPasteboardTypeString
        });
        c.restore(Some(owned));
        assert_eq!(s(&board).as_deref(), Some("user copy"));
        let _: () = unsafe { objc2::msg_send![&*board, releaseGlobally] };
    }
}
