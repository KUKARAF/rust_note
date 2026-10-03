//! Minimal YAML-frontmatter parsing/rendering for git-backed markdown notes.
//!
//! This is intentionally a tiny hand-rolled subset of YAML — just enough to
//! round-trip simple `key: value` frontmatter blocks (as used by the
//! per-user settings note) without pulling in a full YAML parser. It never
//! errors: malformed or missing frontmatter degrades to "no fields, whole
//! content is body" so callers never have to handle a parse failure for
//! user-editable files.

/// A parsed `---\n...\n---\n` frontmatter block plus the remaining body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    /// Order-preserving list of `(key, value)` pairs.
    pub fields: Vec<(String, String)>,
    /// In-block lines that aren't a flat `key: value` pair (nested YAML
    /// lists, comments, block scalars, …), preserved verbatim and in order
    /// so a note with non-flat frontmatter round-trips losslessly through
    /// `parse` -> `set` -> `render` instead of being demoted to `body`.
    pub raw_extra: Vec<String>,
    pub body: String,
}

impl Frontmatter {
    /// Parse `content`. If it doesn't start with a well-formed
    /// `---\n...\n---\n` block, returns
    /// `{ fields: vec![], raw_extra: vec![], body: content.to_string() }`
    /// — never errors.
    ///
    /// A block with a proper closing fence may still contain lines this
    /// parser can't read as flat `key: value` (nested lists, comments,
    /// block scalars, …); those lines are kept verbatim in `raw_extra`
    /// rather than demoting the whole block to `body`. Only a missing
    /// closing fence degrades the whole input to "no frontmatter".
    pub fn parse(content: &str) -> Self {
        let Some(rest) = content.strip_prefix("---\n") else {
            return Self {
                fields: Vec::new(),
                raw_extra: Vec::new(),
                body: content.to_string(),
            };
        };

        // Find the closing `---` fence: a line that is exactly `---`.
        let mut fields = Vec::new();
        let mut raw_extra = Vec::new();
        let mut lines = rest.split('\n');
        let mut consumed_len = 0usize; // bytes of `rest` consumed through (and including) the closing fence line + its newline
        let mut closed = false;

        for line in lines.by_ref() {
            consumed_len += line.len();
            if line == "---" {
                consumed_len += 1; // the '\n' that terminated this line
                closed = true;
                break;
            }
            consumed_len += 1; // the '\n' that terminated this line

            if line.trim().is_empty() {
                raw_extra.push(line.to_string());
                continue;
            }

            let is_flat_field = match line.split_once(':') {
                Some((key, raw_value)) => {
                    let key_trimmed = key.trim();
                    // A flat field has no leading/trailing whitespace on the
                    // key (ruling out indented/nested lines), doesn't start
                    // with a YAML-special marker (list item `-`, comment
                    // `#`), and has a non-empty value: a bare `key:` with
                    // nothing after it is almost always a nested
                    // list/mapping header (e.g. `contacts:` followed by
                    // indented `- name: ...` entries), not a flat scalar.
                    !key_trimmed.is_empty()
                        && key == key_trimmed
                        && !key_trimmed.starts_with('-')
                        && !key_trimmed.starts_with('#')
                        && !raw_value.trim().is_empty()
                }
                None => false,
            };

            if is_flat_field {
                // Safe: `split_once` above guarantees a `:` is present.
                if let Some((key, raw_value)) = line.split_once(':') {
                    let value = unquote(raw_value.trim());
                    fields.push((key.trim().to_string(), value));
                }
            } else {
                // Not a flat field we understand (nested list, comment,
                // block scalar, …): preserve the line verbatim rather than
                // dropping it or demoting the whole block.
                raw_extra.push(line.to_string());
            }
        }

        if !closed {
            // Opening fence with no closing fence: degrade to no-frontmatter.
            return Self {
                fields: Vec::new(),
                raw_extra: Vec::new(),
                body: content.to_string(),
            };
        }

        // `rest` started right after the opening "---\n"; the body starts
        // right after the consumed prefix (closing fence line + its '\n').
        let body = rest.get(consumed_len..).unwrap_or_default().to_string();

        Self {
            fields,
            raw_extra,
            body,
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Insert or overwrite a field, preserving existing order for an
    /// existing key, appending for a new one.
    pub fn set(&mut self, key: &str, value: &str) {
        if let Some(entry) = self.fields.iter_mut().find(|(k, _)| k == key) {
            entry.1 = value.to_string();
        } else {
            self.fields.push((key.to_string(), value.to_string()));
        }
    }

    /// Remove every field whose key equals `prefix` or starts with
    /// `"{prefix}."` (a dotted namespace). Returns the number removed. Used to
    /// drop a whole `stat.<metric>.*` group when deleting a metric definition.
    pub fn remove_prefix(&mut self, prefix: &str) -> usize {
        let dotted = format!("{prefix}.");
        let before = self.fields.len();
        self.fields
            .retain(|(k, _)| k != prefix && !k.starts_with(&dotted));
        before - self.fields.len()
    }

    /// Render back to a SINGLE `---\nkey: value\n...\n{raw_extra}---\n{body}`
    /// block: flat fields first (as `key: value`), then any preserved
    /// `raw_extra` lines verbatim, in order. If there are no fields and no
    /// `raw_extra` lines, renders just the body with no frontmatter block
    /// at all (so an empty `Frontmatter` round-trips to plain content,
    /// matching `parse`'s behavior for no-frontmatter input).
    pub fn render(&self) -> String {
        if self.fields.is_empty() && self.raw_extra.is_empty() {
            return self.body.clone();
        }

        let mut out = String::from("---\n");
        for (key, value) in &self.fields {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(&quote(value));
            out.push('\n');
        }
        for line in &self.raw_extra {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }
}

/// Whether `value` needs to be quoted on write: contains `:` or a newline,
/// has leading/trailing whitespace, or starts with a YAML-special
/// character.
fn needs_quoting(value: &str) -> bool {
    if value.contains(':') {
        return true;
    }
    if value.contains('\n') {
        return true;
    }
    if value != value.trim() {
        return true;
    }
    if value.is_empty() {
        return true;
    }
    let mut chars = value.chars();
    match chars.next() {
        Some('#' | '[' | '{' | '"' | '\'') => true,
        // "- " (hyphen followed by space) is YAML-special (list item).
        Some('-') => value.starts_with("- "),
        _ => false,
    }
}

/// Quote+escape `value` if needed; otherwise return it bare.
///
/// Fields are stored one-per-line, so a literal newline inside a value
/// would otherwise corrupt the block (splitting it into extra, unparseable
/// lines) or silently vanish on reparse. Policy: escape embedded newlines
/// as the two-character sequence `\n` (mirroring `\\`/`\"` escaping below),
/// which `unquote` reverses — this keeps `set`/`render`/`parse` total
/// (never rejecting a caller-supplied value) while staying lossless.
fn quote(value: &str) -> String {
    if !needs_quoting(value) {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Unquote/unescape a raw value read from a frontmatter line. If it starts
/// with `"`, unescape `\"`, `\\` and `\n` and strip the surrounding quotes
/// (tolerating a missing closing quote rather than panicking). Otherwise
/// the value is taken literally (already trimmed by the caller).
fn unquote(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"') else {
        return raw.to_string();
    };
    let inner = inner.strip_suffix('"').unwrap_or(inner);

    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_multi_key_multi_line_body() {
        let content = "---\ntheme: ration\nfoo: bar\n---\n# Heading\n\nSome body\ntext here.\n";
        let fm = Frontmatter::parse(content);
        assert_eq!(
            fm.fields,
            vec![
                ("theme".to_string(), "ration".to_string()),
                ("foo".to_string(), "bar".to_string()),
            ]
        );
        assert_eq!(fm.body, "# Heading\n\nSome body\ntext here.\n");

        let rendered = fm.render();
        let reparsed = Frontmatter::parse(&rendered);
        assert_eq!(reparsed.fields, fm.fields);
        assert_eq!(reparsed.body, fm.body);
    }

    #[test]
    fn unknown_keys_preserved_across_set_on_different_key() {
        let content = "---\ntheme: ration\nfuture_key: keep-me\n---\nbody\n";
        let mut fm = Frontmatter::parse(content);
        fm.set("theme", "ration");
        assert_eq!(fm.get("future_key"), Some("keep-me"));
        assert_eq!(fm.get("theme"), Some("ration"));
    }

    #[test]
    fn body_preserved_byte_for_byte_across_frontmatter_only_set() {
        let content = "---\ntheme: ration\n---\nLine one.\n\nLine two with  double  spaces.\n";
        let mut fm = Frontmatter::parse(content);
        let original_body = fm.body.clone();
        fm.set("theme", "other");
        assert_eq!(fm.body, original_body);
    }

    #[test]
    fn no_frontmatter_input_is_all_body() {
        let content = "just plain text\nno frontmatter here\n";
        let fm = Frontmatter::parse(content);
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, content);
    }

    #[test]
    fn malformed_frontmatter_no_closing_fence_degrades_to_body() {
        let content = "---\ntheme: ration\nno closing fence\n";
        let fm = Frontmatter::parse(content);
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, content);
    }

    #[test]
    fn values_with_colon_and_whitespace_round_trip() {
        let mut fm = Frontmatter {
            fields: Vec::new(),
            raw_extra: Vec::new(),
            body: "body\n".to_string(),
        };
        fm.set("url", "https://example.com:8080/path");
        fm.set("padded", "  leading and trailing  ");

        let rendered = fm.render();
        let reparsed = Frontmatter::parse(&rendered);
        assert_eq!(reparsed.get("url"), Some("https://example.com:8080/path"));
        assert_eq!(reparsed.get("padded"), Some("  leading and trailing  "));
    }

    #[test]
    fn empty_content_does_not_panic() {
        let fm = Frontmatter::parse("");
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, "");
    }

    #[test]
    fn empty_frontmatter_block_does_not_panic() {
        let fm = Frontmatter::parse("---\n---\n");
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, "");
    }

