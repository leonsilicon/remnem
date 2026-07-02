//! Finding and deleting every nested `node_modules` directory under a root.
//!
//! Finding uses ripgrep's parallel directory walker (`ignore`), which fans the
//! traversal across threads. The key trick for speed: once we see a
//! `node_modules` directory we record it and DO NOT descend into it — the whole
//! subtree is going to be deleted wholesale, so walking its (often enormous)
//! contents would be wasted work.
//!
//! Deleting fans the recorded top-level `node_modules` directories across a
//! rayon pool and `remove_dir_all`s each in parallel.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};

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
/// parallel pass). Sizing is the slow part on cold caches, so it is optional.
pub fn find(root: &Path, measure: bool) -> Vec<FoundNodeModules> {
  let found: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

  let mut builder = WalkBuilder::new(root);
  builder
    // We are cleaning artifacts, not respecting ignore files — visit everything
    // except the trees we deliberately prune below.
    .hidden(false)
    .ignore(false)
    .git_ignore(false)
    .git_global(false)
    .git_exclude(false)
    .follow_links(false)
    .filter_entry(|entry| {
      // Prune VCS metadata; huge and never contains node_modules we care about.
      entry.file_name() != ".git"
    });

  builder.build_parallel().run(|| {
    let found = &found;
    Box::new(move |result| {
      let Ok(entry) = result else {
        return WalkState::Continue;
      };
      // Only directories named exactly `node_modules`.
      let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
      if is_dir && entry.file_name() == "node_modules" {
        found.lock().unwrap().push(entry.path().to_path_buf());
        // Do not descend: the entire directory is slated for deletion.
        return WalkState::Skip;
      }
      WalkState::Continue
    })
  });

  let mut paths = found.into_inner().unwrap();
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

/// Sum the apparent size of all regular files under `dir`, in parallel.
fn dir_size(dir: &Path) -> u64 {
  let total = AtomicU64::new(0);
  WalkBuilder::new(dir)
    .hidden(false)
    .ignore(false)
    .git_ignore(false)
    .git_global(false)
    .git_exclude(false)
    .follow_links(false)
    .build_parallel()
    .run(|| {
      let total = &total;
      Box::new(move |result| {
        if let Ok(entry) = result {
          if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
              total.fetch_add(meta.len(), Ordering::Relaxed);
            }
          }
        }
        WalkState::Continue
      })
    });
  total.into_inner()
}

/// Outcome of deleting one directory.
#[derive(Debug)]
pub struct DeleteResult {
  pub path: PathBuf,
  pub error: Option<String>,
}

/// Delete every given directory in parallel. Each deletion is independent; an
/// error on one does not stop the others.
pub fn delete_all(dirs: Vec<PathBuf>) -> Vec<DeleteResult> {
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
      DeleteResult { path, error }
    })
    .collect()
}
