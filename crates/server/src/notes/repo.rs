//! Git-backed notes repository (via `git2`).
//!
//! [`NotesRepo`] wraps a `git2::Repository` rooted at `Config::notes_repo_path`
//! and exposes the small set of operations the rest of the server needs:
//! reading a note's current content, writing+committing a new version,
//! listing all notes, and fetching a file's commit history.
//!
//! `git2::Repository` is not `Sync` (and its calls are blocking, synchronous
//! I/O), so concurrent access from async Axum handlers is mediated by an
//! `Arc<tokio::sync::Mutex<git2::Repository>>`. All git operations are
//! serialized through this mutex, which matches the single-writer model
//! called for in the project plan: every write is a fast-forward commit on
//! top of the current HEAD, so there's no merge-conflict handling to worry
//! about. Callers that want to avoid blocking the async runtime thread for
//! longer operations can wrap calls in `tokio::task::spawn_blocking`
//! themselves; for the note-file sizes this app expects, holding the mutex
//! for the (short) duration of a git call is acceptable.

use anyhow::{anyhow, bail, Context};
use git2::{Repository, Signature};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Fallback signature used for automated commits that aren't attributed to
/// a specific user.
const BOT_NAME: &str = "rust_note bot";
const BOT_EMAIL: &str = "rustnote@localhost";

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    pub message: String,
    pub timestamp: String,
}

#[derive(Clone)]
pub struct NotesRepo {
    inner: Arc<Mutex<Repository>>,
    root: PathBuf,
}

impl NotesRepo {
    /// Open the git repository at `path`, initializing a fresh one if none
    /// exists yet. Creates `path` itself if missing.
    pub fn open_or_init(path: &str) -> anyhow::Result<Self> {
        std::fs::create_dir_all(path).with_context(|| format!("creating notes repo dir {path}"))?;

        let repo = match Repository::open(path) {
            Ok(repo) => repo,
            Err(_) => Repository::init(path)
                .with_context(|| format!("initializing git repo at {path}"))?,
        };

        // Canonicalize the root once up front so `read_file`'s traversal
        // check has a stable, symlink-resolved base to compare against.
        let root = std::fs::canonicalize(path)
            .with_context(|| format!("canonicalizing notes repo path {path}"))?;

        Ok(Self {
            inner: Arc::new(Mutex::new(repo)),
            root,
        })
    }

    /// Resolve `rel_path` against the repo root, rejecting any path that
    /// escapes it (`..` components, absolute paths, symlink shenanigans).
    ///
    /// If `create_dirs` is true, the parent directory is created (via
    /// `create_dir_all`) if it doesn't already exist, so the path can be
    /// canonicalized even before anything has been written there — this is
    /// needed for writes. If `create_dirs` is false, this is a read-only
    /// resolve: a missing parent directory means the file itself can't
    /// exist, so `Ok(None)` is returned instead of creating anything on
    /// disk as a side effect of a read.
    fn resolve_safe(&self, rel_path: &str, create_dirs: bool) -> anyhow::Result<Option<PathBuf>> {
        if Path::new(rel_path).is_absolute() {
            bail!("path must be relative: {rel_path}");
        }
        if rel_path
            .split(['/', '\\'])
            .any(|component| component == "..")
        {
            bail!("path traversal is not allowed: {rel_path}");
        }

        let joined = self.root.join(rel_path);

        // The file may not exist yet (e.g. on write), so canonicalize the
        // parent directory instead and re-append the file name.
        let (dir, file_name) = match joined.parent() {
            Some(parent) => (parent.to_path_buf(), joined.file_name()),
            None => bail!("invalid path: {rel_path}"),
        };
        let file_name = file_name.ok_or_else(|| anyhow!("invalid path: {rel_path}"))?;

        if create_dirs {
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating parent dir for {rel_path}"))?;
        } else if !dir.exists() {
            // Nothing has ever been written under this parent directory, so
            // the file can't exist either. Report "not found" without
            // touching the filesystem.
            return Ok(None);
        }

        let canonical_dir = std::fs::canonicalize(&dir)
            .with_context(|| format!("canonicalizing {}", dir.display()))?;

        if !canonical_dir.starts_with(&self.root) {
            bail!("path escapes notes repo root: {rel_path}");
        }

        Ok(Some(canonical_dir.join(file_name)))
    }