    #[test]
    fn empty_frontmatter_block_with_body_does_not_panic() {
        let fm = Frontmatter::parse("---\n---\nbody text\n");
        assert!(fm.fields.is_empty());
        assert_eq!(fm.body, "body text\n");
    }

    /// R1 (Critical): an externally-authored note with non-flat frontmatter
    /// (a nested YAML list, as produced by Obsidian/vimwiki or our scaffolded
    /// `leads/*` notes) must survive `parse -> set -> render` as exactly one
    /// `---...---` block, with its flat fields recognized, its nested lines
    /// preserved, and no data loss — and a second `set + render` must be
    /// byte-stable (idempotent).
    #[test]
    fn nested_list_frontmatter_round_trips_as_single_block_and_is_idempotent() {
        let content =
            "---\ncontacts:\n  - name: A\n    email: a@x\nkind: lead\ncompany: Foo\n---\nbody\n";
        let fm = Frontmatter::parse(content);

        // Flat fields are recognized despite the nested list being present.
        assert_eq!(fm.get("kind"), Some("lead"));
        assert_eq!(fm.get("company"), Some("Foo"));
        // The nested lines are preserved verbatim rather than dropped.
        assert_eq!(
            fm.raw_extra,
            vec![
                "contacts:".to_string(),
                "  - name: A".to_string(),
                "    email: a@x".to_string(),
            ]
        );
        assert_eq!(fm.body, "body\n");

        let mut fm = fm;
        fm.set("stage", "interview");
        let rendered = fm.render();

        // Exactly one frontmatter block: only two `---` fence lines total.
        let fence_count = rendered.lines().filter(|l| *l == "---").count();
        assert_eq!(fence_count, 2, "expected exactly one frontmatter block");

        let reparsed = Frontmatter::parse(&rendered);
        assert_eq!(reparsed.get("kind"), Some("lead"));
        assert_eq!(reparsed.get("company"), Some("Foo"));
        assert_eq!(reparsed.get("stage"), Some("interview"));
        assert_eq!(
            reparsed.raw_extra,
            vec![
                "contacts:".to_string(),
                "  - name: A".to_string(),
                "    email: a@x".to_string(),
            ]
        );
        assert_eq!(reparsed.body, "body\n");

        // A second set + render is byte-stable (idempotent).
        let mut fm2 = reparsed;
        fm2.set("stage", "interview");
        let rendered2 = fm2.render();
        assert_eq!(rendered2, rendered);
    }

