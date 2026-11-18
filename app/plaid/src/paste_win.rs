//! Clipboard paste with restore on Windows (counterpart of paste.rs, same contract). Not yet run on a real machine.
//! Every memory-backed format is snapshotted (GDI-handle formats such as CF_BITMAP are skipped; CF_DIB carries the
//! image), the text goes in marked out of clipboard history and cloud sync, Ctrl+V is sent, and the snapshot comes
//! back RESTORE_AFTER later unless the clipboard changed since (GetClipboardSequenceNumber).
use std::time::Duration;
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};

pub const RESTORE_AFTER: Duration = Duration::from_millis(500);

const CF_UNICODETEXT: u32 = 13;
const VK_CONTROL: u16 = 0x11;
const VK_V: u16 = 0x56;

/// (format, bytes) per format.
type Snapshot = Vec<(u32, Vec<u8>)>;

#[derive(Default)]
pub struct Clipboard {
    /// Snapshot and the sequence number this write owns.
    pending: Option<(Snapshot, u32)>,
}

/// Open clipboard; closed on drop. Another app may hold it for a moment, so opening retries briefly.
struct Open;

impl Open {
    fn new() -> Option<Open> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Open);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

/// Handles that are GDI objects or owner-drawn, not global memory.
fn memory_backed(f: u32) -> bool {
    !matches!(
        f,
        2 | 3 | 9 | 14 | 0x80 | 0x82 | 0x83 | 0x8E | 0x200..=0x3FF
    )
}

fn snapshot() -> Snapshot {
    let mut out = vec![];
    let mut f = 0;
    loop {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        if !memory_backed(f) {
            continue;
        }
        let h = unsafe { GetClipboardData(f) };
        if h.is_null() {
            continue;
        }
        let p = unsafe { GlobalLock(h) };
        if p.is_null() {
            continue;
        }
        let n = unsafe { GlobalSize(h) };
        out.push((
            f,
            unsafe { std::slice::from_raw_parts(p as *const u8, n) }.to_vec(),
        ));
        unsafe { GlobalUnlock(h) };
    }
    out
}

/// Sets one format; the clipboard owns the memory once SetClipboardData succeeds.
fn set(f: u32, data: &[u8]) -> bool {
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)) };
    if h.is_null() {
        return false;
    }
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        unsafe { GlobalFree(h) };
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), p.cast(), data.len()) };
    unsafe { GlobalUnlock(h) };
    if unsafe { SetClipboardData(f, h as HANDLE) }.is_null() {
        unsafe { GlobalFree(h) };
        return false;
    }
    true
}

fn put_back(snap: &Snapshot) {
    unsafe { EmptyClipboard() };
    for (f, data) in snap {
        set(*f, data);
    }
    mark_transient();
}

/// Keeps a write away from clipboard monitors, Win+V history and cloud clipboard (it is ours for half a second, or
/// a restore).
fn mark_transient() {
    for name in [
        "ExcludeClipboardContentFromMonitorProcessing",
        "CanIncludeInClipboardHistory",
        "CanUploadToCloudClipboard",
    ] {
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let f = unsafe { RegisterClipboardFormatW(wide.as_ptr()) };
        if f != 0 {
            set(f, &0u32.to_le_bytes());
        }
    }
}

impl Clipboard {
    /// Writes `text`, sends Ctrl+V and returns the sequence number to hand back to `restore` after RESTORE_AFTER.
    /// None: the clipboard could not be opened or written; nothing was pasted.
    pub fn paste(&mut self, text: &str) -> Option<u32> {
        self.restore(None);
        let open = Open::new()?;
        let snap = snapshot();
        unsafe { EmptyClipboard() };
        let utf16: Vec<u8> = text
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        if !set(CF_UNICODETEXT, &utf16) {
            put_back(&snap);
            return None;
        }
        mark_transient();
        drop(open);
        let owned = unsafe { GetClipboardSequenceNumber() };
        self.pending = Some((snap, owned));
        crate::win::send_keys(&[
            (VK_CONTROL, true),
            (VK_V, true),
            (VK_V, false),
            (VK_CONTROL, false),
        ]);
        Some(owned)
    }

    /// `only`: restore just the write with this sequence number (the timer of an older paste is a no-op).
    pub fn restore(&mut self, only: Option<u32>) {
        if only.is_some_and(|c| self.pending.as_ref().is_none_or(|p| p.1 != c)) {
            return;
        }
        let Some((snap, owned)) = self.pending.take() else {
            return;
        };
        if unsafe { GetClipboardSequenceNumber() } != owned {
            return;
        }
        if let Some(_open) = Open::new() {
            put_back(&snap);
        }
    }
}
