//! Workspace resolution mirroring bun's `package.json#workspaces` and pnpm's
//! `pnpm-workspace.yaml#packages`.
//!
//! Both tools resolve a list of glob patterns (relative to the project root)
//! into a set of workspace-package directories. A directory only counts as a
//! workspace package when it contains its own `package.json`. Both support:
//!   - `*`   — exactly one path segment (`packages/*`)
//!   - `**`  — any number of segments (`components/**`)
//!   - `!`   — a negation pattern that excludes previously-matched dirs
//!     (bun: `!**/test/**`, pnpm: `!**/test/**`)
//!
//! bun reads the globs from the root `package.json`:
//!   - array form:  `"workspaces": ["packages/*"]`
//!   - object form: `"workspaces": { "packages": ["packages/*"] }`  (npm-compat)
//!
//! pnpm reads them from `pnpm-workspace.yaml`:
//!   - `packages:\n  - 'packages/*'`
//!
//! The root package is always part of the workspace (pnpm states this
//! explicitly; bun installs the root too). We surface the root separately.

use std::path::{Path, PathBuf};

use globset::{GlobSet, GlobSetBuilder};
use serde::Deserialize;

/// Which manifest the workspace globs came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceKind {
  /// No workspace config found — a plain (single-package or non-package) project.
  None,
  /// bun / npm / yarn `package.json#workspaces`.
  PackageJson,
  /// `pnpm-workspace.yaml#packages`.
  Pnpm,
}

/// Resolved workspace layout for a project root.
#[derive(Debug)]
pub struct Workspace {
  pub kind: WorkspaceKind,
  /// Positive glob patterns (already normalized, root-relative, forward-slash).
  pub positive: Vec<String>,
  /// Negation glob patterns (the part after the leading `!`).
  pub negative: Vec<String>,
}

#[derive(Deserialize)]
struct PackageJsonWorkspaces {
  #[serde(default)]
  workspaces: Option<WorkspacesField>,
}

/// `workspaces` may be an array (`["packages/*"]`) or an object
/// (`{ "packages": ["packages/*"], "nohoist": [...] }`).
#[derive(Deserialize)]
#[serde(untagged)]
enum WorkspacesField {
  Array(Vec<String>),
  Object {
    #[serde(default)]
    packages: Vec<String>,
  },
}

impl WorkspacesField {
  fn into_patterns(self) -> Vec<String> {
    match self {
      WorkspacesField::Array(v) => v,
      WorkspacesField::Object { packages } => packages,
    }
  }
}

/// Read the workspace glob patterns for `root`, preferring an explicit
/// `pnpm-workspace.yaml` (pnpm) and otherwise `package.json#workspaces` (bun).
pub fn resolve(root: &Path) -> Workspace {
  // pnpm-workspace.yaml wins when present: a repo using pnpm keeps its globs
  // there even though a root package.json also exists.
  if let Some(patterns) = read_pnpm_workspace(root) {
    let (positive, negative) = split_patterns(patterns);
    return Workspace {
      kind: WorkspaceKind::Pnpm,
      positive,
      negative,
    };
  }

  if let Some(patterns) = read_package_json_workspaces(root) {
    let (positive, negative) = split_patterns(patterns);
    return Workspace {
      kind: WorkspaceKind::PackageJson,
      positive,
      negative,
    };
  }

  Workspace {
    kind: WorkspaceKind::None,
    positive: Vec::new(),
    negative: Vec::new(),
  }
}

fn read_package_json_workspaces(root: &Path) -> Option<Vec<String>> {
  let text = std::fs::read_to_string(root.join("package.json")).ok()?;
  let parsed: PackageJsonWorkspaces = serde_json::from_str(&text).ok()?;
  let patterns = parsed.workspaces?.into_patterns();
  if patterns.is_empty() {
    None
  } else {
    Some(patterns)
  }
}

fn read_pnpm_workspace(root: &Path) -> Option<Vec<String>> {
  let text = std::fs::read_to_string(root.join("pnpm-workspace.yaml"))
    .or_else(|_| std::fs::read_to_string(root.join("pnpm-workspace.yml")))
    .ok()?;
  let patterns = parse_pnpm_packages(&text);
  if patterns.is_empty() {
    None
  } else {
    Some(patterns)
  }
}

