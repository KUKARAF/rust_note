//! Op model + parsing + anchor validation + instruction extraction for the
//! in-note `#AI!` command (see `docs/ai-command.md`).
//!
//! The LLM returns a small set of **anchored** find/replace ops rather than a
//! whole-note rewrite. Every anchor is validated against the *current* note
//! text (each `find` must occur EXACTLY ONCE) before it is applied, so a stale
//! or ambiguous anchor is skipped rather than corrupting the note.

use serde::Deserialize;

/// Trailing marker that turns a line into an `#AI!` instruction.
pub const MARKER: &str = "#AI!";

/// Instruction used when the command line is bare (`#AI!` only).
pub const DEFAULT_INSTRUCTION: &str = "Improve and clean up this note.";

/// A single edit operation emitted by the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Replace the (unique) `find` substring with `with`.
    Replace { find: String, with: String },
    /// Insert `text` immediately after the (unique) `find` substring.
    InsertAfter { find: String, text: String },
    /// Append `text` at the very end of the note.
    Append { text: String },
}

impl Op {
    /// Validate this op against `text` and return the byte offset at which the
    /// edit lands (used to position the "AI" cursor). Returns `None` when the
    /// op must be skipped: an empty anchor, or a `find` that does not occur
    /// EXACTLY ONCE in the current text.
    pub fn validate_and_offset(&self, text: &str) -> Option<usize> {
        match self {
            Op::Replace { find, .. } => {
                let idx = unique_match(text, find)?;
                Some(idx)
            }
            Op::InsertAfter { find, .. } => {
                let idx = unique_match(text, find)?;
                Some(idx + find.len())
            }
            Op::Append { .. } => Some(text.len()),
        }
    }

    /// Produce the full new note text after applying this op to `old`, or
    /// `None` if the op no longer validates against `old` (so the caller skips
    /// it). Validation is re-checked here against the exact text the splice
    /// will be computed from, closing any gap with [`Op::validate_and_offset`].
    pub fn apply_to(&self, old: &str) -> Option<String> {
        match self {
            Op::Replace { find, with } => {
                unique_match(old, find)?;
                Some(old.replacen(find.as_str(), with, 1))
            }
            Op::InsertAfter { find, text } => {
                let idx = unique_match(old, find)?;
                let pos = idx + find.len();
                let before = old.get(..pos)?;
                let after = old.get(pos..)?;
                Some(format!("{before}{text}{after}"))
            }
            Op::Append { text } => Some(format!("{old}{text}")),
        }
    }
}

/// Byte index of `needle` in `haystack` iff it occurs EXACTLY ONCE (and is
/// non-empty). `None` for zero, two-or-more, or empty-needle cases.
fn unique_match(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let mut matches = haystack.match_indices(needle);
    let first = matches.next()?.0;
    if matches.next().is_some() {
        return None; // ambiguous: more than one occurrence
    }
    Some(first)
}

// ---- model-output parsing ------------------------------------------------

/// Envelope the model is asked to emit: `{"ops":[ ... ]}`.
#[derive(Debug, Deserialize)]
struct OpsEnvelope {
    #[serde(default)]
    ops: Vec<RawOp>,
}

