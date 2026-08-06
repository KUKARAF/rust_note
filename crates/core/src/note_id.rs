//! Note identity and path normalization.
//!
//! TODO: define the real `NoteId` type once the notes storage layout
//! (crates/server/src/notes) is finalized. For now this provides a small,
//! genuinely useful helper for turning arbitrary note paths/titles into
//! filesystem- and URL-safe slugs.

/// Normalize an arbitrary string (e.g. a note title or path segment) into a
/// lowercase, hyphen-separated slug safe for use as a filename or URL
/// segment.
///
/// This keeps any Unicode letter/digit (per `char::is_alphanumeric`), not
/// just ASCII, so titles in non-Latin scripts (accented Latin, CJK,
/// Cyrillic, etc.) still produce a meaningful, non-empty slug instead of
/// collapsing to nothing. Characters with no alphanumeric meaning in any
/// script (emoji, punctuation, whitespace, combining marks) are still
/// treated as separators. This is intentionally simple: it does not attempt
/// full Unicode normalization (e.g. NFKC) beyond `char::to_lowercase`.
pub fn slugify(path: &str) -> String {
    let mut slug = String::with_capacity(path.len());
    let mut last_was_hyphen = false;

    for ch in path.trim().chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
            last_was_hyphen = false;
        } else if !last_was_hyphen && !slug.is_empty() {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("  Foo/Bar Baz  "), "foo-bar-baz");
        assert_eq!(slugify("already-slugged"), "already-slugged");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn slugify_preserves_non_ascii_letters() {
        // Accented Latin: the 'é' must survive, not be dropped.
        assert_eq!(slugify("café"), "café");
        // CJK: Unicode ideographs are alphabetic and must be preserved.
        assert_eq!(slugify("日本語のノート"), "日本語のノート");
        // Cyrillic, lowercased.
        assert_eq!(slugify("Привет Мир"), "привет-мир");
        // Emoji and other non-alphanumeric symbols still act as separators
        // and are dropped, same as ASCII punctuation.
        assert_eq!(slugify("hello 🎉 world"), "hello-world");
    }
}