/// Extract the `packages:` list from a `pnpm-workspace.yaml`.
///
/// `pnpm-workspace.yaml` is a flat, hand-authored YAML file whose `packages`
/// value is always a block sequence of quoted/unquoted glob strings, e.g.:
///
/// ```yaml
/// packages:
///   - 'packages/*'
///   - "components/**"
///   - '!**/test/**'
/// ```
///
/// We parse exactly that shape directly rather than pulling in a full YAML
/// engine: find the top-level `packages:` key, then read the following
/// more-indented `- item` lines until the indentation returns to top level.
fn parse_pnpm_packages(text: &str) -> Vec<String> {
  let mut patterns = Vec::new();
  let mut in_packages = false;
  let mut list_indent: Option<usize> = None;

  for raw_line in text.lines() {
    // Strip trailing comments that are not inside quotes. pnpm-workspace.yaml
    // uses `#` comments; a `#` inside a quoted glob is not a comment, but such
    // globs are vanishingly rare, so only strip a `#` that is preceded by
    // whitespace or starts the line.
    let line = strip_comment(raw_line);
    if line.trim().is_empty() {
      continue;
    }

    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim();

    if !in_packages {
      // Top-level `packages:` (indent 0, ends the key with a colon and nothing
      // meaningful after it).
      if indent == 0 && (trimmed == "packages:" || trimmed.starts_with("packages:")) {
        let after = trimmed["packages:".len()..].trim();
        // Inline-array form `packages: ['a', 'b']` is not emitted by pnpm, but
        // support it defensively.
        if after.starts_with('[') {
          patterns.extend(parse_inline_array(after));
          // Inline form is self-contained; keep scanning for nothing more.
          continue;
        }
        in_packages = true;
      }
      continue;
    }

    // Inside the packages block. A list item starts with `-`.
    if let Some(item) = trimmed.strip_prefix('-') {
      // Record the indentation of the first item to know when the block ends.
      if list_indent.is_none() {
        list_indent = Some(indent);
      }
      if let Some(value) = clean_scalar(item.trim()) {
        patterns.push(value);
      }
      continue;
    }

    // A non-item line at top level (indent 0) ends the packages block.
    if indent == 0 {
      break;
    }
    // Any other content at the packages indentation ends the block too.
    if let Some(li) = list_indent {
      if indent < li {
        break;
      }
    }
  }

  patterns
}

/// Strip an unquoted trailing `#` comment from a YAML line.
fn strip_comment(line: &str) -> &str {
  let bytes = line.as_bytes();
  let mut in_single = false;
  let mut in_double = false;
  for (i, &b) in bytes.iter().enumerate() {
    match b {
      b'\'' if !in_double => in_single = !in_single,
      b'"' if !in_single => in_double = !in_double,
      b'#' if !in_single && !in_double => {
        // Only a `#` at line start or preceded by whitespace is a comment.
        if i == 0 || bytes[i - 1].is_ascii_whitespace() {
          return &line[..i];
        }
      }
      _ => {}
    }
  }
  line
}

