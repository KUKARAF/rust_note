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
    pub body: String,
}

impl Frontmatter {
    /// Parse `content`. If it doesn't start with a well-formed
    /// `---\n...\n---\n` block, returns `{ fields: vec![], body: content.to_string() }`
    /// — never errors.
    pub fn parse(content: &str) -> Self {
        let Some(rest) = content.strip_prefix("---\n") else {
            return Self {
                fields: Vec::new(),
                body: content.to_string(),
            };
        };

        // Find the closing `---` fence: a line that is exactly `---`.
        let mut fields = Vec::new();
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
                continue;
            }
            if let Some((key, raw_value)) = line.split_once(':') {
                let key = key.trim();
                if key.is_empty() {
                    // Not a well-formed `key:` line; treat whole thing as
                    // malformed frontmatter.
                    return Self {
                        fields: Vec::new(),
                        body: content.to_string(),
                    };
                }
                let value = unquote(raw_value.trim());
                fields.push((key.to_string(), value));
            } else {
                // A non-empty line with no `:` isn't a field we understand;
                // treat the whole block as malformed rather than silently
                // dropping data.
                return Self {
                    fields: Vec::new(),
                    body: content.to_string(),
                };
            }
        }

        if !closed {
            // Opening fence with no closing fence: degrade to no-frontmatter.
            return Self {
                fields: Vec::new(),
                body: content.to_string(),
            };
        }

        // `rest` started right after the opening "---\n"; the body starts
        // right after the consumed prefix (closing fence line + its '\n').
        let body = rest.get(consumed_len..).unwrap_or("").to_string();

        Self { fields, body }
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

    /// Render back to `---\nkey: value\n...\n---\n{body}`. If there are no
    /// fields, renders just the body with no frontmatter block at all (so
    /// an empty `Frontmatter` round-trips to plain content, matching
    /// `parse`'s behavior for no-frontmatter input).
    pub fn render(&self) -> String {
        if self.fields.is_empty() {
            return self.body.clone();
        }

        let mut out = String::from("---\n");
        for (key, value) in &self.fields {
            out.push_str(key);
            out.push_str(": ");
            out.push_str(&quote(value));
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(&self.body);
        out
    }
}

/// Whether `value` needs to be quoted on write: contains `:`, has
/// leading/trailing whitespace, or starts with a YAML-special character.
fn needs_quoting(value: &str) -> bool {
    if value.contains(':') {
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
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Unquote/unescape a raw value read from a frontmatter line. If it starts
/// with `"`, unescape `\"` and `\\` and strip the surrounding quotes
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
}
