//! Translation between a public "note id" (as it appears in URLs, e.g.
//! `projects/rust-note`) and the `.md`-suffixed relative path `NotesRepo`
//! expects on disk (e.g. `projects/rust-note.md`).
//!
//! `NotesRepo` already guards against path traversal (`resolve_safe`); this
//! module is purely about the id<->path naming convention, not safety.

/// Maximum length (in bytes) allowed for a single `/`-separated segment of a
/// note id. This is well under typical filesystem `NAME_MAX` limits (255
/// bytes on ext4/most Linux filesystems, even after appending `.md`), so a
/// segment within this bound can never trigger an OS-level `ENAMETOOLONG`
/// when `NotesRepo` resolves it to a path.
pub const MAX_ID_SEGMENT_LEN: usize = 200;

/// Returns `true` if `note_id` is well-formed enough to safely resolve to a
/// file path: non-empty, not absolute, contains no `..`/`.` traversal
/// segments or NUL bytes, and has no oversized segment that could trip an
/// OS-level path-length error. This is a pure input-validation check (no
/// filesystem access), meant to be applied *before* handing a caller-
/// supplied id to `NotesRepo` so malformed input can be rejected with a
/// clean 4xx instead of surfacing a raw filesystem error.
pub fn is_valid_note_id(note_id: &str) -> bool {
    if note_id.is_empty() || note_id.len() > MAX_ID_SEGMENT_LEN * 16 {
        return false;
    }
    if note_id.contains('\0') || note_id.starts_with('/') || note_id.starts_with('\\') {
        return false;
    }
    note_id.split(['/', '\\']).all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment.len() <= MAX_ID_SEGMENT_LEN
    })
}

/// Convert a public note id into the relative `.md` file path used by
/// `NotesRepo`.
///
/// This *always* appends `.md`, even when the id itself already ends in
/// `.md`. That is deliberate and required for the id<->path mapping to be a
/// bijection: `path_to_note_id` strips exactly one `.md` suffix, so the only
/// way to guarantee `note_id_to_path(path_to_note_id(x)) == x` for every
/// on-disk path `x` (including pathological ones like `notes.md.md`) is to
/// mirror that with an unconditional single-suffix append here. The earlier
/// "leave it as-is if it already ends in `.md`" special case broke that
/// round-trip: a vault file `notes.md.md` registered under id `notes.md` but
/// then resolved back to `notes.md` (the wrong file), so a DELETE could
/// remove a different note than the one addressed (see DLI-5).
pub fn note_id_to_path(note_id: &str) -> String {
    let trimmed = note_id.trim().trim_matches('/');
    format!("{trimmed}.md")
}

/// Convert a relative `.md` file path (as returned by `NotesRepo::list_notes`
/// or `history`) back into the public note id used in URLs (i.e. strip the
/// `.md` suffix).
pub fn path_to_note_id(rel_path: &str) -> String {
    rel_path.strip_suffix(".md").unwrap_or(rel_path).to_string()
}

