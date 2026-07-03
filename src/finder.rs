//! Finding and deleting every nested `node_modules` directory under a root.
//!
//! # Finding
//!
//! We do a lean, parallel directory-only walk. The `ignore` crate (ripgrep's
//! walker) is built to yield *files* and apply gitignore machinery; here we care
//! about neither — we only need directories, and we want to visit as few entries
//! as possible. So we hand-roll the walk on `std::fs::read_dir`, which on macOS
//! and Linux exposes each entry's type via `d_type` (from `readdir`) so we can
//! tell directories from files **without a per-entry `stat`**.
//!
//! The key tricks for speed:
//!   - Once we see a `node_modules` directory we record it and DO NOT descend —
//!     the whole subtree is slated for deletion, so walking its (often enormous)
//!     contents would be pure waste. This is what keeps the walk proportional to
//!     the *source* tree, not the installed dependency tree.
//!   - We never touch regular files: on each `read_dir` we only recurse into
//!     sub-directories and skip everything else with no extra syscalls.
//!   - `.git` is pruned (huge, never holds a `node_modules` we care about).
//!   - Work is fanned across a `rayon` scope so independent subtrees walk in
//!     parallel, and results are gathered per-thread (no shared lock on the hot
//!     path) then merged once at the end.
//!
//! # Deleting
//!
//! `Mode::Remove` fans the recorded top-level `node_modules` directories across a
//! rayon pool and `remove_dir_all`s each in parallel. `Mode::Trash` moves each to
//! the OS Trash (a same-volume rename — effectively instant regardless of size).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A discovered `node_modules` directory and the number of bytes it holds.
#[derive(Debug)]
pub struct FoundNodeModules {
  pub path: PathBuf,
  pub bytes: u64,
}

/// Walk `root` in parallel and collect every `node_modules` directory, without
/// descending into any of them (nested `node_modules` inside a `node_modules`
/// are covered by deleting the outer one, so we never recurse in).
///
/// `measure` controls whether each directory's on-disk size is summed (a second
/// parallel pass). Sizing is inherently expensive (it has to touch every file in
/// every dependency tree), so it is off by default and only done when explicitly
/// requested.
pub fn find(root: &Path, measure: bool) -> Vec<FoundNodeModules> {
  let mut paths = find_node_modules(root);
  paths.sort();

  if !measure {
    return paths
      .into_iter()
      .map(|path| FoundNodeModules { path, bytes: 0 })
      .collect();
  }

  use rayon::prelude::*;
  paths
    .into_par_iter()
    .map(|path| {
      let bytes = dir_size(&path);
      FoundNodeModules { path, bytes }
    })
    .collect()
}

/// Parallel, directory-only walk that collects every top-level `node_modules`
/// directory under `root`. Regular files are never inspected; `.git` and the
/// interior of any `node_modules` are never entered.
///
/// Findings are gathered into a single `Mutex<Vec<..>>`, but the lock is touched
/// at most once per *directory that actually contains a `node_modules`* (to
/// append that dir's finds in one batch) — never on the per-entry hot path — so
/// contention is negligible.
fn find_node_modules(root: &Path) -> Vec<PathBuf> {
  let sink = Mutex::new(Vec::new());
  rayon::scope(|scope| {
    walk_dir(root.to_path_buf(), scope, &sink);
  });
  sink.into_inner().unwrap()
}

