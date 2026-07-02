# rnmn

**r**e**m**ove **n**ode_**m**odules — find every nested `node_modules` in a project (root + all workspaces + any nested ones) and clear them all, **instantly**.

Written in Rust ([napi-rs](https://napi.rs)) with a parallel directory walker. By default it **moves each `node_modules` to the Trash**, which on the same volume is a directory rename — O(1), effectively instant no matter how many files the tree holds — and recoverable in Finder. Uses the **same workspace resolution as [bun](https://bun.sh/docs/install/workspaces) and [pnpm](https://pnpm.io/pnpm-workspace_yaml)** to describe the workspace layout.

```
$ rnmn
root: /Users/you/dev/my-monorepo
      package.json workspace (12 packages)
found 13 node_modules totalling 2.4 GB:
    318 MB  node_modules
    1.1 GB  apps/web/node_modules
    ...
move to Trash these 13 directories? [y/N] y

moved to Trash: 13/13 node_modules (2.4 GB) in 38ms
empty the Trash to reclaim the space (or re-run with --rm).
```

## Why it's instant

Deleting a `node_modules` is slow because it holds tens of thousands of tiny files, and `remove_dir_all` must unlink each one. Moving a directory to the Trash on the **same volume** is a single rename — one filesystem operation, independent of how many files are inside. So `rnmn` renames each `node_modules` into the OS Trash and returns immediately; the actual bytes are reclaimed when you empty the Trash (or run `rnmn --rm`). A `node_modules` on a *different* volume (rare) can't be renamed instantly, so those fall back to a direct parallel delete.

## Install

Build the native addon and link the `rnmn` binary globally:

```sh
bun install
bun run build      # release build → rnmn.<platform>.node + index.js/index.d.ts
bun link           # makes `rnmn` available on your PATH
```

Then from any repo root:

```sh
rnmn
```

## What it clears

**Every `node_modules` directory** under the given root — the root's own, every
workspace package's, and any stray nested ones — leaving all your source and
`package.json` files untouched. The walker never descends *into* a
`node_modules` (the whole subtree is going to be removed anyway), so it stays
fast even on trees with hundreds of thousands of files.

Workspace resolution (reading `package.json#workspaces` for bun/npm/yarn, or
`pnpm-workspace.yaml#packages` for pnpm) is used to **report** the workspace
layout; clearing always targets every nested `node_modules`, not only workspace
packages.

## Usage

```
rnmn [path] [options]

Arguments:
  path                 Project root to clean (default: current directory)

Options:
  -n, --dry-run        List what would be cleared; touch nothing
      --rm             Permanently delete instead of moving to the Trash
      --no-measure     Skip sizing each node_modules (faster; sizes show as 0)
      --json           Print the raw result as JSON
  -y, --yes            Skip the confirmation prompt
  -h, --help           Show this help
```

By default `rnmn` moves each `node_modules` to the Trash (instant, recoverable),
after printing what it found and asking for confirmation (skipped with `-y`, or
when stdout isn't a TTY, e.g. in CI). Space is reclaimed when you empty the
Trash. Use `--rm` to hard-delete and reclaim the space immediately, or
`--dry-run` to preview without touching anything.

## Workspace resolution

`rnmn` mirrors how bun and pnpm resolve workspace packages:

| Source | Field | Example |
| --- | --- | --- |
| bun / npm / yarn | `package.json` → `workspaces` | `["packages/*", "!packages/excluded"]` |
| bun / npm / yarn | `package.json` → `workspaces.packages` | `{ "packages": ["libs/*"] }` |
| pnpm | `pnpm-workspace.yaml` → `packages` | `- 'packages/*'`<br>`- '!**/test/**'` |

Glob semantics match [picomatch](https://github.com/micromatch/picomatch) (the
matcher bun/npm/yarn use):

- `*` matches exactly one path segment (`packages/*` → `packages/a`, not `packages/a/b`)
- `**` matches any number of segments, and a trailing `/**` is **optional**
  (`components/**` matches `components` itself and everything beneath it)
- `!pattern` excludes previously-matched directories (`!**/test/**` drops a
  directory named `test` and its contents)

A directory only counts as a workspace package when it contains its own
`package.json`.

## API

The napi-rs core is also usable directly from JavaScript:

```js
const { clean, resolveWorkspace } = require("rnmn");

// Clear every nested node_modules under a root.
//   trash: true (default) → move to Trash (instant); false → permanent delete.
const result = clean({ root: "/path/to/repo", dryRun: false, measure: true, trash: true });
// → { root, workspaceKind, workspacePackages, cleaned: [{ path, bytes, deleted, trashed, error }], totalBytes, count, failed }

// Just inspect how bun/pnpm would see the workspace (no deletion).
const ws = resolveWorkspace("/path/to/repo");
// → { workspaceKind: "pnpm" | "package.json" | "none", workspacePackages: [...] }
```

See `index.d.ts` for the full typed surface.

## Development

```sh
cargo test                 # Rust unit tests (workspace resolution + glob semantics)
bun run build:debug        # debug build
bun run build              # release build (LTO)
```

## License

MIT