/// Remove surrounding quotes from a scalar value, returning `None` for empties.
fn clean_scalar(value: &str) -> Option<String> {
  let v = value.trim();
  let unquoted = if (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
    || (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
  {
    &v[1..v.len() - 1]
  } else {
    v
  };
  if unquoted.is_empty() {
    None
  } else {
    Some(unquoted.to_string())
  }
}

/// Parse an inline YAML/JSON-ish array `['a', "b"]` into its string items.
fn parse_inline_array(text: &str) -> Vec<String> {
  let inner = text.trim().trim_start_matches('[').trim_end_matches(']');
  inner
    .split(',')
    .filter_map(|item| clean_scalar(item.trim()))
    .collect()
}

/// Split a raw pattern list into (positive, negative) sets, stripping the
/// leading `!` from negations and normalizing to forward slashes.
fn split_patterns(patterns: Vec<String>) -> (Vec<String>, Vec<String>) {
  let mut positive = Vec::new();
  let mut negative = Vec::new();
  for raw in patterns {
    let pattern = normalize_pattern(raw.trim());
    if let Some(stripped) = pattern.strip_prefix('!') {
      negative.push(stripped.trim_start_matches("./").to_string());
    } else {
      positive.push(pattern.trim_start_matches("./").to_string());
    }
  }
  (positive, negative)
}

/// Normalize a workspace pattern: backslashes → forward slashes, drop trailing
/// slash. (bun/pnpm globs are always forward-slash and directory-oriented.)
fn normalize_pattern(pattern: &str) -> String {
  let mut p = pattern.replace('\\', "/");
  // A pattern like `packages/*/` targets the directory itself; drop the
  // trailing slash so the glob matches the directory path (not a child).
  if p.len() > 1 && p.ends_with('/') {
    p.pop();
  }
  p
}

/// Compile positive/negative pattern sets into a matcher.
///
/// A directory is a workspace package iff it (a) matches at least one positive
/// pattern, (b) matches no negative pattern, and (c) contains a `package.json`.
pub struct WorkspaceMatcher {
  positive: GlobSet,
  negative: GlobSet,
  has_positive: bool,
}

impl WorkspaceMatcher {
  pub fn build(ws: &Workspace) -> Result<Self, globset::Error> {
    Ok(Self {
      positive: build_globset(&ws.positive)?,
      negative: build_globset(&ws.negative)?,
      has_positive: !ws.positive.is_empty(),
    })
  }

  /// `rel` is the candidate directory path relative to the project root, using
  /// forward slashes (e.g. `packages/pkg-a`).
  pub fn is_match(&self, rel: &str) -> bool {
    if !self.has_positive {
      return false;
    }
    self.positive.is_match(rel) && !self.negative.is_match(rel)
  }
}

fn build_globset(patterns: &[String]) -> Result<GlobSet, globset::Error> {
  let mut builder = GlobSetBuilder::new();
  for pattern in patterns {
    add_glob(&mut builder, pattern)?;
    // picomatch (the matcher bun/npm/yarn use for workspace globs) treats a
    // trailing `/**` as OPTIONAL: `components/**` matches `components` itself as
    // well as anything beneath it, and `!**/test/**` excludes a directory named
    // `test` as well as its contents. globset's `**` requires at least one
    // trailing segment, so we additionally register the pattern with the
    // trailing `/**` stripped to recover picomatch's semantics.
    if let Some(prefix) = pattern.strip_suffix("/**") {
      if !prefix.is_empty() {
        add_glob(&mut builder, prefix)?;
      }
    }
  }
  builder.build()
}

fn add_glob(builder: &mut GlobSetBuilder, pattern: &str) -> Result<(), globset::Error> {
  // `literal_separator(true)` makes `*` stop at `/` (one segment) and `**` span
  // segments — matching bun/pnpm/picomatch semantics rather than globset's
  // default where `*` also crosses `/`.
  let glob = globset::GlobBuilder::new(pattern)
    .literal_separator(true)
    .build()?;
  builder.add(glob);
  Ok(())
}

/// Collect the workspace-package directories under `root` per the matcher.
///
/// This is a bounded walk used only to *report* the workspace layout (the
/// deletion itself is a separate, exhaustive node_modules sweep). It prunes
/// descent into `node_modules`, `.git`, and hidden directories so it stays fast
/// even on huge trees, while still supporting `**` patterns.
pub fn collect_workspace_dirs(root: &Path, matcher: &WorkspaceMatcher) -> Vec<PathBuf> {
  use ignore::WalkBuilder;

  let mut out = Vec::new();
  let mut builder = WalkBuilder::new(root);
  builder
    .hidden(false)
    .ignore(false)
    .git_ignore(false)
    .git_global(false)
    .git_exclude(false)
    .follow_links(false)
    .filter_entry(|entry| {
      let name = entry.file_name().to_string_lossy();
      // Never descend into node_modules or VCS metadata while discovering
      // workspace packages.
      !(name == "node_modules" || name == ".git")
    });

  for result in builder.build() {
    let Ok(entry) = result else { continue };
    if !entry.file_type().is_some_and(|ft| ft.is_dir()) {
      continue;
    }
    let path = entry.path();
    if path == root {
      continue;
    }
    let Ok(rel) = path.strip_prefix(root) else {
      continue;
    };
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    if matcher.is_match(&rel_str) && path.join("package.json").is_file() {
      out.push(path.to_path_buf());
    }
  }

  out.sort();
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn matcher(positive: &[&str], negative: &[&str]) -> WorkspaceMatcher {
    let ws = Workspace {
      kind: WorkspaceKind::PackageJson,
      positive: positive.iter().map(|s| s.to_string()).collect(),
      negative: negative.iter().map(|s| s.to_string()).collect(),
    };
    WorkspaceMatcher::build(&ws).unwrap()
  }

  #[test]
  fn star_matches_one_segment_only() {
    let m = matcher(&["packages/*"], &[]);
    assert!(m.is_match("packages/a"));
    assert!(!m.is_match("packages/a/sub"));
    assert!(!m.is_match("packages"));
  }

  #[test]
  fn globstar_matches_prefix_and_descendants_like_picomatch() {
    // picomatch treats a trailing `/**` as optional: `components/**` matches
    // `components` itself as well as anything under it.
    let m = matcher(&["components/**"], &[]);
    assert!(m.is_match("components"));
    assert!(m.is_match("components/x"));
    assert!(m.is_match("components/deep/c"));
  }

  #[test]
  fn negation_excludes_matched_dirs() {
    let m = matcher(&["packages/*", "components/**"], &["**/test/**"]);
    assert!(m.is_match("packages/a"));
    assert!(m.is_match("components/deep/c"));
    // `!**/test/**` excludes a dir literally named `test` AND its contents.
    assert!(!m.is_match("packages/test"));
    assert!(!m.is_match("components/test"));
    assert!(!m.is_match("packages/nested/test/fixtures"));
  }

  #[test]
  fn no_positive_patterns_matches_nothing() {
    let m = matcher(&[], &["**/test/**"]);
    assert!(!m.is_match("packages/a"));
  }

  #[test]
  fn parses_pnpm_block_sequence() {
    let yaml = "\
# a comment
packages:
  - 'packages/*'
  - \"components/**\"   # inline comment
  - '!**/test/**'
";
    let got = parse_pnpm_packages(yaml);
    assert_eq!(got, vec!["packages/*", "components/**", "!**/test/**"]);
  }

  #[test]
  fn pnpm_block_ends_at_next_top_level_key() {
    let yaml = "\
packages:
  - 'packages/*'
catalog:
  react: ^19.0.0
";
    let got = parse_pnpm_packages(yaml);
    assert_eq!(got, vec!["packages/*"]);
  }

  #[test]
  fn pnpm_inline_array_form() {
    let yaml = "packages: ['packages/*', \"apps/*\"]\n";
    let got = parse_pnpm_packages(yaml);
    assert_eq!(got, vec!["packages/*", "apps/*"]);
  }

  #[test]
  fn package_json_array_form() {
    let json = r#"{ "workspaces": ["packages/*", "!packages/excluded"] }"#;
    let parsed: PackageJsonWorkspaces = serde_json::from_str(json).unwrap();
    let (pos, neg) = split_patterns(parsed.workspaces.unwrap().into_patterns());
    assert_eq!(pos, vec!["packages/*"]);
    assert_eq!(neg, vec!["packages/excluded"]);
  }

  #[test]
  fn package_json_object_form() {
    let json = r#"{ "workspaces": { "packages": ["libs/*"], "nohoist": ["x"] } }"#;
    let parsed: PackageJsonWorkspaces = serde_json::from_str(json).unwrap();
    let patterns = parsed.workspaces.unwrap().into_patterns();
    assert_eq!(patterns, vec!["libs/*"]);
  }

  #[test]
  fn split_patterns_strips_bang_and_dot_slash() {
    let (pos, neg) = split_patterns(vec![
      "./packages/*".to_string(),
      "!./packages/test".to_string(),
    ]);
    assert_eq!(pos, vec!["packages/*"]);
    assert_eq!(neg, vec!["packages/test"]);
  }
}
