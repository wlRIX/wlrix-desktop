// SPDX-License-Identifier: GPL-3.0-or-later
//! What a drag off the desktop carries to another application.
//!
//! A drag of desktop icons is a real Wayland drag-and-drop, so the file manager, the icon
//! catalog or anything else that takes files can be the drop target. What travels is the list
//! of files, as `text/uri-list` -- the type every toolkit's file drop reads -- and as plain
//! paths for a text field. The desktop's own icons, the home folder and the Dumpster, travel as
//! nothing: no file of theirs should be moved or copied anywhere by a stray drag, least of all
//! the whole home directory.
//!
//! Every drag also offers [`INTERNAL`], which carries no data. It is what the desktop accepts
//! when the drag comes back onto its own surface, where the drop is a move of the icons rather
//! than a transfer, and what lets it tell its own drag from another application's.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// The desktop's own drag, recognized when it comes back to the desktop. Carries no data.
pub const INTERNAL: &str = "application/x-wlrix-desktop-icons";
/// The file list, one `file://` URI per line, as RFC 2483 has it.
pub const URI_LIST: &str = "text/uri-list";
/// The same files as plain paths, one per line, for a target that only takes text.
pub const PLAIN_TEXT: &str = "text/plain;charset=utf-8";

/// The files one drag carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payload {
    paths: Vec<PathBuf>,
}

impl Payload {
    /// A payload of `paths`, in the order given.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }

    /// Whether there is any file to hand over at all.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The MIME types to offer, the internal one first. A drag of nothing but built-in icons
    /// offers only that, so no other application takes it.
    pub fn mime_types(&self) -> Vec<&'static str> {
        if self.paths.is_empty() {
            vec![INTERNAL]
        } else {
            vec![INTERNAL, URI_LIST, PLAIN_TEXT]
        }
    }

    /// What to write when a target asks for `mime`, or `None` for a type this does not offer.
    pub fn bytes_for(&self, mime: &str) -> Option<Vec<u8>> {
        match mime {
            INTERNAL => Some(Vec::new()),
            URI_LIST if !self.paths.is_empty() => Some(self.uri_list().into_bytes()),
            PLAIN_TEXT if !self.paths.is_empty() => {
                let lines: Vec<&[u8]> = self
                    .paths
                    .iter()
                    .map(|p| p.as_os_str().as_bytes())
                    .collect();
                Some(lines.join(&b'\n'))
            }
            _ => None,
        }
    }

    /// `text/uri-list`: each file as a `file://` URI, every line ended with CRLF.
    fn uri_list(&self) -> String {
        self.paths
            .iter()
            .map(|path| {
                format!(
                    "file://{}\r\n",
                    crate::trash::percent_encode_bytes(path.as_os_str().as_bytes())
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn files_travel_as_a_uri_list_and_as_paths() {
        let payload = Payload::new(vec![
            "/home/a/notes.txt".into(),
            "/home/a/My App.desktop".into(),
        ]);
        assert_eq!(payload.mime_types(), [INTERNAL, URI_LIST, PLAIN_TEXT]);
        assert_eq!(
            payload.bytes_for(URI_LIST).unwrap(),
            b"file:///home/a/notes.txt\r\nfile:///home/a/My%20App.desktop\r\n"
        );
        assert_eq!(
            payload.bytes_for(PLAIN_TEXT).unwrap(),
            b"/home/a/notes.txt\n/home/a/My App.desktop"
        );
    }

    #[test]
    fn the_internal_type_carries_nothing() {
        let payload = Payload::new(vec!["/a".into()]);
        assert_eq!(payload.bytes_for(INTERNAL), Some(Vec::new()));
        assert_eq!(payload.bytes_for("image/png"), None);
    }

    #[test]
    fn a_drag_of_only_built_in_icons_offers_nothing_to_anyone_else() {
        let payload = Payload::default();
        assert!(payload.is_empty());
        assert_eq!(payload.mime_types(), [INTERNAL]);
        assert_eq!(payload.bytes_for(URI_LIST), None);
    }

    #[test]
    fn a_name_that_is_not_utf8_keeps_its_bytes() {
        let path = PathBuf::from(OsStr::from_bytes(b"/home/a/\xff.txt"));
        let payload = Payload::new(vec![path]);
        assert_eq!(
            payload.bytes_for(URI_LIST).unwrap(),
            b"file:///home/a/%FF.txt\r\n"
        );
    }
}