/// Recursively walk `dir`, appending discovered `node_modules` paths to `sink`
/// and spawning parallel tasks for each sub-directory that must be descended.
///
/// Only sub-directories are ever recursed into; regular files are skipped with
/// no extra syscall (their type comes from `readdir`'s `d_type`). `node_modules`
/// and `.git` are recorded/pruned without descending.
fn walk_dir<'scope>(dir: PathBuf, scope: &rayon::Scope<'scope>, sink: &'scope Mutex<Vec<PathBuf>>) {
  let entries = match fs::read_dir(&dir) {
    Ok(e) => e,
    // A directory we cannot read (permissions, race) simply contributes nothing.
    Err(_) => return,
  };

  // Sub-directories we must descend into, and any node_modules found right here.
  let mut subdirs: Vec<PathBuf> = Vec::new();
  let mut found_here: Vec<PathBuf> = Vec::new();

  for entry in entries.flatten() {
    // `file_type()` is served from the `readdir` `d_type` on macOS/Linux — no
    // extra `stat` syscall. (Filesystems that don't report a type fall back to a
    // stat inside `is_dir()`, but that is the uncommon path.)
    let Ok(file_type) = entry.file_type() else {
      continue;
    };
    if !file_type.is_dir() {
      // Regular file / symlink: never relevant to finding node_modules.
      continue;
    }

    let name = entry.file_name();
    if name == "node_modules" {
      // Found one. Record it and DO NOT descend — the whole tree goes.
      found_here.push(entry.path());
      continue;
    }
    if name == ".git" {
      // VCS metadata: huge, never holds a node_modules we care about.
      continue;
    }

    subdirs.push(entry.path());
  }

  if !found_here.is_empty() {
    sink.lock().unwrap().append(&mut found_here);
  }

  // Fan the sub-directories across the pool. Keep the last one on this thread to
  // avoid spawning a task only to immediately block on it (and to keep shallow
  // trees from paying task-spawn overhead they don't need).
  let inline = subdirs.pop();
  for child in subdirs {
    scope.spawn(move |s| walk_dir(child, s, sink));
  }
  if let Some(child) = inline {
    walk_dir(child, scope, sink);
  }
}

/// Sum the apparent size of all regular files under `dir`, in parallel.
fn dir_size(dir: &Path) -> u64 {
  use std::sync::atomic::AtomicU64;
  let total = AtomicU64::new(0);
  rayon::scope(|scope| {
    size_dir(dir.to_path_buf(), scope, &total);
  });
  total.into_inner()
}

fn size_dir<'scope>(
  dir: PathBuf,
  scope: &rayon::Scope<'scope>,
  total: &'scope std::sync::atomic::AtomicU64,
) {
  use std::sync::atomic::Ordering;
  let entries = match fs::read_dir(&dir) {
    Ok(e) => e,
    Err(_) => return,
  };
  let mut subdirs: Vec<PathBuf> = Vec::new();
  for entry in entries.flatten() {
    let Ok(file_type) = entry.file_type() else {
      continue;
    };
    if file_type.is_dir() {
      subdirs.push(entry.path());
    } else if file_type.is_file() {
      if let Ok(meta) = entry.metadata() {
        total.fetch_add(meta.len(), Ordering::Relaxed);
      }
    }
  }
  let inline = subdirs.pop();
  for child in subdirs {
    scope.spawn(move |s| size_dir(child, s, total));
  }
  if let Some(child) = inline {
    size_dir(child, scope, total);
  }
}

/// Outcome of disposing of one directory.
#[derive(Debug)]
pub struct DeleteResult {
  pub path: PathBuf,
  pub error: Option<String>,
  /// `true` if the directory was moved to the Trash; `false` otherwise (the
  /// default rename-and-reclaim path, or a hard-remove fallback).
  pub trashed: bool,
}

/// How a directory should be disposed of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// **Default, and the reason `remnem` is instant.** Each `node_modules` is
  /// `rename`d to a hidden sibling in the same parent directory — an O(1)
  /// same-filesystem metadata operation, no matter how many files the tree
  /// holds. The moment the rename returns, the `node_modules` is gone from its
  /// original path, so a clean reinstall can proceed immediately. The renamed
  /// staging directories are then `remove_dir_all`d by a **detached background
  /// process** (see [`reap`]), so the actual disk-freeing I/O never blocks the
  /// foreground. Space is reclaimed within seconds, hands-free — no Trash to
  /// empty. If a rename fails (e.g. permissions), that one item falls back to a
  /// synchronous `remove_dir_all`.
  Remove,
  /// Synchronous, blocking `remove_dir_all` — waits until the space is actually
  /// reclaimed. Slower (I/O-bound on huge trees) but self-contained: used by the
  /// background reaper and by callers/tests that must observe the space freed
  /// before returning.
  RemoveSync,
  /// Move to the OS Trash via the native `trashItemAtURL` API. On the same
  /// volume this is a directory rename — effectively instant regardless of how
  /// many files the tree holds — and recoverable in Finder ("Put Back"). Space
  /// is reclaimed when the Trash is emptied. If trashing fails (e.g. a
  /// cross-volume item the OS would have to copy, or a permissions issue),
  /// falls back to a direct `remove_dir_all`.
  Trash,
}

