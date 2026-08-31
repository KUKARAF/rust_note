//! Parsing markdown checkbox tasks out of daily-note bodies.
//!
//! Hand-rolled (no regex) in the spirit of [`crate::frontmatter`], and kept
//! dependency-light so `core` stays usable from both server and mobile. It
//! never errors: a line that isn't a recognizable task is simply skipped.
//!
//! Recognized on each task line, matching what the vault actually uses:
//! - Markers: `- [ ]`/`* [ ]`/`+ [ ]` open, `[x]`/`[X]` done, `[m]` carry-over
//!   and `[o]` legacy-started (both treated as open). A double space after the
//!   bullet (`-  [ ]`) is tolerated.
//! - Inline tokens: `p:N` (pomodoro estimate), `start:HH:MM` (time-of-day),
//!   `due:<freeform>` (consumes to end of line), `#tag`, `@location`.
//! - Kitchen-burner priority from tags: `#fb`/`#frontburner`, `#bb`/`#backburner`,
//!   `#fridge`, `#oven`.
//! - Locations (`@work`, `@home`, `@store`) are GTD-style contexts, lowercased
//!   and collected like tags; a task with none is left with an empty list.
//! - Nesting via leading indentation, normalized to a 0-based `depth`.

use serde::{Deserialize, Serialize};

/// Kitchen-burner priority, derived from a task's tags. Ordered most- to
/// least-urgent as a base ranking (`#fridge` escalates with age in consumers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Burner {
    Frontburner,
    Backburner,
    Fridge,
    Oven,
}

/// A parsed markdown checkbox task line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// 1-based line number within the source document (for locating the line
    /// again when toggling the checkbox).
    pub line: usize,
    /// Nesting depth, 0 for a top-level task, normalized from indentation so
    /// tab- and space-indented sub-items both come out as 1, 2, ….
    pub depth: usize,
    /// Raw marker character between the brackets (`' '`, `'x'`, `'o'`, `'m'`, …).
    pub marker: char,
    /// Whether the task is completed (`x`/`X`). `[ ]`/`[m]`/`[o]` are open.
    pub done: bool,
    /// Task text after the checkbox, verbatim (metadata tokens left in place).
    pub text: String,
    /// Task text with the recognized metadata tokens stripped, for display.
    pub text_clean: String,
    /// Pomodoro estimate from a `p:N` token, if present.
    pub pomodoros: Option<u32>,
    /// Time-of-day from a `start:HH:MM` token, kept as-is (e.g. `"09:40"`).
    pub start: Option<String>,
    /// Freeform due text from a `due:…` token (everything to end of line).
    pub due: Option<String>,
    /// All `#tag`s on the line, without the leading `#`.
    pub tags: Vec<String>,
    /// All `@location` contexts on the line, without the leading `@` and
    /// lowercased (e.g. `["work", "home"]`). Empty when the task has none.
    pub locations: Vec<String>,
    /// Kitchen-burner priority derived from `tags`, if any.
    pub burner: Option<Burner>,
}

/// Parse every checkbox task line out of `doc`.
pub fn parse_tasks(doc: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    // Indent-width stack, used to normalize raw indentation into a 0-based
    // `depth`. Each entry is the indent width of an ancestor level.
    let mut stack: Vec<usize> = Vec::new();

    for (idx, raw_line) in doc.split('\n').enumerate() {
        let Some((indent_width, marker, text)) = parse_task_line(raw_line) else {
            continue;
        };

        // Normalize indent width to a depth via the stack: pop levels wider
        // than or equal to us, then our depth is what remains.
        while let Some(&top) = stack.last() {
            if top >= indent_width {
                stack.pop();
            } else {
                break;
            }
        }
        let depth = stack.len();
        stack.push(indent_width);

        let done = matches!(marker, 'x' | 'X');
        let meta = extract_metadata(text);

        tasks.push(Task {
            line: idx + 1,
            depth,
            marker,
            done,
            text: text.to_string(),
            text_clean: meta.text_clean,
            pomodoros: meta.pomodoros,
            start: meta.start,
            due: meta.due,
            tags: meta.tags,
            locations: meta.locations,
            burner: meta.burner,
        });
    }

    tasks
}

/// If `raw_line` is a checkbox task, return `(indent_width, marker, text)`.
///
/// `indent_width` counts leading whitespace with a tab as 4 columns; `text` is
/// everything after the `]`, trimmed on the left.
fn parse_task_line(raw_line: &str) -> Option<(usize, char, &str)> {
    // Measure and strip leading whitespace.
    let mut indent_width = 0usize;
    let mut rest = "";
    for (i, ch) in raw_line.char_indices() {
        match ch {
            ' ' => indent_width += 1,
            '\t' => indent_width += 4,
            _ => {
                rest = raw_line.get(i..).unwrap_or("");
                break;
            }
        }
    }

    // Bullet: one of - * + followed by at least one space/tab.
    let bullet = rest.chars().next()?;
    if !matches!(bullet, '-' | '*' | '+') {
        return None;
    }
    let after_bullet = rest.get(1..)?;
    if !after_bullet.starts_with([' ', '\t']) {
        return None;
    }
    let after_bullet = after_bullet.trim_start();

    // Checkbox: `[`, one marker char, `]`.
    let bytes = after_bullet.as_bytes();
    if bytes.first() != Some(&b'[') || bytes.get(2) != Some(&b']') {
        return None;
    }
    let marker = *bytes.get(1)? as char;
    if !matches!(marker, ' ' | 'x' | 'X' | 'o' | 'O' | 'm' | 'M') {
        return None;
    }

    let text = after_bullet.get(3..).unwrap_or("").trim_start();
    Some((indent_width, marker, text))
}

