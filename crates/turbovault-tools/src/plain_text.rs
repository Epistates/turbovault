//! A note's readable text, for the indexes and reports built over every note.

use turbovault_core::VaultFile;
use turbovault_parser::to_plain_text;

/// `file`'s content as plain text: [`to_plain_text`], or its Markdown as
/// written when rendering that panics.
///
/// Rendering parses the note a second time, with the block parser, which the
/// vault's scan never ran on it. Search, similarity, quality scoring and the
/// vault statistics each render every note, so a note that trips a parser
/// bug there would fail the whole index or report, and fail it again on
/// every call. Caught here, it costs that note its rendering and nothing
/// else: the raw text still holds every word in it, and a warning names the
/// note.
///
/// The closure borrows only the note's text and returns a new `String`, so a
/// panic leaves nothing half-updated behind it.
pub(crate) fn note_text(file: &VaultFile) -> String {
    let content = file.content.as_str();
    std::panic::catch_unwind(|| {
        #[cfg(test)]
        panic_on_marker(content);
        to_plain_text(content)
    })
    .unwrap_or_else(|panic| {
        tracing::warn!(
            "Rendering {} as plain text failed, so its Markdown is indexed as written: {}",
            file.path.display(),
            turbovault_core::panic_message(panic.as_ref())
        );
        content.to_string()
    })
}

/// A note holding this text makes its rendering panic, in this crate's tests
/// only. It stands in for a bug in the Markdown parser, which has no input
/// that is both known to panic it and safe to keep in the repository.
#[cfg(test)]
pub(crate) const RENDER_PANIC_MARKER: &str = "<!-- test: rendering this note panics -->";

#[cfg(test)]
fn panic_on_marker(content: &str) {
    if content.contains(RENDER_PANIC_MARKER) {
        panic!("test render panic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use turbovault_core::FileMetadata;

    fn note(content: &str) -> VaultFile {
        let path = PathBuf::from("/vault/note.md");
        let metadata = FileMetadata {
            path: path.clone(),
            size: content.len() as u64,
            created_at: 0.0,
            modified_at: 0.0,
            checksum: String::new(),
            is_attachment: false,
        };
        VaultFile::new(path, content.to_string(), metadata)
    }

    #[test]
    fn a_note_renders_as_plain_text() {
        let content = "# Title\n\nSee [[Other|that]].\n";
        assert_eq!(note_text(&note(content)), to_plain_text(content));
        assert_ne!(note_text(&note(content)), content);
    }

    #[test]
    fn a_note_whose_rendering_panics_keeps_its_text_as_written() {
        let content = format!("# Title\n\n{RENDER_PANIC_MARKER}\n\n**harbor**\n");
        assert_eq!(note_text(&note(&content)), content);
    }
}