/// Dispose of every given directory according to `mode`. Each operation is
/// independent; an error on one does not stop the others. Whether a directory
/// was trashed (vs. hard-removed via fallback) is recorded per result.
pub fn delete_all(dirs: Vec<PathBuf>, mode: Mode) -> Vec<DeleteResult> {
  match mode {
    Mode::Remove => rename_and_reap(dirs),
    Mode::RemoveSync => remove_all_parallel(dirs)
      .into_iter()
      .map(|(path, error)| DeleteResult {
        path,
        error,
        trashed: false,
      })
      .collect(),
    Mode::Trash => trash_all(dirs),
  }
}

/// The instant path. Rename every `node_modules` out of the way (fast), then
/// hand the staged directories to a detached background process that hard-deletes
/// them. Returns as soon as the renames are done — the caller sees every
/// `node_modules` already gone from its original location.
fn rename_and_reap(dirs: Vec<PathBuf>) -> Vec<DeleteResult> {
  let timing = std::env::var_os("REMNEM_TIMING").is_some();
  let rename_start = std::time::Instant::now();
  let pid = std::process::id();

  let outcomes = parallel_rename(dirs, pid);

  let mut results = Vec::with_capacity(outcomes.len());
  let mut staged = Vec::new();
  for (result, stage) in outcomes {
    results.push(result);
    if let Some(s) = stage {
      staged.push(s);
    }
  }
  if timing {
    eprintln!(
      "[timing]   rename: {:.1}ms ({} staged)",
      rename_start.elapsed().as_secs_f64() * 1e3,
      staged.len()
    );
  }

  // Hand the staged dirs to a detached background reaper. If we can't spawn one
  // (unlikely), delete them synchronously so we never leak disk space.
  let spawn_start = std::time::Instant::now();
  if !staged.is_empty() {
    if let Err(_e) = spawn_reaper(&staged) {
      let _ = remove_all_parallel(staged);
    }
  }
  if timing {
    eprintln!(
      "[timing]   spawn reaper: {:.1}ms",
      spawn_start.elapsed().as_secs_f64() * 1e3
    );
  }

  results
}

/// Rename every directory aside, spreading the work over an **oversubscribed**
/// thread pool. Renames are I/O-syscall-bound (each blocks on the filesystem's
/// directory-metadata journal, not the CPU), so running many more threads than
/// cores hides that latency: on APFS this lifts throughput from ~16k renames/sec
/// (core-count threads) toward the filesystem's ceiling. We use our own scratch
/// threads rather than rayon's CPU-sized global pool for exactly this reason.
///
/// Work is split into contiguous chunks, one owned by each thread — no shared
/// state, no locks, no unsafe. The global item index (chunk offset + local
/// position) seeds each staging name so they stay unique across threads.
fn parallel_rename(dirs: Vec<PathBuf>, pid: u32) -> Vec<(DeleteResult, Option<PathBuf>)> {
  let n = dirs.len();
  if n == 0 {
    return Vec::new();
  }

  // Oversubscribe: ~5× cores, capped, and never more threads than items.
  let cores = std::thread::available_parallelism()
    .map(|c| c.get())
    .unwrap_or(4);
  let threads = (cores * 5).clamp(1, 64).min(n);
  let chunk = n.div_ceil(threads);

  // Move each chunk (with its starting global offset) into its own thread.
  let mut chunks: Vec<(usize, Vec<PathBuf>)> = Vec::with_capacity(threads);
  let mut offset = 0;
  let mut remaining = dirs;
  while !remaining.is_empty() {
    let take = chunk.min(remaining.len());
    let rest = remaining.split_off(take);
    chunks.push((offset, remaining));
    offset += take;
    remaining = rest;
  }

  let mut per_thread: Vec<Vec<(DeleteResult, Option<PathBuf>)>> = std::thread::scope(|scope| {
    let handles: Vec<_> = chunks
      .into_iter()
      .map(|(base, chunk_dirs)| {
        scope.spawn(move || {
          chunk_dirs
            .into_iter()
            .enumerate()
            .map(|(i, path)| dispose_one(path, pid, base + i))
            .collect::<Vec<_>>()
        })
      })
      .collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
  });

  // Re-concatenate the per-thread results in order.
  let mut outcomes = Vec::with_capacity(n);
  for chunk in &mut per_thread {
    outcomes.append(chunk);
  }
  outcomes
}