struct Metadata {
    text_clean: String,
    pomodoros: Option<u32>,
    start: Option<String>,
    due: Option<String>,
    tags: Vec<String>,
    locations: Vec<String>,
    burner: Option<Burner>,
}

fn extract_metadata(text: &str) -> Metadata {
    // `due:` consumes to end of line (values are freeform, e.g. "next week
    // tuesday"). Split it off first; the head is where p:/start: live.
    let (head, due) = match find_token(text, "due:") {
        Some(pos) => {
            let due_val = text
                .get(pos + "due:".len()..)
                .unwrap_or("")
                .trim()
                .to_string();
            (
                text.get(..pos).unwrap_or(text),
                if due_val.is_empty() {
                    None
                } else {
                    Some(due_val)
                },
            )
        }
        None => (text, None),
    };

    // Tags are scanned across the WHOLE line so a `#fb` sitting after a `due:`
    // still yields a burner.
    let mut tags = Vec::new();
    for token in text.split_whitespace() {
        if let Some(tag) = token.strip_prefix('#') {
            let tag = tag.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            if !tag.is_empty() {
                tags.push(tag.to_string());
            }
        }
    }

    // Locations (`@context`) are scanned across the whole line like tags, and
    // lowercased so `@Work` and `@work` collapse into one context. Deduped
    // within a line. A task with no `@` keeps an empty list — the todo board's
    // location filter deliberately still shows those (relevant everywhere).
    let mut locations = Vec::new();
    for token in text.split_whitespace() {
        if let Some(loc) = token.strip_prefix('@') {
            let loc = loc.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            if !loc.is_empty() {
                let loc = loc.to_ascii_lowercase();
                if !locations.contains(&loc) {
                    locations.push(loc);
                }
            }
        }
    }

    let mut pomodoros = None;
    let mut start = None;
    let mut clean_tokens: Vec<&str> = Vec::new();
    for token in head.split_whitespace() {
        if let Some(n) = token.strip_prefix("p:").and_then(|s| s.parse::<u32>().ok()) {
            pomodoros = Some(n);
            continue;
        }
        if let Some(t) = token.strip_prefix("start:") {
            if is_hhmm(t) {
                start = Some(t.to_string());
                continue;
            }
        }
        if token.starts_with('#') || token.starts_with('@') {
            continue;
        }
        clean_tokens.push(token);
    }

    let burner = burner_from_tags(&tags);

    Metadata {
        text_clean: clean_tokens.join(" "),
        pomodoros,
        start,
        due,
        tags,
        locations,
        burner,
    }
}

/// Find `needle` at a whitespace/start boundary in `haystack`, returning its
/// byte offset. Avoids matching e.g. `due:` inside another word.
fn find_token(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .match_indices(needle)
        .find(|&(pos, _)| {
            pos == 0
                || haystack
                    .get(..pos)
                    .and_then(|s| s.chars().next_back())
                    .is_some_and(|c| c.is_whitespace())
        })
        .map(|(pos, _)| pos)
}

fn is_hhmm(s: &str) -> bool {
    let Some((h, m)) = s.split_once(':') else {
        return false;
    };
    !h.is_empty()
        && h.len() <= 2
        && h.chars().all(|c| c.is_ascii_digit())
        && m.len() == 2
        && m.chars().all(|c| c.is_ascii_digit())
}

fn burner_from_tags(tags: &[String]) -> Option<Burner> {
    // Highest-priority burner wins if several are present.
    let mut best: Option<Burner> = None;
    for tag in tags {
        let candidate = match tag.to_ascii_lowercase().as_str() {
            "fb" | "frontburner" => Some(Burner::Frontburner),
            "bb" | "backburner" => Some(Burner::Backburner),
            "fridge" => Some(Burner::Fridge),
            "oven" => Some(Burner::Oven),
            _ => None,
        };
        if let Some(c) = candidate {
            best = Some(match best {
                Some(existing) if burner_rank(existing) <= burner_rank(c) => existing,
                _ => c,
            });
        }
    }
    best
}