    /// Read the current working-tree content of a note. Returns `None` if
    /// the file doesn't exist.
    pub fn read_file(&self, rel_path: &str) -> anyhow::Result<Option<String>> {
        let abs = match self.resolve_safe(rel_path, false)? {
            Some(abs) => abs,
            None => return Ok(None),
        };
        match std::fs::read_to_string(&abs) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {rel_path}")),
        }
    }

    /// Convenience wrapper for automated writes with no specific user
    /// attribution, using the fixed bot signature.
    pub async fn write_and_commit_as_bot(
        &self,
        rel_path: &str,
        content: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        self.write_and_commit(rel_path, content, BOT_NAME, BOT_EMAIL, message)
            .await
    }

    /// Write `content` to `rel_path` and create a commit authored by
    /// (`author_name`, `author_email`) with the given `message`.
    pub async fn write_and_commit(
        &self,
        rel_path: &str,
        content: &str,
        author_name: &str,
        author_email: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let abs = self.resolve_safe(rel_path, true)?.ok_or_else(|| {
            anyhow::anyhow!(
                "internal error: resolve_safe returned None for {rel_path} with create_dirs=true \
                 (it should always return Some in that mode)"
            )
        })?;

        // Acquire the repo mutex *before* touching the working tree so that
        // the filesystem write and the git staging/commit that reads it back
        // happen as a single atomic unit. Otherwise a concurrent writer to
        // the same path could overwrite the file on disk between our write
        // and our commit, causing us to stage and commit someone else's
        // content under our message (or vice versa), silently losing data.
        let repo = self.inner.lock().await;

        // Remember what was on disk before we touch it (if anything), so we
        // can roll the working tree back if a later git step fails. Without
        // this, a transient git failure (e.g. a stale index.lock) would
        // leave the working tree holding uncommitted content forever, since
        // reads go straight to the working tree rather than HEAD.
        let previous_content = match std::fs::read(&abs) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {rel_path} before write")),
        };

        // DLI-3: an external tool (Obsidian, Syncthing, a manual `vim`) may
        // have written this file WITHOUT committing it. Comparing git SHAs
        // alone can't see that, so a stale write would silently clobber it
        // with no git trace. Before overwriting, capture any such uncommitted
        // on-disk change as its own commit so the external edit is preserved
        // in history. (A collab flush's authoritative overwrite goes through
        // here too, so an external edit made while a room was live is
        // checkpointed rather than lost — the safe behavior.)
        checkpoint_external_change(&repo, rel_path, previous_content.as_deref())
            .with_context(|| format!("checkpointing external change to {rel_path}"))?;

        std::fs::write(&abs, content).with_context(|| format!("writing {rel_path}"))?;

        let result: anyhow::Result<()> = (|| {
            // Build the commit tree from HEAD's tree plus *only* this one
            // changed path, rather than from the whole on-disk index. This
            // means (a) we never stage anything to the on-disk index before
            // committing, so a failure mid-commit can't leave a poisoned blob
            // staged for the next unrelated note to sweep up (DLI-2); and (b)
            // any files the user `git add`ed out-of-band (or an in-progress
            // merge) are never swept into the app's "update foo.md" commit
            // (DLI-4).
            let blob_oid = repo
                .blob(content.as_bytes())
                .context("hashing note content")?;
            let head_tree = head_tree(&repo)?;
            let components = path_components(rel_path)?;
            let tree_oid =
                build_tree_with_path(&repo, head_tree.as_ref(), &components, Some(blob_oid))?;
            let tree = repo
                .find_tree(tree_oid)
                .context("looking up written tree")?;

            let signature = build_signature(author_name, author_email)?;

            let parent_commit = head_commit(&repo)?;
            let parents: Vec<&git2::Commit> = parent_commit.iter().collect();

            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents,
            )
            .context("creating commit")?;

            Ok(())
        })();

        if let Err(err) = result {
            // Roll the working tree back to what it held before this call,
            // so a failed commit never leaves uncommitted content visible
            // to readers (which read the working tree, not HEAD).
            let rollback = match &previous_content {
                Some(bytes) => std::fs::write(&abs, bytes)
                    .with_context(|| format!("rolling back {rel_path} after failed commit")),
                None => std::fs::remove_file(&abs)
                    .with_context(|| format!("removing {rel_path} after failed commit")),
            };
            if let Err(rollback_err) = rollback {
                return Err(err.context(format!("and rollback also failed: {rollback_err:#}")));
            }
            return Err(err);
        }

        // Bring the on-disk index entry for just this path in line with the
        // commit we just made, so the user's `git status` stays clean for
        // this note. Best-effort: the commit is already durable, so an index
        // hiccup here is logged, not fatal.
        sync_index_entry(&repo, rel_path, true);

        Ok(())
    }

    /// Remove `rel_path` from the working tree and index, and create a
    /// commit recording the deletion. Follows the same pattern as
    /// `write_and_commit`. No-ops the filesystem removal (but still
    /// commits, if the path was tracked) if the file is already gone.
    pub async fn delete_and_commit(
        &self,
        rel_path: &str,
        author_name: &str,
        author_email: &str,
        message: &str,
    ) -> anyhow::Result<()> {
        let abs = self.resolve_safe(rel_path, false)?;

        // See write_and_commit: hold the repo mutex across the filesystem
        // change and the git staging/commit so they form a single atomic
        // unit with respect to concurrent writers/deleters of the same path.
        let repo = self.inner.lock().await;

        // Snapshot the bytes before removing so we can restore them if a
        // later git step fails (DLI-7): unlike write_and_commit, the old
        // delete path removed the file first and had no rollback, so a
        // transient git failure left the note gone from the working tree
        // while HEAD still had it.
        let previous_content: Option<Vec<u8>> = match &abs {
            Some(abs) => match std::fs::read(abs) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(e).with_context(|| format!("reading {rel_path} before delete"))
                }
            },
            None => None,
        };

        // Whether HEAD actually tracks this path — i.e. there is a committed
        // version whose deletion is worth recording.
        let tracked_in_head = head_blob_bytes(&repo, rel_path)?.is_some();

        // `resolve_safe` returns `None` when the parent directory doesn't
        // exist at all, which means the file is already gone.
        if let Some(abs) = &abs {
            match std::fs::remove_file(abs) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("deleting {rel_path}")),
            }
        }

        // Nothing tracked in git to record a deletion against: the
        // working-tree removal above (if any) is all there is to do.
        // Committing here would just be an empty no-op commit (DLI-7).
        if !tracked_in_head {
            return Ok(());
        }

        let result: anyhow::Result<()> = (|| {
            // Build the commit tree from HEAD minus this one path (see
            // write_and_commit for why we don't touch the on-disk index).
            let head_tree = head_tree(&repo)?;
            let components = path_components(rel_path)?;
            let tree_oid = build_tree_with_path(&repo, head_tree.as_ref(), &components, None)?;
            let tree = repo
                .find_tree(tree_oid)
                .context("looking up written tree")?;

            let signature = build_signature(author_name, author_email)?;

            let parent_commit = head_commit(&repo)?;
            let parents: Vec<&git2::Commit> = parent_commit.iter().collect();

            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents,
            )
            .context("creating commit")?;

            Ok(())
        })();

        if let Err(err) = result {
            // Restore the file we removed so a failed delete doesn't lose the
            // note (HEAD still has it, but the working tree — which reads
            // serve from — would not without this).
            if let (Some(abs), Some(bytes)) = (&abs, &previous_content) {
                if let Err(rollback_err) = std::fs::write(abs, bytes)
                    .with_context(|| format!("restoring {rel_path} after failed delete"))
                {
                    return Err(err.context(format!("and rollback also failed: {rollback_err:#}")));
                }
            }
            return Err(err);
        }

        sync_index_entry(&repo, rel_path, false);

        Ok(())
    }

    /// List all `.md` files in the working tree (excluding `.git/`),
    /// relative to the repo root.
    pub fn list_notes(&self) -> anyhow::Result<Vec<String>> {
        let mut notes = Vec::new();
        Self::walk_dir(&self.root, &self.root, &mut notes)?;
        notes.sort();
        Ok(notes)
    }

    fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading dir {dir:?}"))? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            if file_name == ".git" {
                continue;
            }

            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                Self::walk_dir(root, &path, out)?;
            } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                let rel = path
                    .strip_prefix(root)
                    .context("stripping repo root prefix")?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
        Ok(())
    }

    /// Commit history (across all branches from HEAD) touching `rel_path`,
    /// newest first.
    ///
    /// This walks the full commit graph and diffs each commit against its
    /// (first) parent, checking whether `rel_path` appears in the diff. It
    /// is not the most efficient approach but is simple and correct for the
    /// repo sizes this app expects in v1.
    ///
    /// Note identity: a path can be deleted and later recreated as a
    /// logically distinct note (the app treats "create after delete" as a
    /// brand new note, not a continuation). To avoid leaking a prior,
    /// deleted incarnation's commits/timestamps/content into the current
    /// note's history, the walk stops as soon as it includes the commit
    /// that (re)created `rel_path` (a diff status of `Added`, or a rename
    /// into `rel_path`) — anything older than that belongs to a previous
    /// incarnation of the path and is intentionally excluded.
    pub async fn history(&self, rel_path: &str) -> anyhow::Result<Vec<CommitInfo>> {
        let repo = self.inner.lock().await;

        if repo.head().is_err() {
            return Ok(Vec::new());
        }

        let mut revwalk = repo.revwalk().context("creating revwalk")?;
        revwalk.push_head().context("pushing HEAD to revwalk")?;

        let mut history = Vec::new();
        for oid in revwalk {
            let oid = oid?;
            let commit = repo.find_commit(oid)?;
            let tree = commit.tree()?;

            let parent_tree = if commit.parent_count() > 0 {
                Some(commit.parent(0)?.tree()?)
            } else {
                None
            };

            // Pathspec-limit the diff to just `rel_path` so each commit's
            // diff only inspects this one file rather than diffing the whole
            // tree against its parent (dos-01/dos-02): the single-note history
            // walk is O(commits) cheap-diffs instead of O(commits × tree
            // size).
            let mut diff_opts = git2::DiffOptions::new();
            diff_opts.pathspec(rel_path);
            let diff =
                repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut diff_opts))?;

            let mut touches_path = false;
            let mut is_creation = false;
            for delta in diff.deltas() {
                let matches_side = |file: git2::DiffFile| {
                    file.path()
                        .map(|p| p.to_string_lossy() == rel_path)
                        .unwrap_or(false)
                };
                if matches_side(delta.old_file()) || matches_side(delta.new_file()) {
                    touches_path = true;
                    if matches!(delta.status(), git2::Delta::Added | git2::Delta::Renamed) {
                        is_creation = true;
                    }
                }
            }

            if touches_path {
                let author = commit.author();
                let name = author.name().unwrap_or("unknown");
                let email = author.email().unwrap_or("unknown");
                let time = commit.time();
                let timestamp = time_to_rfc3339(time);

                history.push(CommitInfo {
                    sha: commit.id().to_string(),
                    author: format!("{name} <{email}>"),
                    message: commit.message().unwrap_or("").trim_end().to_string(),
                    timestamp,
                });

                // Stop once we've included the commit that (re)created this
                // path: anything further back belongs to a prior, distinct
                // incarnation of the same path and must not be attributed
                // to the current note.
                if is_creation {
                    break;
                }
            }
        }

        Ok(history)
    }
}