/// Dispose of one directory via the instant rename, with fallbacks. Returns its
/// [`DeleteResult`] and, on a successful rename, the staging path to reap.
fn dispose_one(path: PathBuf, pid: u32, idx: usize) -> (DeleteResult, Option<PathBuf>) {
  let ok = |path, staged| {
    (
      DeleteResult {
        path,
        error: None,
        trashed: false,
      },
      staged,
    )
  };
  // Fall back to a synchronous remove and report its outcome.
  let remove = |path: PathBuf| {
    let error = match std::fs::remove_dir_all(&path) {
      Ok(()) => None,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
      Err(e) => Some(e.to_string()),
    };
    (
      DeleteResult {
        path,
        error,
        trashed: false,
      },
      None,
    )
  };

  match staging_path(&path, pid, idx) {
    Some(staged) => match std::fs::rename(&path, &staged) {
      Ok(()) => ok(path, Some(staged)),
      // Already gone — nothing to reap.
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => ok(path, None),
      // Rename failed (permissions, cross-device, etc.): hard-remove instead.
      Err(_) => remove(path),
    },
    // No parent (root) — never a node_modules in practice; remove synchronously.
    None => remove(path),
  }
}

/// Derive a hidden sibling staging path for `dir` in its own parent directory,
/// so the rename is guaranteed to stay on the same filesystem (an O(1) op). E.g.
/// `a/b/node_modules` → `a/b/.node_modules.remnem-<pid>-<idx>`.
fn staging_path(dir: &Path, pid: u32, idx: usize) -> Option<PathBuf> {
  let parent = dir.parent()?;
  Some(parent.join(format!(".node_modules.remnem-{pid}-{idx}")))
}

