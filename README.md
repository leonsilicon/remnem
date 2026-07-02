# rnmn

**r**e**m**ove **n**ode_**m**odules — find every nested `node_modules` in a project (root + all workspaces + any nested ones) and delete them all, as fast as possible.

Written in Rust ([napi-rs](https://napi.rs)) with a parallel directory walker and parallel deletion. Uses the **same workspace resolution as [bun](https://bun.sh/docs/install/workspaces) and [pnpm](https://pnpm.io/pnpm-workspace_yaml)** to describe the workspace layout.

```
$ rnmn
root: /Users/you/dev/my-monorepo
      package.json workspace (12 packages)
found 13 node_modules totalling 2.4 GB:
    318 MB  node_modules
    1.1 GB  apps/web/node_modules
    ...
delete these 13 directories? [y/N] y

deleted 13/13 node_modules (2.4 GB) in 412ms
```

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

## What it deletes

**Every `node_modules` directory** under the given root — the root's own, every
workspace package's, and any stray nested ones — leaving all your source and
`package.json` files untouched. The walker never descends *into* a
`node_modules` (the whole subtree is going to be removed anyway), so it stays
fast even on trees with hundreds of thousands of files.

Workspace resolution (reading `package.json#workspaces` for bun/npm/yarn, or
`pnpm-workspace.yaml#packages` for pnpm) is used to **report** the workspace
layout; deletion always targets every nested `node_modules`, not only workspace
packages.

## Usage

```
rnmn [path] [options]

Arguments:
  path                 Project root to clean (default: current directory)

Options:
  -n, --dry-run        List what would be deleted; delete nothing
      --no-measure     Skip sizing each node_modules (faster; sizes show as 0)
      --json           Print the raw result as JSON
  -y, --yes            Skip the confirmation prompt
  -h, --help           Show this help
```

By default `rnmn` prints what it found and asks for confirmation before
deleting (skipped with `-y`, or when stdout isn't a TTY, e.g. in CI). Use
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

// Delete every nested node_modules under a root.
const result = clean({ root: "/path/to/repo", dryRun: false, measure: true });
// → { root, workspaceKind, workspacePackages, cleaned: [...], totalBytes, count, failed }

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
