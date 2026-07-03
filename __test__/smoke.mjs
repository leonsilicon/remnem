// Cross-platform smoke test run in CI for each built target: proves the compiled
// `remnem` binary runs on this platform and that its scan finds the right
// node_modules, then that a real delete clears them. Uses a throwaway temp
// workspace, so it never touches anything real.
//
// Usage: node __test__/smoke.mjs <path-to-remnem-binary>

import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const BIN = process.argv[2];
if (!BIN) {
  console.error("usage: node __test__/smoke.mjs <path-to-remnem-binary>");
  process.exit(2);
}

function assert(cond, msg) {
  if (!cond) {
    console.error(`FAIL: ${msg}`);
    process.exit(1);
  }
}

function run(args, cwd) {
  const r = spawnSync(BIN, args, { cwd, encoding: "utf8" });
  assert(r.status === 0, `remnem ${args.join(" ")} exited ${r.status}: ${r.stderr}`);
  return r.stdout;
}

const root = mkdtempSync(join(tmpdir(), "remnem-smoke-"));
try {
  // A tiny workspace: root + one package, each with a node_modules.
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify({ name: "smoke-root", workspaces: ["packages/*"] }),
  );
  mkdirSync(join(root, "packages", "a"), { recursive: true });
  writeFileSync(join(root, "packages", "a", "package.json"), "{}");
  mkdirSync(join(root, "node_modules", "dep"), { recursive: true });
  writeFileSync(join(root, "node_modules", "dep", "index.js"), "module.exports = 1;");
  mkdirSync(join(root, "packages", "a", "node_modules"), { recursive: true });

  // List mode (--list) must find both node_modules and delete nothing. -w/-m
  // exercise the workspace-resolution and sizing passes too.
  const scan = JSON.parse(run(["--list", "--json", "--workspace", "--measure", root]));
  assert(scan.count === 2, `expected 2 node_modules, found ${scan.count}`);
  assert(scan.workspaceKind === "package.json", `workspaceKind was ${scan.workspaceKind}`);
  assert(
    scan.workspacePackages.length === 1,
    `expected 1 workspace package, got ${scan.workspacePackages.length}`,
  );
  assert(scan.totalBytes > 0, "expected non-zero total bytes with --measure");
  assert(existsSync(join(root, "node_modules")), "list mode must not delete anything");

  // Real deletion (non-TTY auto-confirms). Both node_modules must be gone,
  // package.json must survive.
  const result = JSON.parse(run(["--json", "-y", root]));
  assert(result.count === 2, `expected to delete 2, got ${result.count}`);
  assert(result.failed === 0, `expected 0 failures, got ${result.failed}`);
  assert(!existsSync(join(root, "node_modules")), "root node_modules should be gone");
  assert(
    !existsSync(join(root, "packages", "a", "node_modules")),
    "package node_modules should be gone",
  );
  assert(existsSync(join(root, "package.json")), "package.json must survive");

  console.log(`OK: remnem smoke test passed on ${process.platform}/${process.arch}`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