/// Spawn a detached child process that hard-deletes the staged directories in
/// the background, then exits — so the disk-freeing I/O never blocks `remnem`.
///
/// The staged paths are passed via a temp list file (there can be thousands,
/// too many for a command line) which the reaper reads and then removes. The
/// child is fully detached: new session, no controlling terminal, stdio to
/// /dev/null, so it outlives the parent shell without holding it open.
fn spawn_reaper(staged: &[PathBuf]) -> std::io::Result<()> {
  use std::io::Write;

  // Write the list of staged dirs to a temp file, one path per line.
  let list_path = std::env::temp_dir().join(format!(
    "remnem-reap-{}-{}.txt",
    std::process::id(),
    reap_nonce()
  ));
  {
    let mut f = std::fs::File::create(&list_path)?;
    for p in staged {
      f.write_all(p.as_os_str().as_encoded_bytes())?;
      f.write_all(b"\n")?;
    }
    f.flush()?;
  }

  let exe = std::env::current_exe()?;
  let mut cmd = std::process::Command::new(exe);
  cmd
    .arg("__reap")
    .arg(&list_path)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());

  // Detach from the controlling terminal / process group so the reaper is not
  // killed when the invoking shell command returns.
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` in the pre-exec hook of the child only touches the child.
    unsafe {
      cmd.pre_exec(|| {
        // Start a new session; detaches from the parent's controlling terminal.
        libc_setsid();
        Ok(())
      });
    }
  }

  cmd.spawn()?;
  Ok(())
}

/// A small nonce for the reap-list filename, avoiding `Math.random`/time deps.
/// Uniqueness only has to hold among concurrent `remnem` runs of the same pid,
/// which never collide (pid is already unique per run); a monotonic counter
/// distinguishes multiple disposals within one run.
fn reap_nonce() -> u64 {
  use std::sync::atomic::{AtomicU64, Ordering};
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// `setsid(2)` via a tiny extern binding so we don't pull in the whole `libc`
/// crate for one call. Detaches the reaper into its own session.
#[cfg(unix)]
fn libc_setsid() {
  extern "C" {
    fn setsid() -> i32;
  }
  // SAFETY: `setsid` has no memory effects; a failure (already a session leader)
  // is harmless for our purposes.
  unsafe {
    setsid();
  }
}

/// Read a reap-list file and hard-delete every directory it names, in parallel,
/// then remove the list file. This is the body of the detached `__reap`
/// subcommand. Errors are ignored — the reaper is best-effort background cleanup.
pub fn reap(list_path: &Path) {
  let Ok(text) = std::fs::read_to_string(list_path) else {
    return;
  };
  let dirs: Vec<PathBuf> = text
    .lines()
    .filter(|l| !l.is_empty())
    .map(PathBuf::from)
    .collect();
  let _ = remove_all_parallel(dirs);
  let _ = std::fs::remove_file(list_path);
}

fn remove_all_parallel(dirs: Vec<PathBuf>) -> Vec<(PathBuf, Option<String>)> {
  use rayon::prelude::*;
  dirs
    .into_par_iter()
    .map(|path| {
      let error = match std::fs::remove_dir_all(&path) {
        Ok(()) => None,
        // A concurrent delete / already-gone directory is not a failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(e.to_string()),
      };
      (path, error)
    })
    .collect()
}

/// Build a `TrashContext`. On macOS we opt into the `NsFileManager` backend —
/// it calls `trashItemAtURL` directly, which is faster than the default
/// Finder/osascript path and silent (no delete sound), while still recording
/// the "Put Back" metadata. On Linux (freedesktop) and Windows the default
/// backend is already the fast native trash, so no tuning is needed.
#[cfg(target_os = "macos")]
fn new_trash_context() -> trash::TrashContext {
  use trash::macos::{DeleteMethod, TrashContextExtMacos};
  let mut ctx = trash::TrashContext::default();
  ctx.set_delete_method(DeleteMethod::NsFileManager);
  ctx
}

#[cfg(not(target_os = "macos"))]
fn new_trash_context() -> trash::TrashContext {
  trash::TrashContext::default()
}

/// Move each directory to the OS Trash. Trashing is a single native call per
/// item and, for same-volume items, an O(1) rename — so this runs fast without
/// a thread pool. Any item the native trash call rejects falls back to a direct
/// removal so `remnem` always makes progress.
fn trash_all(dirs: Vec<PathBuf>) -> Vec<DeleteResult> {
  let ctx = new_trash_context();

  dirs
    .into_iter()
    .map(|path| match ctx.delete(&path) {
      Ok(()) => DeleteResult {
        path,
        error: None,
        trashed: true,
      },
      Err(trash_err) => {
        // Trash rejected it (cross-volume copy cost, unsupported location,
        // etc.) — reclaim the space directly instead of failing.
        let error = match std::fs::remove_dir_all(&path) {
          Ok(()) => None,
          Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
          Err(rm_err) => Some(format!(
            "trash failed ({trash_err}) and remove failed ({rm_err})"
          )),
        };
        DeleteResult {
          path,
          error,
          trashed: false,
        }
      }
    })
    .collect()
}