/// Slugify a user-supplied title/path into a safe note id, normalizing each
/// `/`-separated segment independently (so `slugify` doesn't collapse the
/// path separators themselves) and re-joining with `/`.
pub fn slugify_note_id(title_or_path: &str) -> String {
    title_or_path
        .split('/')
        .map(rust_note_core::note_id::slugify)
        .map(truncate_slug_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Whether `note_id` is already in canonical slug form, i.e. running it
/// through [`slugify_note_id`] would be a no-op. Used to reject PUTs that
/// would otherwise auto-vivify a new note under an id that could never have
/// been produced by the create (POST) endpoint, which always slugifies.
pub fn is_slug_clean(note_id: &str) -> bool {
    slugify_note_id(note_id) == note_id
}

/// Truncate an already-slugified segment down to `MAX_ID_SEGMENT_LEN` bytes,
/// trimming any trailing hyphen left behind by the cut so the result still
/// looks like a normal slug. `note_id::slugify` may emit multi-byte Unicode
/// characters (e.g. accented Latin, CJK), so the cut point is walked back to
/// the nearest preceding `char` boundary rather than byte-sliced blindly,
/// which would otherwise be able to panic or split a codepoint in half.
fn truncate_slug_segment(segment: String) -> String {
    if segment.len() <= MAX_ID_SEGMENT_LEN {
        return segment;
    }
    let mut cut = MAX_ID_SEGMENT_LEN;
    while cut > 0 && !segment.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = segment[..cut].to_string();
    while truncated.ends_with('-') {
        truncated.pop();
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_to_path_appends_md() {
        assert_eq!(
            note_id_to_path("projects/rust-note"),
            "projects/rust-note.md"
        );
        // Unconditional append, even when the id already ends in `.md` (see
        // DLI-5 / the bijection round-trip test below).
        assert_eq!(note_id_to_path("already.md"), "already.md.md");
        assert_eq!(note_id_to_path("/leading-slash"), "leading-slash.md");
    }

    #[test]
    fn path_to_id_strips_md() {
        assert_eq!(
            path_to_note_id("projects/rust-note.md"),
            "projects/rust-note"
        );
        assert_eq!(path_to_note_id("no-suffix"), "no-suffix");
    }

    #[test]
    fn id_path_round_trip_is_bijective_even_for_md_suffixed_files() {
        // Regression test for DLI-5: for *any* `.md` path on disk, mapping it
        // to an id and back must yield the exact same path, so a note is
        // always addressed by (and deleted from) the file it actually came
        // from. The `notes.md.md` case is the one the old idempotent
        // `note_id_to_path` got wrong.
        for path in [
            "notes.md",
            "notes.md.md",
            "projects/rust-note.md",
            "a/b/c.md",
            "weird.md.md.md",
            "trailing.md",
        ] {
            let id = path_to_note_id(path);
            assert_eq!(
                note_id_to_path(&id),
                path,
                "round-trip must be identity for on-disk path {path:?} (via id {id:?})"
            );
        }
    }

    #[test]
    fn is_slug_clean_accepts_canonical_slugs_and_rejects_others() {
        assert!(is_slug_clean("projects/rust-note"));
        assert!(is_slug_clean("plain-id"));
        assert!(is_slug_clean(""));

        // Leading underscore, mixed case, spaces, punctuation: none of
        // these could ever come out of slugify_note_id, so they must be
        // rejected as PUT-auto-vivify targets.
        assert!(!is_slug_clean("_test_sweep/Weird Id!"));
        assert!(!is_slug_clean("Test-Sweep/foo"));
        assert!(!is_slug_clean("foo_bar"));
        assert!(!is_slug_clean("foo bar"));
        assert!(!is_slug_clean("UPPER"));
    }

    #[test]
    fn is_valid_note_id_rejects_traversal_and_absolute() {
        assert!(!is_valid_note_id("../../etc/passwd"));
        assert!(!is_valid_note_id("foo/../bar"));
        assert!(!is_valid_note_id("/absolute/path"));
        assert!(!is_valid_note_id(""));
        assert!(!is_valid_note_id("."));
        assert!(is_valid_note_id("projects/rust-note"));
        assert!(is_valid_note_id("plain-id"));
    }

    #[test]
    fn is_valid_note_id_rejects_oversized_segments() {
        let huge_segment = "a".repeat(MAX_ID_SEGMENT_LEN + 1);
        assert!(!is_valid_note_id(&huge_segment));

        let ok_segment = "a".repeat(MAX_ID_SEGMENT_LEN);
        assert!(is_valid_note_id(&ok_segment));
    }

    #[test]
    fn slugify_note_id_truncates_oversized_segments() {
        let huge_title = "a".repeat(50_000);
        let slug = slugify_note_id(&huge_title);
        assert!(slug.len() <= MAX_ID_SEGMENT_LEN);
        assert!(is_valid_note_id(&slug));
    }

    #[test]
    fn slugify_note_id_normalizes_segments() {
        assert_eq!(
            slugify_note_id("My Project/Cool Note!"),
            "my-project/cool-note"
        );
        assert_eq!(slugify_note_id("  spaced out  "), "spaced-out");
    }

    #[test]
    fn slugify_note_id_preserves_non_latin_path_segments() {
        // Regression test: a non-Latin-script segment used to slugify to
        // the empty string and get filtered out entirely, silently
        // collapsing "folder/日本語" down to just "folder" (a different,
        // pre-existing note id) instead of a nested note under "folder/".
        assert_eq!(slugify_note_id("test-sweep/日本語"), "test-sweep/日本語");
        assert_eq!(slugify_note_id("test-sweep/café"), "test-sweep/café");
        // Distinct non-Latin titles under the same folder must still slugify
        // to distinct, non-colliding ids.
        assert_ne!(
            slugify_note_id("test-sweep/日本語"),
            slugify_note_id("test-sweep/中文标题")
        );
    }

    #[test]
    fn slugify_note_id_truncates_multibyte_segments_without_panicking() {
        // A segment made entirely of multi-byte characters must be
        // truncated at a char boundary, not byte-sliced blindly.
        let huge_title = "日".repeat(1000);
        let slug = slugify_note_id(&huge_title);
        assert!(slug.len() <= MAX_ID_SEGMENT_LEN);
        assert!(is_valid_note_id(&slug));
    }
}
