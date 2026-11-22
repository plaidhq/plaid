//! Plaid's own icons (under `plaid/`), drawn as monochrome masks and tinted by the element's text color.
//! Everything else falls through to the kit's assets.
use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const S: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">"##;

fn icon(name: &str) -> Option<String> {
    let body = match name {
        "overview" => r##"<path d="M3 13.5V8.5M8 13.5V3M13 13.5V6"/>"##,
        "history" => r##"<circle cx="8" cy="8" r="5.8"/><path d="M8 4.8V8l2.3 1.6"/>"##,
        "dictionary" => {
            r##"<path d="M3 3.2c1.8-.6 3.5-.4 5 .8 1.5-1.2 3.2-1.4 5-.8v9.6c-1.8-.6-3.5-.4-5 .8-1.5-1.2-3.2-1.4-5-.8z"/><path d="M8 4v9.6"/>"##
        }
        "refine" => {
            r##"<path d="M8 2.5l1.3 3.2 3.2 1.3-3.2 1.3L8 11.5 6.7 8.3 3.5 7l3.2-1.3z"/><path d="M12.5 11v3M11 12.5h3"/>"##
        }
        "commands" => {
            r##"<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M5 6.5L7 8 5 9.5M8.5 10h2.5"/>"##
        }
        "settings" => {
            r##"<circle cx="8" cy="8" r="2.3"/><path d="M8 1.8v2M8 12.2v2M1.8 8h2M12.2 8h2M3.6 3.6l1.4 1.4M11 11l1.4 1.4M3.6 12.4L5 11M11 5l1.4-1.4"/>"##
        }
        "perf" => r##"<path d="M2.5 11.5a5.5 5.5 0 0 1 11 0"/><path d="M8 11.5l2.6-3.4"/>"##,
        "updown" => r##"<path d="M5 6l3-3 3 3M5 10l3 3 3-3" stroke-width="1.7"/>"##,
        "check" => r##"<path d="M2.5 8.5l3.6 3.6L13.5 3" stroke-width="2.4"/>"##,
        "tri-right" => r##"<path d="M4 2l8 6-8 6z" fill="#000" stroke="none"/>"##,
        "pause" => r##"<path d="M4 3h3v10H4zM9 3h3v10H9z" fill="#000" stroke="none"/>"##,
        "tri-down" => r##"<path d="M2 4h12l-6 8z" fill="#000" stroke="none"/>"##,
        "up" => r##"<path d="M2 11L8 4l6 7z" fill="#000" stroke="none"/>"##,
        "down" => r##"<path d="M2 5l6 7 6-7z" fill="#000" stroke="none"/>"##,
        "plus" => r##"<path d="M8 2.5v11M2.5 8h11" stroke-width="2"/>"##,
        "minus" => r##"<path d="M2.5 8h11" stroke-width="2"/>"##,
        "xmark" => r##"<path d="M4 4l8 8M12 4l-8 8" stroke-width="1.8"/>"##,
        "accessibility" => {
            r##"<circle cx="8" cy="8" r="6.6"/><circle cx="8" cy="4.6" r="1" fill="#000" stroke="none"/><path d="M4.6 6.8l3.4.7 3.4-.7M8 7.5v2.5l-1.7 2.9M8 10l1.7 2.9"/>"##
        }
        "chev-down" => r##"<path d="M4 6l4 4 4-4" stroke-width="1.8"/>"##,
        "chev-right" => r##"<path d="M6 4l4 4-4 4" stroke-width="1.8"/>"##,
        "spoke" => r##"<path d="M8 1v4.4" stroke-width="1.8"/>"##,
        "exclaim" => {
            r##"<path d="M8 2.5v7" stroke-width="2.6"/><circle cx="8" cy="13" r="1.5" fill="#000" stroke="none"/>"##
        }
        "xcircle" => {
            r##"<circle cx="8" cy="8" r="7" fill="#000" stroke="none"/><path d="M5.5 5.5l5 5M10.5 5.5l-5 5" stroke="#fff" stroke-width="1.6"/>"##
        }
        "search" => {
            r##"<circle cx="7" cy="7" r="4.2"/><path d="M10.2 10.2l3.6 3.6" stroke-width="1.8"/>"##
        }
        "pencil" => {
            r##"<path d="M3 13l.7-3L11 2.7l2.3 2.3L6 12.3z"/><path d="M9.5 4.2l2.3 2.3"/>"##
        }
        "trash" => {
            r##"<path d="M2.8 4.2h10.4M6.2 4.2V2.6h3.6v1.6M4.2 4.2l.7 9.2h6.2l.7-9.2M6.7 6.5v4.8M9.3 6.5v4.8"/>"##
        }
        "folder" => r##"<path d="M2 4.2h4.2l1.4 1.5H14v7.5H2z"/>"##,
        "action" => {
            r##"<circle cx="3.5" cy="8" r="1.3" fill="#000" stroke="none"/><circle cx="8" cy="8" r="1.3" fill="#000" stroke="none"/><circle cx="12.5" cy="8" r="1.3" fill="#000" stroke="none"/>"##
        }
        "mic" => {
            r##"<rect x="5.6" y="1.8" width="4.8" height="8" rx="2.4" fill="#000" stroke="none"/><path d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.3M5.6 14.3h4.8"/>"##
        }
        "return" => r##"<path d="M13 3.5v4.2H4"/><path d="M6.6 5L4 7.7l2.6 2.7"/>"##,
        "flame" => {
            r##"<path d="M8 1.8c.4 2.4 3.9 3.8 3.9 7.4A3.9 3.9 0 0 1 4.1 9.2c0-1.6.8-2.6 1.6-3.3 0 1.3.7 2.1 1.4 2.3C7 5.9 7.1 3.6 8 1.8z" fill="#000" stroke="none"/>"##
        }
        "cloud" => {
            r##"<path d="M4.5 12.5h7.2a2.8 2.8 0 0 0 .3-5.6 4 4 0 0 0-7.7 1A2.3 2.3 0 0 0 4.5 12.5z"/>"##
        }
        _ => return None,
    };
    Some(format!("{S}{body}</svg>"))
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(name) = path
            .strip_prefix("plaid/")
            .and_then(|p| p.strip_suffix(".svg"))
        {
            return Ok(icon(name).map(|s| Cow::Owned(s.into_bytes())));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}