/// Split a repo-relative path into its non-empty `/`-separated components.
fn path_components(rel_path: &str) -> anyhow::Result<Vec<&str>> {
    let components: Vec<&str> = rel_path.split('/').filter(|c| !c.is_empty()).collect();
    if components.is_empty() {
        bail!("invalid empty path");
    }
    Ok(components)
}

/// The commit at HEAD, or `None` if the repo has no commits yet.
fn head_commit(repo: &Repository) -> anyhow::Result<Option<git2::Commit<'_>>> {
    match repo.head() {
        Ok(head) => Ok(Some(
            head.peel_to_commit().context("resolving HEAD commit")?,
        )),
        Err(_) => Ok(None),
    }
}

/// HEAD's tree, or `None` if the repo has no commits yet.
fn head_tree(repo: &Repository) -> anyhow::Result<Option<git2::Tree<'_>>> {
    match head_commit(repo)? {
        Some(commit) => Ok(Some(commit.tree().context("resolving HEAD tree")?)),
        None => Ok(None),
    }
}

/// The committed (HEAD) blob bytes for `rel_path`, or `None` if the path is
/// untracked / the repo is empty.
fn head_blob_bytes(repo: &Repository, rel_path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let Some(tree) = head_tree(repo)? else {
        return Ok(None);
    };
    match tree.get_path(Path::new(rel_path)) {
        Ok(entry) => {
            let obj = entry
                .to_object(repo)
                .context("resolving HEAD tree entry to object")?;
            Ok(obj.as_blob().map(|b| b.content().to_vec()))
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e).context("looking up path in HEAD tree")),
    }
}