/// One raw op as the model phrased it, before shape validation.
#[derive(Debug, Deserialize)]
struct RawOp {
    op: String,
    #[serde(default)]
    find: Option<String>,
    #[serde(default)]
    with: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

impl RawOp {
    /// Convert to a typed [`Op`], dropping anything whose required fields are
    /// missing or whose `op` tag is unknown.
    fn into_op(self) -> Option<Op> {
        match self.op.as_str() {
            "replace" => Some(Op::Replace {
                find: self.find?,
                with: self.with.unwrap_or_default(),
            }),
            "insert_after" => Some(Op::InsertAfter {
                find: self.find?,
                text: self.text?,
            }),
            "append" => Some(Op::Append { text: self.text? }),
            _ => None,
        }
    }
}

/// Parse the model's raw content into a list of ops.
///
/// Tolerates prose and ```` ``` ```` fences around the JSON by extracting the
/// first balanced `{ … }` object. Returns `Err(reason)` when no JSON object is
/// present or it fails to deserialize; `Ok(vec)` (possibly empty, with
/// malformed individual ops dropped) otherwise.
pub fn parse_ops(content: &str) -> Result<Vec<Op>, String> {
    let json = first_balanced_object(content).ok_or("no JSON object in model output")?;
    let envelope: OpsEnvelope =
        serde_json::from_str(json).map_err(|e| format!("invalid ops JSON: {e}"))?;
    Ok(envelope
        .ops
        .into_iter()
        .filter_map(RawOp::into_op)
        .collect())
}

/// Return the first balanced `{ … }` object substring of `content`, honoring
/// string literals and escapes so a `}` inside a JSON string doesn't close the
/// object prematurely. `None` if there is no balanced object.
fn first_balanced_object(content: &str) -> Option<&str> {
    let bytes = content.as_bytes();
    let start = content.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = start;
    while i < bytes.len() {
        // Indexing is bounded by the `while` guard; `.get` keeps clippy happy.
        let b = *bytes.get(i)?;
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return content.get(start..=i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

// ---- command-line detection + instruction extraction ---------------------

/// A located `#AI!` command line: the byte range it occupies in the note
/// (including the newline that should be removed with it) and the extracted
/// instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// Start byte of the span to delete (may include a leading newline when the
    /// command is the final line).
    pub start: usize,
    /// End byte (exclusive) of the span to delete (includes the trailing
    /// newline when present).
    pub end: usize,
    /// The instruction for the model (bare `#AI!` → [`DEFAULT_INSTRUCTION`]).
    pub instruction: String,
}

/// Locate the first line whose trimmed text ends with [`MARKER`] and describe
/// how to strip it + what instruction it carries. `None` if no command line.
pub fn locate_command(text: &str) -> Option<Command> {
    let mut line_start = 0usize;
    for line in text.split('\n') {
        let line_len = line.len();
        let trimmed = line.trim();
        if trimmed.ends_with(MARKER) {
            let instr = trimmed
                .get(..trimmed.len() - MARKER.len())
                .unwrap_or("")
                .trim();
            let instruction = if instr.is_empty() {
                DEFAULT_INSTRUCTION.to_string()
            } else {
                instr.to_string()
            };

            // Delete the whole line. Prefer eating the trailing newline so no
            // blank line is left behind; if this is the final line (no trailing
            // newline) eat the preceding one instead.
            let mut start = line_start;
            let mut end = line_start + line_len;
            if text.as_bytes().get(end) == Some(&b'\n') {
                end += 1;
            } else if start > 0 && text.as_bytes().get(start - 1) == Some(&b'\n') {
                start -= 1;
            }

            return Some(Command {
                start,
                end,
                instruction,
            });
        }
        line_start += line_len + 1; // +1 for the '\n' that `split` consumed
    }
    None
}

/// The note text with its command line removed, or `None` if there is no
/// command line (used as the `apply_text_edit` splice that strips the marker).
pub fn strip_command_line(text: &str) -> Option<String> {
    let cmd = locate_command(text)?;
    let before = text.get(..cmd.start)?;
    let after = text.get(cmd.end..)?;
    Some(format!("{before}{after}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_command_extracts_instruction_and_strips_line() {
        let note = "# Title\n\nSome body.\nmake this terser #AI!\ntail\n";
        let cmd = locate_command(note).expect("command line found");
        assert_eq!(cmd.instruction, "make this terser");
        let stripped = strip_command_line(note).unwrap();
        assert_eq!(stripped, "# Title\n\nSome body.\ntail\n");
    }

    #[test]
    fn bare_marker_uses_default_instruction() {
        let note = "body\n#AI!\n";
        let cmd = locate_command(note).unwrap();
        assert_eq!(cmd.instruction, DEFAULT_INSTRUCTION);
        assert_eq!(strip_command_line(note).unwrap(), "body\n");

        // Marker with only surrounding whitespace is still bare.
        let note2 = "body\n   #AI!   \n";
        assert_eq!(
            locate_command(note2).unwrap().instruction,
            DEFAULT_INSTRUCTION
        );
    }

    #[test]
    fn command_as_final_line_without_trailing_newline() {
        let note = "line one\nfix grammar #AI!";
        let cmd = locate_command(note).unwrap();
        assert_eq!(cmd.instruction, "fix grammar");
        // The preceding newline is eaten so no dangling blank line remains.
        assert_eq!(strip_command_line(note).unwrap(), "line one");
    }

    #[test]
    fn no_marker_is_none() {
        assert!(locate_command("just a note\nno command here\n").is_none());
        assert!(strip_command_line("nothing").is_none());
        // A line merely containing the marker mid-text does not count.
        assert!(locate_command("see #AI! for details\n").is_none());
    }

    #[test]
    fn parse_ops_strips_fences_and_drops_malformed() {
        let raw = "Sure! Here are the edits:\n```json\n{\"ops\":[\
            {\"op\":\"replace\",\"find\":\"old\",\"with\":\"new\"},\
            {\"op\":\"insert_after\",\"find\":\"anchor\",\"text\":\"\\nmore\"},\
            {\"op\":\"append\",\"text\":\"\\ntail\"},\
            {\"op\":\"replace\",\"with\":\"missing find\"},\
            {\"op\":\"bogus\",\"text\":\"x\"}\
            ]}\n```\n";
        let ops = parse_ops(raw).unwrap();
        assert_eq!(
            ops,
            vec![
                Op::Replace {
                    find: "old".into(),
                    with: "new".into()
                },
                Op::InsertAfter {
                    find: "anchor".into(),
                    text: "\nmore".into()
                },
                Op::Append {
                    text: "\ntail".into()
                },
            ],
            "malformed replace (no find) and unknown op are dropped"
        );
    }

    #[test]
    fn parse_ops_tolerates_brace_in_string_and_requires_an_object() {
        let raw = "{\"ops\":[{\"op\":\"append\",\"text\":\"a } brace\"}]}";
        let ops = parse_ops(raw).unwrap();
        assert_eq!(
            ops,
            vec![Op::Append {
                text: "a } brace".into()
            }]
        );
        assert!(parse_ops("no json at all").is_err());
    }

    #[test]
    fn validate_requires_verbatim_unique_anchor() {
        let text = "alpha beta alpha gamma";
        // "alpha" occurs twice -> ambiguous -> skipped.
        let ambiguous = Op::Replace {
            find: "alpha".into(),
            with: "X".into(),
        };
        assert_eq!(ambiguous.validate_and_offset(text), None);
        assert_eq!(ambiguous.apply_to(text), None);

        // "beta" occurs exactly once.
        let ok = Op::Replace {
            find: "beta".into(),
            with: "BETA".into(),
        };
        assert_eq!(ok.validate_and_offset(text), Some(6));
        assert_eq!(ok.apply_to(text).unwrap(), "alpha BETA alpha gamma");

        // Missing anchor -> skipped.
        let missing = Op::InsertAfter {
            find: "zzz".into(),
            text: "!".into(),
        };
        assert_eq!(missing.validate_and_offset(text), None);
    }

    #[test]
    fn insert_after_and_append_offsets_and_text() {
        let text = "one two three";
        let insert = Op::InsertAfter {
            find: "two".into(),
            text: "-X".into(),
        };
        assert_eq!(insert.validate_and_offset(text), Some(7)); // after "one two"
        assert_eq!(insert.apply_to(text).unwrap(), "one two-X three");

        let append = Op::Append {
            text: "\nend".into(),
        };
        assert_eq!(append.validate_and_offset(text), Some(text.len()));
        assert_eq!(append.apply_to(text).unwrap(), "one two three\nend");
    }
}
