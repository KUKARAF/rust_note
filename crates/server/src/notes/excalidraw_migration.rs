//! One-time migration: Obsidian excalidraw wrapper files → bare drawings.
//!
//! Historically the vault's drawings were written by the Obsidian excalidraw
//! plugin as `Something.excalidraw.md`: a markdown wrapper (frontmatter,
//! `# Text Elements`, `# Drawing`/`## Drawing`) around the scene JSON, either
//! verbatim in a ```json fence or LZ-String-compressed in a
//! ```compressed-json fence (base64 chunked at 256 chars with blank lines).
//!
//! The app now stores drawings as bare `Something.excalidraw` files holding
//! plain pretty-printed scene JSON — the format the embedded Excalidraw
//! editor round-trips natively (Obsidian compatibility deliberately
//! abandoned; its "compatibility mode" can still open bare files). The
//! id<->path mapping is a pure extension rule (`fs_store::note_id_to_path`)
//! whose bijection REQUIRES that `*.excalidraw.md` wrappers never appear in
//! `list_notes` output — this migration is what makes that true on disk.
//!
//! Runs at every boot (and from `POST /api/reindex`); converges to a no-op
//! once no wrappers remain. Per wrapper `p` with bare target `t`:
//!   1. extract + validate the scene JSON (raw or LZ-String-decompressed),
//!      re-emit pretty-printed;
//!   2. if `t` already exists: identical content → just delete the wrapper;
//!      different content → skip with a warning (never clobber);
//!   3. otherwise write `t` (bot commit) then delete `p` (bot commit) — two
//!      commits via the existing, well-tested primitives; a crash between
//!      them is healed by rule 2 on the next run;
//!   4. extraction failure → rename to `{stem}.excalidraw-broken.md`
//!      (content untouched) so the note stays visible and text-editable
//!      instead of colliding with the bare id space.
//!
//! Every step is a revertible git commit authored by the bot identity.

use crate::notes::repo::NotesRepo;

/// Outcome counts for one migration pass (all zero ⇒ nothing to do).
#[derive(Debug, Default, Clone, Copy)]
pub struct MigrationReport {
    pub converted: usize,
    pub broken_renamed: usize,
    pub skipped_conflicts: usize,
}

impl MigrationReport {
    pub fn is_noop(&self) -> bool {
        self.converted == 0 && self.broken_renamed == 0 && self.skipped_conflicts == 0
    }
}

/// Convert all `*.excalidraw.md` wrappers in the repo. Never fails the boot
/// for a single bad file — per-file problems are warned and counted.
pub async fn migrate(repo: &NotesRepo) -> anyhow::Result<MigrationReport> {
    let wrappers = repo.list_excalidraw_wrappers()?;
    let mut report = MigrationReport::default();

    for wrapper_path in wrappers {
        let Some(target_path) = wrapper_path.strip_suffix(".md").map(str::to_string) else {
            continue; // unreachable by construction of the listing
        };

        let Some(content) = repo.read_file(&wrapper_path)? else {
            continue; // raced away (e.g. Syncthing removal) — nothing to do
        };

        match extract_scene_json(&content) {
            Some(scene_json) => {
                if let Some(existing) = repo.read_file(&target_path)? {
                    if existing == scene_json {
                        // Crash-recovery / rerun path: bare target already
                        // written, only the wrapper deletion is left.
                        repo.delete_and_commit_as_bot(
                            &wrapper_path,
                            &format!("migrate excalidraw: remove wrapper {wrapper_path}"),
                        )
                        .await?;
                        report.converted += 1;
                    } else {
                        tracing::warn!(
                            wrapper = wrapper_path,
                            target = target_path,
                            "excalidraw migration: bare target exists with different \
                             content — leaving both untouched"
                        );
                        report.skipped_conflicts += 1;
                    }
                    continue;
                }

                repo.write_and_commit_as_bot(
                    &target_path,
                    &scene_json,
                    &format!("migrate excalidraw: {wrapper_path} -> {target_path}"),
                )
                .await?;
                repo.delete_and_commit_as_bot(
                    &wrapper_path,
                    &format!("migrate excalidraw: remove wrapper {wrapper_path}"),
                )
                .await?;
                report.converted += 1;
            }
            None => {
                // Unparseable: rename out of the colliding `.excalidraw.md`
                // shape so the walk lists it again (as an ordinary markdown
                // note the user can repair in the text editor).
                let broken_path = format!("{target_path}-broken.md");
                if repo.read_file(&broken_path)?.is_some() {
                    tracing::warn!(
                        wrapper = wrapper_path,
                        broken = broken_path,
                        "excalidraw migration: broken-rename target already exists — skipping"
                    );
                    report.skipped_conflicts += 1;
                    continue;
                }
                tracing::warn!(
                    wrapper = wrapper_path,
                    broken = broken_path,
                    "excalidraw migration: could not extract a scene — renaming"
                );
                repo.write_and_commit_as_bot(
                    &broken_path,
                    &content,
                    &format!("migrate excalidraw: rename unparseable {wrapper_path}"),
                )
                .await?;
                repo.delete_and_commit_as_bot(
                    &wrapper_path,
                    &format!("migrate excalidraw: remove unparseable wrapper {wrapper_path}"),
                )
                .await?;
                report.broken_renamed += 1;
            }
        }
    }

    Ok(report)
}