/// Build a new git tree from `base` (HEAD's tree, or `None` for an empty
/// repo) with a single path set to `blob_oid` (or removed when `blob_oid` is
/// `None`), rebuilding only the subtrees along that path. Every other entry
/// is carried over from `base` untouched, so the resulting commit is scoped
/// to exactly the one changed path regardless of the on-disk index.
fn build_tree_with_path(
    repo: &Repository,
    base: Option<&git2::Tree>,
    components: &[&str],
    blob_oid: Option<git2::Oid>,
) -> anyhow::Result<git2::Oid> {
    let mut builder = repo.treebuilder(base).context("creating tree builder")?;
    let (&name, rest) = components
        .split_first()
        .context("internal error: build_tree_with_path called with empty path components")?;

    if rest.is_empty() {
        match blob_oid {
            Some(oid) => {
                builder
                    .insert(name, oid, 0o100644)
                    .context("inserting blob into tree")?;
            }
            None => {
                if builder.get(name).context("reading tree entry")?.is_some() {
                    builder.remove(name).context("removing tree entry")?;
                }
            }
        }
    } else {
        let sub_base = match builder.get(name).context("reading tree entry")? {
            Some(entry) if entry.kind() == Some(git2::ObjectType::Tree) => {
                Some(repo.find_tree(entry.id()).context("resolving subtree")?)
            }
            _ => None,
        };
        let sub_oid = build_tree_with_path(repo, sub_base.as_ref(), rest, blob_oid)?;
        let sub_tree = repo
            .find_tree(sub_oid)
            .context("resolving rebuilt subtree")?;
        if sub_tree.is_empty() {
            // The subtree became empty (last file under it removed); drop the
            // directory entry entirely rather than committing an empty tree,
            // matching how git prunes empty directories.
            if builder.get(name).context("reading tree entry")?.is_some() {
                builder.remove(name).context("removing emptied subtree")?;
            }
        } else {
            builder
                .insert(name, sub_oid, 0o040000)
                .context("inserting subtree")?;
        }
    }

    builder.write().context("writing tree")
}