    /// R1 follow-up: a comment line and a top-level list item inside the
    /// block are also preserved rather than demoting the whole block.
    #[test]
    fn comment_and_top_level_list_item_preserved() {
        let content = "---\n# a comment\nkind: task\n- stray list item\n---\nbody\n";
        let fm = Frontmatter::parse(content);
        assert_eq!(fm.get("kind"), Some("task"));
        assert_eq!(
            fm.raw_extra,
            vec!["# a comment".to_string(), "- stray list item".to_string()]
        );
        assert_eq!(fm.body, "body\n");
    }

    /// L5: a value containing an embedded newline must round-trip rather
    /// than corrupting the block. Policy: `quote`/`unquote` escape `\n` as
    /// the two-character sequence `\n` (like `\\` and `\"`), so `set` never
    /// has to reject a caller-supplied value.
    #[test]
    fn value_with_embedded_newline_round_trips() {
        let mut fm = Frontmatter {
            fields: Vec::new(),
            raw_extra: Vec::new(),
            body: "body\n".to_string(),
        };
        fm.set("note", "line one\nline two");

        let rendered = fm.render();
        // The block stays well-formed: exactly one closing fence line.
        assert_eq!(rendered.lines().filter(|l| *l == "---").count(), 2);

        let reparsed = Frontmatter::parse(&rendered);
        assert_eq!(reparsed.get("note"), Some("line one\nline two"));
        assert_eq!(reparsed.body, "body\n");
    }
}