/// Extract the scene JSON from a wrapper's content: find the
/// `# Drawing`/`## Drawing` heading, then a ```json or ```compressed-json
/// fence, decompress if needed, validate, and pretty-print (2-space, matching
/// Excalidraw's own `serializeAsJSON` style to keep future diffs minimal).
/// Returns `None` on any structural or parse failure.
///
/// Line-based on purpose (no regex dependency); tolerant of CRLF and of the
/// plugin's 256-char base64 chunking with blank lines inside the fence.
fn extract_scene_json(content: &str) -> Option<String> {
    let mut lines = content.lines().map(|l| l.trim_end_matches('\r'));

    // Locate the Drawing heading.
    lines.by_ref().find(|line| {
        let t = line.trim();
        t == "# Drawing" || t == "## Drawing"
    })?;

    // Locate the opening fence (blank lines may intervene).
    let compressed = loop {
        let line = lines.next()?;
        match line.trim() {
            "```json" => break false,
            "```compressed-json" => break true,
            "" => continue,
            _ => return None,
        }
    };

    // Collect the fence body up to the closing ``` (the plugin appends `%%`
    // after the fence — irrelevant here since we stop at the fence).
    let mut body = String::new();
    loop {
        let line = lines.next()?;
        if line.trim_start().starts_with("```") {
            break;
        }
        body.push_str(line);
        body.push('\n');
    }

    let json_text = if compressed {
        let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        let decompressed = lz_str::decompress_from_base64(&compact)?;
        String::from_utf16(&decompressed).ok()?
    } else {
        body
    };

    let scene: serde_json::Value = serde_json::from_str(&json_text).ok()?;
    if !scene
        .get("elements")
        .is_some_and(serde_json::Value::is_array)
    {
        return None;
    }

    let pretty = serde_json::to_string_pretty(&scene).ok()?;
    Some(format!("{pretty}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENE: &str = r##"{"type":"excalidraw","version":2,"source":"test","elements":[{"type":"rectangle","id":"r1"}],"appState":{"viewBackgroundColor":"#ffffff"},"files":{}}"##;

    fn raw_wrapper(scene: &str) -> String {
        format!(
            "---\nexcalidraw-plugin: parsed\ntags: [excalidraw]\n---\n\n\
             # Text Elements\nSIGN IN ^abc123\n%%\n# Drawing\n```json\n{scene}\n```\n%%"
        )
    }

    fn compressed_wrapper(scene: &str) -> String {
        let compressed = lz_str::compress_to_base64(scene);
        // Mimic the plugin's 256-char chunking with blank-line separators.
        let chunked: Vec<String> = compressed
            .chars()
            .collect::<Vec<_>>()
            .chunks(256)
            .map(|c| c.iter().collect())
            .collect();
        format!(
            "---\nexcalidraw-plugin: parsed\n---\n\n# Excalidraw Data\n\n\
             ## Text Elements\n\n%%\n## Drawing\n```compressed-json\n{}\n```\n%%",
            chunked.join("\n\n")
        )
    }

    fn expected_pretty() -> String {
        let v: serde_json::Value = serde_json::from_str(SCENE).unwrap();
        format!("{}\n", serde_json::to_string_pretty(&v).unwrap())
    }

    #[test]
    fn extracts_raw_json_wrapper() {
        assert_eq!(
            extract_scene_json(&raw_wrapper(SCENE)),
            Some(expected_pretty())
        );
    }

    #[test]
    fn extracts_compressed_wrapper() {
        assert_eq!(
            extract_scene_json(&compressed_wrapper(SCENE)),
            Some(expected_pretty())
        );
    }

    #[test]
    fn extracts_crlf_wrapper() {
        let crlf = raw_wrapper(SCENE).replace('\n', "\r\n");
        assert_eq!(extract_scene_json(&crlf), Some(expected_pretty()));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(extract_scene_json("no drawing section at all"), None);
        assert_eq!(
            extract_scene_json("# Drawing\n```json\nnot json\n```"),
            None
        );
        // Valid JSON but not a scene (no elements array).
        assert_eq!(
            extract_scene_json("# Drawing\n```json\n{\"foo\": 1}\n```"),
            None
        );
        // Unknown fence type.
        assert_eq!(extract_scene_json("# Drawing\n```yaml\nfoo: 1\n```"), None);
    }

    /// Opt-in rehearsal against REAL vault wrapper files: set
    /// `EXCALIDRAW_FIXTURE_DIR` to a directory containing copies of
    /// `*.excalidraw.md` files and every one of them must yield a scene.
    /// Skipped (trivially passes) when the env var is unset, so CI is
    /// unaffected; run locally before deploying the migration.
    #[test]
    fn extracts_real_vault_fixtures_when_provided() {
        let Ok(dir) = std::env::var("EXCALIDRAW_FIXTURE_DIR") else {
            return;
        };
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.to_string_lossy().ends_with(".excalidraw.md") {
                continue;
            }
            let content = std::fs::read_to_string(&path).unwrap();
            if content.trim().is_empty() {
                continue;
            }
            assert!(
                extract_scene_json(&content).is_some(),
                "failed to extract scene from real fixture {path:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "no fixtures found in {dir}");
        eprintln!("extracted {checked} real vault fixtures OK");
    }

    #[tokio::test]
    async fn migrates_wrappers_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit_as_bot("a.excalidraw.md", &raw_wrapper(SCENE), "seed")
            .await
            .unwrap();
        repo.write_and_commit_as_bot("sub/b.excalidraw.md", &compressed_wrapper(SCENE), "seed")
            .await
            .unwrap();
        repo.write_and_commit_as_bot("broken.excalidraw.md", "not a drawing at all", "seed")
            .await
            .unwrap();
        repo.write_and_commit_as_bot("normal.md", "# Normal\n", "seed")
            .await
            .unwrap();

        let report = migrate(&repo).await.unwrap();
        assert_eq!(report.converted, 2);
        assert_eq!(report.broken_renamed, 1);
        assert_eq!(report.skipped_conflicts, 0);

        // Bare files exist with the pretty scene; wrappers are gone.
        assert_eq!(
            repo.read_file("a.excalidraw").unwrap(),
            Some(expected_pretty())
        );
        assert_eq!(
            repo.read_file("sub/b.excalidraw").unwrap(),
            Some(expected_pretty())
        );
        assert_eq!(repo.read_file("a.excalidraw.md").unwrap(), None);
        assert_eq!(repo.read_file("sub/b.excalidraw.md").unwrap(), None);
        // Broken wrapper renamed, content preserved.
        assert_eq!(repo.read_file("broken.excalidraw.md").unwrap(), None);
        assert_eq!(
            repo.read_file("broken.excalidraw-broken.md").unwrap(),
            Some("not a drawing at all".to_string())
        );

        // The walk now lists exactly the migrated shapes.
        let listed = repo.list_notes().unwrap();
        assert!(listed.contains(&"a.excalidraw".to_string()));
        assert!(listed.contains(&"sub/b.excalidraw".to_string()));
        assert!(listed.contains(&"broken.excalidraw-broken.md".to_string()));
        assert!(listed.contains(&"normal.md".to_string()));
        assert!(!listed.iter().any(|p| p.ends_with(".excalidraw.md")));

        // Second run: no-op.
        let again = migrate(&repo).await.unwrap();
        assert!(again.is_noop());
    }

    #[tokio::test]
    async fn conflicting_bare_target_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit_as_bot("c.excalidraw.md", &raw_wrapper(SCENE), "seed")
            .await
            .unwrap();
        repo.write_and_commit_as_bot("c.excalidraw", "{\"different\": true}", "seed")
            .await
            .unwrap();

        let report = migrate(&repo).await.unwrap();
        assert_eq!(report.skipped_conflicts, 1);
        assert_eq!(report.converted, 0);
        // Both files untouched.
        assert!(repo.read_file("c.excalidraw.md").unwrap().is_some());
        assert_eq!(
            repo.read_file("c.excalidraw").unwrap(),
            Some("{\"different\": true}".to_string())
        );
    }

    #[tokio::test]
    async fn crash_recovery_identical_target_completes_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit_as_bot("d.excalidraw.md", &raw_wrapper(SCENE), "seed")
            .await
            .unwrap();
        // Simulate a crash after the bare write, before the wrapper delete.
        repo.write_and_commit_as_bot("d.excalidraw", &expected_pretty(), "seed")
            .await
            .unwrap();

        let report = migrate(&repo).await.unwrap();
        assert_eq!(report.converted, 1);
        assert_eq!(repo.read_file("d.excalidraw.md").unwrap(), None);
        assert_eq!(
            repo.read_file("d.excalidraw").unwrap(),
            Some(expected_pretty())
        );
    }
}