/// Sanitize a user-derived author name/email field so `Signature::now`
/// accepts it. git rejects `<`, `>`, and newlines in these fields (and other
/// control characters are undesirable), so an OIDC display name like
/// "Foo <bar>" would otherwise make every commit fail — and, under the old
/// index-staging flow, poison the index on the way out (DLI-2).
fn sanitize_author_field(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '<' || c == '>' || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Build a commit signature from user-derived `name`/`email`, sanitizing both
/// and falling back to the fixed bot signature if either is empty/invalid
/// after sanitizing.
fn build_signature(name: &str, email: &str) -> anyhow::Result<Signature<'static>> {
    let clean_name = sanitize_author_field(name);
    let clean_email = sanitize_author_field(email);
    let (clean_name, clean_email) = if clean_name.is_empty() || clean_email.is_empty() {
        (BOT_NAME.to_string(), BOT_EMAIL.to_string())
    } else {
        (clean_name, clean_email)
    };
    Signature::now(&clean_name, &clean_email)
        // Belt-and-suspenders: if a sanitized value still somehow trips
        // libgit2's validation, don't fail the whole save — attribute it to
        // the bot instead.
        .or_else(|_| Signature::now(BOT_NAME, BOT_EMAIL))
        .context("building commit signature")
}

/// Best-effort: update the on-disk index entry for `rel_path` to match the
/// commit we just made (`present == true` after a write, `false` after a
/// delete), so the user's `git status` stays clean for this note. We never
/// stage before committing, so this is the only place the on-disk index is
/// touched and it only ever touches this single path — unrelated staged files
/// are left alone.
fn sync_index_entry(repo: &Repository, rel_path: &str, present: bool) {
    let mut index = match repo.index() {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(rel_path, error = %err, "could not open git index to sync after commit");
            return;
        }
    };
    let staged = if present {
        index.add_path(Path::new(rel_path))
    } else {
        // `remove_path` errors if the path isn't in the index; that's fine.
        index.remove_path(Path::new(rel_path)).or(Ok(()))
    };
    if let Err(err) = staged.and_then(|_| index.write()) {
        tracing::warn!(rel_path, error = %err, "failed to sync git index after commit");
    }
}

/// Capture any uncommitted on-disk change to `rel_path` as its own commit
/// before it is about to be overwritten, so an external edit (Obsidian,
/// Syncthing, a manual `vim`) that touched the working tree without
/// committing is preserved in history rather than silently clobbered (DLI-3).
/// No-op when the on-disk content already matches HEAD, or when there is no
/// on-disk content to preserve.
fn checkpoint_external_change(
    repo: &Repository,
    rel_path: &str,
    disk_bytes: Option<&[u8]>,
) -> anyhow::Result<()> {
    let Some(disk_bytes) = disk_bytes else {
        return Ok(());
    };
    let head_bytes = head_blob_bytes(repo, rel_path)?;
    if head_bytes.as_deref() == Some(disk_bytes) {
        // On-disk content already matches what HEAD committed; nothing
        // uncommitted to preserve.
        return Ok(());
    }

    let blob_oid = repo.blob(disk_bytes).context("hashing external content")?;
    let components = path_components(rel_path)?;
    let base_tree = head_tree(repo)?;
    let tree_oid = build_tree_with_path(repo, base_tree.as_ref(), &components, Some(blob_oid))?;
    let tree = repo
        .find_tree(tree_oid)
        .context("looking up checkpoint tree")?;

    let signature = build_signature(BOT_NAME, BOT_EMAIL)?;
    let parent = head_commit(repo)?;
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    let message = format!("checkpoint external edit to {rel_path} before overwrite");
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &message,
        &tree,
        &parents,
    )
    .context("creating checkpoint commit")?;
    Ok(())
}

