#!/usr/bin/env node
"use strict";

// Thin launcher. remnem ships as a compiled Rust binary, one per platform, in an
// optional dependency package (@leonsilicon/remnem-<platform>). This shim
// resolves the binary for the current platform and hands off to it with the same
// argv, stdio, and exit code — so `remnem` behaves exactly like the native
// executable while still installing through npm's platform-package mechanism.

const { spawnSync } = require("child_process");
const { existsSync } = require("fs");
const { join } = require("path");

// Map Node's platform/arch to the npm sub-package that carries the binary. Keep
// this in sync with the `optionalDependencies` in package.json and the `npm/*`
// package directories.
const PACKAGES = {
  "darwin-arm64": "@leonsilicon/remnem-darwin-arm64",
  "darwin-x64": "@leonsilicon/remnem-darwin-x64",
  "linux-arm64-gnu": "@leonsilicon/remnem-linux-arm64-gnu",
  "linux-arm64-musl": "@leonsilicon/remnem-linux-arm64-musl",
  "linux-x64-gnu": "@leonsilicon/remnem-linux-x64-gnu",
  "linux-x64-musl": "@leonsilicon/remnem-linux-x64-musl",
  "win32-arm64-msvc": "@leonsilicon/remnem-win32-arm64-msvc",
  "win32-x64-msvc": "@leonsilicon/remnem-win32-x64-msvc",
};

// On Linux, tell glibc from musl so we pick the matching binary package.
function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    // `process.report` exposes the runtime's libc in its header on modern Node.
    if (process.report && typeof process.report.getReport === "function") {
      const report = process.report.getReport();
      const header = report.header || {};
      if (typeof header.glibcVersionRuntime === "string") return false;
    }
  } catch {}
  try {
    // Fallback: the ldd on musl systems mentions musl.
    return require("fs").readFileSync("/usr/bin/ldd", "utf8").includes("musl");
  } catch {
    // Default to gnu; if wrong the resolution below fails loudly with guidance.
    return false;
  }
}

function platformKey() {
  const { platform, arch } = process;
  if (platform === "linux") {
    return `linux-${arch}-${isMusl() ? "musl" : "gnu"}`;
  }
  if (platform === "win32") {
    return `win32-${arch}-msvc`;
  }
  return `${platform}-${arch}`;
}

function resolveBinary() {
  const key = platformKey();
  const pkg = PACKAGES[key];
  if (!pkg) {
    return { error: `remnem: unsupported platform ${process.platform}-${process.arch}` };
  }
  const exe = process.platform === "win32" ? "remnem.exe" : "remnem";

  // Prefer resolving through the package's own manifest so we find it wherever
  // the package manager placed it (hoisted, nested, pnpm store, etc.).
  try {
    const manifest = require.resolve(`${pkg}/package.json`);
    const binPath = join(manifest, "..", exe);
    if (existsSync(binPath)) return { binPath };
  } catch {}

  return {
    error:
      `remnem: could not find the ${pkg} package for your platform.\n` +
      `Install it with your package manager, or reinstall remnem so npm can pull the\n` +
      `matching optional dependency (${pkg}).`,
  };
}

const { binPath, error } = resolveBinary();
if (error) {
  process.stderr.write(error + "\n");
  process.exit(1);
}

// Hand off: inherit stdio so the interactive confirmation prompt and all output
// pass straight through, and propagate the binary's exit code.
const result = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  process.stderr.write(`remnem: failed to run ${binPath}: ${result.error.message}\n`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
