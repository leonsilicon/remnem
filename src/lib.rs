#![deny(clippy::all)]

//! remnem — find every nested `node_modules` under a project root and delete
//! them all, as fast as possible.
//!
//! This crate is split into a small library (this file plus [`finder`] and
//! [`workspace`]) and a CLI binary (`src/main.rs`). The library exists mainly so
//! the finder and workspace-resolution logic can be unit-tested; the shipped
//! artifact is the binary.

pub mod finder;
pub mod workspace;

pub use finder::{DeleteResult, FoundNodeModules, Mode};
pub use workspace::WorkspaceKind;

use std::path::Path;

/// Human-readable name for a [`WorkspaceKind`].
pub fn workspace_kind_str(kind: WorkspaceKind) -> &'static str {
  match kind {
    WorkspaceKind::None => "none",
    WorkspaceKind::PackageJson => "package.json",
    WorkspaceKind::Pnpm => "pnpm",
  }
}

/// Resolve the workspace-package directories under `root` (bun/pnpm globs).
///
/// Purely informational — deletion targets every nested `node_modules`, not just
/// these. This walks the source tree, so callers should only invoke it when the
/// user actually wants the workspace layout reported.
pub fn resolve_workspace(root: &Path) -> (WorkspaceKind, Vec<std::path::PathBuf>) {
  let ws = workspace::resolve(root);
  let packages = match workspace::WorkspaceMatcher::build(&ws) {
    Ok(matcher) => workspace::collect_workspace_dirs(root, &matcher),
    Err(_) => Vec::new(),
  };
  (ws.kind, packages)
}