fn time_to_rfc3339(time: git2::Time) -> String {
    // git2::Time is seconds-since-epoch + a UTC offset in minutes; render a
    // simple RFC3339-ish UTC timestamp without pulling in a datetime crate.
    let secs = time.seconds();
    let days_since_epoch = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days_since_epoch);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days-since-1970-01-01 into a (year, month, day) civil date.
/// Standard Howard Hinnant "days_from_civil"-style algorithm, inverted.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_or_init_creates_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        assert!(repo.list_notes().unwrap().is_empty());
    }

    #[tokio::test]
    async fn write_read_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        // No file yet.
        assert_eq!(repo.read_file("note.md").unwrap(), None);

        repo.write_and_commit(
            "note.md",
            "# hello\n",
            "Test User",
            "test@example.com",
            "initial commit",
        )
        .await
        .unwrap();

        assert_eq!(
            repo.read_file("note.md").unwrap(),
            Some("# hello\n".to_string())
        );

        repo.write_and_commit(
            "note.md",
            "# hello again\n",
            "Test User",
            "test@example.com",
            "update",
        )
        .await
        .unwrap();

        assert_eq!(
            repo.read_file("note.md").unwrap(),
            Some("# hello again\n".to_string())
        );

        let history = repo.history("note.md").await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].message, "update");
        assert_eq!(history[1].message, "initial commit");

        let notes = repo.list_notes().unwrap();
        assert_eq!(notes, vec!["note.md".to_string()]);
    }

    #[tokio::test]
    async fn history_does_not_leak_prior_incarnation_after_delete_and_recreate() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit(
            "note.md",
            "# v1\n",
            "Test User",
            "test@example.com",
            "v1 create",
        )
        .await
        .unwrap();
        repo.write_and_commit(
            "note.md",
            "# v1 edited\n",
            "Test User",
            "test@example.com",
            "v1 edit",
        )
        .await
        .unwrap();
        repo.delete_and_commit("note.md", "Test User", "test@example.com", "v1 delete")
            .await
            .unwrap();

        // Recreate the same path as a logically new note.
        repo.write_and_commit(
            "note.md",
            "# v2\n",
            "Test User",
            "test@example.com",
            "v2 create",
        )
        .await
        .unwrap();
        repo.write_and_commit(
            "note.md",
            "# v2 edited\n",
            "Test User",
            "test@example.com",
            "v2 edit",
        )
        .await
        .unwrap();

        let history = repo.history("note.md").await.unwrap();
        let messages: Vec<&str> = history.iter().map(|c| c.message.as_str()).collect();

        // Only the current incarnation's commits should be present, newest
        // first, stopping at (and including) the commit that recreated the
        // path.
        assert_eq!(messages, vec!["v2 edit", "v2 create"]);
    }

    #[tokio::test]
    async fn author_with_angle_brackets_and_newlines_is_sanitized_not_rejected() {
        // DLI-2 trigger: an OIDC display name like "Foo <bar>" (or one with a
        // newline) used to make `Signature::now` fail, failing EVERY save.
        // It must now be sanitized and the commit must succeed.
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit(
            "note.md",
            "hi\n",
            "Foo <bar>\nInjected",
            "foo<@>bar\nx",
            "m",
        )
        .await
        .expect("write with an unsanitary author must succeed, not fail");

        let history = repo.history("note.md").await.unwrap();
        assert_eq!(history.len(), 1);
        // The stored author must not contain the characters git rejects.
        let author = &history[0].author;
        assert!(
            !author.contains('\n'),
            "author must not contain newlines: {author:?}"
        );
        // The commit is well-formed and content is correct.
        assert_eq!(repo.read_file("note.md").unwrap(), Some("hi\n".to_string()));
    }

    #[tokio::test]
    async fn empty_author_falls_back_to_bot_signature() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit("n.md", "x\n", "<>", "\n\t", "m")
            .await
            .unwrap();
        let history = repo.history("n.md").await.unwrap();
        assert!(history[0].author.contains(BOT_NAME));
        assert!(history[0].author.contains(BOT_EMAIL));
    }

    #[tokio::test]
    async fn commit_does_not_sweep_in_unrelated_staged_files() {
        // DLI-4: a file the user `git add`ed out-of-band must NOT be pulled
        // into the app's single-note commit.
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        // Seed an initial commit so there's a HEAD.
        repo.write_and_commit("first.md", "one\n", "u", "u@e", "first")
            .await
            .unwrap();

        // Create an unrelated file on disk and stage it into the index
        // out-of-band, exactly as a user running `git add` would.
        std::fs::write(dir.path().join("stray.md"), "stray content\n").unwrap();
        {
            let git = git2::Repository::open(dir.path()).unwrap();
            let mut index = git.index().unwrap();
            index.add_path(Path::new("stray.md")).unwrap();
            index.write().unwrap();
        }

        // Now the app commits a different note.
        repo.write_and_commit("second.md", "two\n", "u", "u@e", "second")
            .await
            .unwrap();

        // The HEAD commit for "second" must contain second.md but NOT the
        // out-of-band staged stray.md.
        let git = git2::Repository::open(dir.path()).unwrap();
        let tree = git
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        assert!(tree.get_path(Path::new("second.md")).is_ok());
        assert!(
            tree.get_path(Path::new("stray.md")).is_err(),
            "unrelated out-of-band staged file must not be swept into the commit"
        );
    }

    #[tokio::test]
    async fn successful_write_leaves_index_clean_for_the_note() {
        // After a write, `git status` must show nothing pending for the note
        // (worktree == index == HEAD for that path).
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        repo.write_and_commit("clean.md", "v1\n", "u", "u@e", "c1")
            .await
            .unwrap();
        repo.write_and_commit("clean.md", "v2\n", "u", "u@e", "c2")
            .await
            .unwrap();

        let git = git2::Repository::open(dir.path()).unwrap();
        let status = git.status_file(Path::new("clean.md")).unwrap();
        assert_eq!(
            status,
            git2::Status::CURRENT,
            "clean.md must be CURRENT (index==worktree==HEAD) after a committed write; got {status:?}"
        );
    }

    #[tokio::test]
    async fn failed_write_rolls_back_worktree_and_does_not_poison_next_commit() {
        // DLI-2: force a git failure AFTER the worktree write by making the
        // object store unwritable, then verify (a) the worktree is rolled
        // back and (b) a later unrelated commit is clean (no staged leftover).
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        repo.write_and_commit("keep.md", "original\n", "u", "u@e", "seed")
            .await
            .unwrap();

        // Recursively make `.git` read-only so writing new objects
        // (repo.blob / commit) fails, while the working-tree write itself
        // (outside `.git`) still succeeds. Chmod'ing only the top-level
        // `.git` isn't enough — new loose objects land under the already-
        // writable `.git/objects/` subtree.
        #[cfg(unix)]
        fn chmod_tree(path: &Path, dir_mode: u32, file_mode: u32) {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(path).unwrap();
            if meta.is_dir() {
                // Descend first (need the dir writable/traversable to recurse).
                for entry in std::fs::read_dir(path).unwrap() {
                    chmod_tree(&entry.unwrap().path(), dir_mode, file_mode);
                }
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(dir_mode)).unwrap();
            } else {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(file_mode)).unwrap();
            }
        }

        let git_dir = dir.path().join(".git");
        #[cfg(unix)]
        chmod_tree(&git_dir, 0o555, 0o444);

        let result = repo
            .write_and_commit("keep.md", "corrupt attempt\n", "u", "u@e", "should fail")
            .await;

        // Restore writable perms before asserting so the tempdir can be
        // cleaned up.
        #[cfg(unix)]
        chmod_tree(&git_dir, 0o755, 0o644);

        assert!(
            result.is_err(),
            "write should fail when the object store is unwritable"
        );
        // Worktree rolled back to the previous committed content.
        assert_eq!(
            repo.read_file("keep.md").unwrap(),
            Some("original\n".to_string()),
            "worktree must be rolled back after a failed commit"
        );

        // A subsequent unrelated write must commit cleanly (nothing poisoned).
        repo.write_and_commit("other.md", "other\n", "u", "u@e", "other")
            .await
            .unwrap();
        let git = git2::Repository::open(dir.path()).unwrap();
        let tree = git
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        // The failed write's content must NOT have leaked into this commit.
        let keep = tree.get_path(Path::new("keep.md")).unwrap();
        let blob = keep.to_object(&git).unwrap();
        assert_eq!(
            blob.as_blob().unwrap().content(),
            b"original\n",
            "the failed write's content must not appear in a later commit"
        );
    }

    #[tokio::test]
    async fn external_uncommitted_edit_is_checkpointed_before_overwrite() {
        // DLI-3: an external tool writes the working tree without committing;
        // a subsequent app write must preserve that external content as its
        // own commit rather than silently clobbering it.
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        repo.write_and_commit("ext.md", "app v1\n", "u", "u@e", "v1")
            .await
            .unwrap();

        // Simulate Obsidian/Syncthing writing the file directly.
        std::fs::write(dir.path().join("ext.md"), "EXTERNAL EDIT\n").unwrap();

        // App overwrites via the normal path.
        repo.write_and_commit("ext.md", "app v2\n", "u", "u@e", "v2")
            .await
            .unwrap();

        assert_eq!(
            repo.read_file("ext.md").unwrap(),
            Some("app v2\n".to_string())
        );

        // History must now include a checkpoint commit preserving the
        // external edit, sandwiched between v1 and v2.
        let history = repo.history("ext.md").await.unwrap();
        let messages: Vec<&str> = history.iter().map(|c| c.message.as_str()).collect();
        assert_eq!(
            messages.len(),
            3,
            "expected v1, checkpoint, v2; got {messages:?}"
        );
        assert_eq!(messages[0], "v2");
        assert!(
            messages[1].contains("checkpoint external edit"),
            "middle commit must be the external-edit checkpoint; got {:?}",
            messages[1]
        );
        assert_eq!(messages[2], "v1");
    }

    #[tokio::test]
    async fn delete_of_untracked_ondisk_file_makes_no_empty_commit() {
        // DLI-7: deleting a file that exists on disk but was never committed
        // must remove it without creating an empty no-op commit.
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        repo.write_and_commit("tracked.md", "x\n", "u", "u@e", "seed")
            .await
            .unwrap();
        let commits_before = repo.history("tracked.md").await.unwrap().len();

        // Untracked file straight to disk.
        std::fs::write(dir.path().join("untracked.md"), "y\n").unwrap();
        repo.delete_and_commit("untracked.md", "u", "u@e", "del")
            .await
            .unwrap();

        assert_eq!(
            repo.read_file("untracked.md").unwrap(),
            None,
            "file removed from disk"
        );
        // No new commit was created (HEAD unchanged).
        let git = git2::Repository::open(dir.path()).unwrap();
        let head_msg = git
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .message()
            .unwrap()
            .to_string();
        assert_eq!(head_msg.trim(), "seed", "no empty commit should be created");
        assert_eq!(
            repo.history("tracked.md").await.unwrap().len(),
            commits_before
        );
    }

    #[tokio::test]
    async fn delete_of_nonexistent_note_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        // Nothing on disk, nothing tracked: must not error and must not
        // create a commit (there is no HEAD at all).
        repo.delete_and_commit("ghost.md", "u", "u@e", "del")
            .await
            .unwrap();
        assert!(repo.list_notes().unwrap().is_empty());
        assert!(repo.history("ghost.md").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn nested_note_write_and_delete_prunes_empty_dirs_in_tree() {
        // Exercises the recursive tree builder for nested paths, including
        // pruning the now-empty directory entry on delete.
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();
        repo.write_and_commit("a/b/c.md", "deep\n", "u", "u@e", "add deep")
            .await
            .unwrap();
        repo.write_and_commit("a/b/d.md", "deep2\n", "u", "u@e", "add deep2")
            .await
            .unwrap();
        assert_eq!(
            repo.read_file("a/b/c.md").unwrap(),
            Some("deep\n".to_string())
        );

        repo.delete_and_commit("a/b/c.md", "u", "u@e", "del c")
            .await
            .unwrap();
        assert_eq!(repo.read_file("a/b/c.md").unwrap(), None);
        // Sibling still present.
        assert_eq!(
            repo.read_file("a/b/d.md").unwrap(),
            Some("deep2\n".to_string())
        );

        repo.delete_and_commit("a/b/d.md", "u", "u@e", "del d")
            .await
            .unwrap();
        // Whole tree now empty of notes; the a/ and a/b/ tree entries must be
        // pruned so HEAD's tree is empty.
        let git = git2::Repository::open(dir.path()).unwrap();
        let tree = git
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        assert!(
            tree.get_path(Path::new("a")).is_err(),
            "empty dir must be pruned from tree"
        );
    }

    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        assert!(repo.read_file("../escape.md").is_err());
        assert!(repo
            .write_and_commit("../../etc/evil.md", "x", "a", "b@c.com", "m")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn nested_dirs_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        repo.write_and_commit(
            "sub/dir/note.md",
            "content",
            "Test User",
            "test@example.com",
            "add nested note",
        )
        .await
        .unwrap();

        assert_eq!(
            repo.list_notes().unwrap(),
            vec!["sub/dir/note.md".to_string()]
        );
    }

    #[tokio::test]
    async fn read_file_of_nonexistent_path_does_not_create_parent_directory() {
        // Regression test: `read_file` (and `resolve_safe` in its read
        // mode) must never call `create_dir_all` as a side effect of a
        // read. Doing so used to leave orphaned, untracked, empty
        // directories on disk any time a read was attempted for a note
        // under a parent directory that doesn't exist yet (e.g. as a
        // pre-check for an id that turns out not to exist).
        let dir = tempfile::tempdir().unwrap();
        let repo = NotesRepo::open_or_init(dir.path().to_str().unwrap()).unwrap();

        assert_eq!(repo.read_file("brand-new-dir/note.md").unwrap(), None);

        assert!(
            !dir.path().join("brand-new-dir").exists(),
            "read_file must not create the parent directory of a nonexistent file"
        );
    }
}