/// Base rank, lower = more urgent.
fn burner_rank(b: Burner) -> u8 {
    match b {
        Burner::Frontburner => 0,
        Burner::Backburner => 1,
        Burner::Fridge => 2,
        Burner::Oven => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(line: &str) -> Task {
        let tasks = parse_tasks(line);
        assert_eq!(tasks.len(), 1, "expected exactly one task in {line:?}");
        tasks.into_iter().next().unwrap()
    }

    #[test]
    fn basic_open_and_done() {
        let open = parse_one("- [ ] write the parser");
        assert!(!open.done);
        assert_eq!(open.marker, ' ');
        assert_eq!(open.text_clean, "write the parser");

        let done = parse_one("- [x] shipped it");
        assert!(done.done);
        let done_upper = parse_one("- [X] shipped it too");
        assert!(done_upper.done);
    }

    #[test]
    fn carry_over_and_legacy_markers_are_open() {
        assert!(!parse_one("- [m] get bot to work").done);
        assert!(!parse_one("- [o] learn rust").done);
        assert_eq!(parse_one("- [m] get bot to work").marker, 'm');
    }

    #[test]
    fn star_bullet_and_double_space() {
        assert_eq!(
            parse_one("* [ ] chunking rework").text_clean,
            "chunking rework"
        );
        assert_eq!(parse_one("-  [ ] double space").text_clean, "double space");
        assert_eq!(parse_one("+ [ ] plus bullet").text_clean, "plus bullet");
    }

    #[test]
    fn pomodoros_start_and_tags() {
        let t = parse_one("- [ ] contact businesses start:09:40 p:2 #fb");
        assert_eq!(t.pomodoros, Some(2));
        assert_eq!(t.start.as_deref(), Some("09:40"));
        assert_eq!(t.tags, vec!["fb".to_string()]);
        assert_eq!(t.burner, Some(Burner::Frontburner));
        assert_eq!(t.text_clean, "contact businesses");
    }

    #[test]
    fn locations_are_lowercased_stripped_and_deduped() {
        let t = parse_one("- [ ] buy milk @Store p:1 @store #fb");
        // Lowercased and deduped despite the mixed-case repeat.
        assert_eq!(t.locations, vec!["store".to_string()]);
        // Stripped from the display text (like #tags), other metadata intact.
        assert_eq!(t.text_clean, "buy milk");
        assert_eq!(t.pomodoros, Some(1));
        assert_eq!(t.tags, vec!["fb".to_string()]);

        // Multiple distinct contexts, and one sitting after a `due:` value is
        // still found (whole-line scan, same as tags).
        let t2 = parse_one("- [ ] errands @home due:today @work");
        assert_eq!(t2.locations, vec!["home".to_string(), "work".to_string()]);
        assert_eq!(t2.due.as_deref(), Some("today @work"));

        // No `@` → empty list (so the board's location filter still shows it).
        assert!(parse_one("- [ ] think about it").locations.is_empty());
    }

    #[test]
    fn due_is_freeform_to_end_of_line() {
        let t = parse_one("- [ ] discuss timeline due:next week tuesday");
        assert_eq!(t.due.as_deref(), Some("next week tuesday"));
        assert_eq!(t.text_clean, "discuss timeline");

        let t2 = parse_one("- [ ] finnish cli todo due:today");
        assert_eq!(t2.due.as_deref(), Some("today"));
    }

    #[test]
    fn tag_after_due_still_sets_burner() {
        // `#fb` sits after the freeform due value but must still be found.
        let t = parse_one("- [ ] renew domain due:before it expires #fridge");
        assert_eq!(t.burner, Some(Burner::Fridge));
        assert!(t.tags.iter().any(|tag| tag == "fridge"));
    }

    #[test]
    fn burner_long_forms_and_priority() {
        assert_eq!(
            parse_one("- [ ] x #backburner").burner,
            Some(Burner::Backburner)
        );
        // Frontburner wins over oven when both present.
        assert_eq!(
            parse_one("- [ ] x #oven #fb").burner,
            Some(Burner::Frontburner)
        );
        assert_eq!(parse_one("- [ ] no tags here").burner, None);
    }

    #[test]
    fn nesting_depth_from_tabs_and_spaces() {
        let doc = "- [ ] parent\n\t- [ ] child\n\t\t- [ ] grandchild\n- [ ] parent2";
        let tasks = parse_tasks(doc);
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].depth, 0);
        assert_eq!(tasks[1].depth, 1);
        assert_eq!(tasks[2].depth, 2);
        assert_eq!(tasks[3].depth, 0);
        assert_eq!(tasks[3].line, 4);
    }

    #[test]
    fn line_numbers_are_one_based_and_skip_non_tasks() {
        let doc = "---\nplan: true\n---\n- [ ] first\nsome prose\n- [x] second";
        let tasks = parse_tasks(doc);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].line, 4);
        assert_eq!(tasks[1].line, 6);
    }

    #[test]
    fn non_task_lines_ignored() {
        assert!(parse_tasks("# heading").is_empty());
        assert!(parse_tasks("- just a bullet").is_empty());
        assert!(parse_tasks("- [[wiki link]] not a task").is_empty());
        assert!(parse_tasks("plain text").is_empty());
        assert!(parse_tasks("").is_empty());
    }

    #[test]
    fn p_token_must_be_numeric() {
        let t = parse_one("- [ ] talk to p:person about p:3");
        // "p:person" is not numeric and stays in the text; "p:3" is pomodoros.
        assert_eq!(t.pomodoros, Some(3));
        assert!(t.text_clean.contains("p:person"));
    }
}
