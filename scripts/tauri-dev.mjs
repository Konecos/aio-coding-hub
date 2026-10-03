/**
 * Usage:
 *   pnpm tauri:dev -- [tauri dev args...]
 *
 * Purpose:
 * - Allow running the dev app alongside an installed/running release app on the same machine.
 * - Keep dev data isolated under `~/.aio-coding-hub-fork-dev/` for accurate testing.
 *
 * How it works:
 * - Ensures a local (gitignored) Tauri config overlay exists at `.local/tauri.dev.local.json`.
 * - Runs `tauri dev -c <overlay>` so dev uses a different `identifier` than the release app.
 * - Injects `AIO_CODING_HUB_DOTDIR_NAME=.aio-coding-hub-fork-dev` so Rust stores data separately.
 */

import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path, { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const APP_DOTDIR_NAME_ENV = "AIO_CODING_HUB_DOTDIR_NAME";
const DEV_APP_DOTDIR_NAME = ".aio-coding-hub-fork-dev";
const DEV_TAURI_IDENTIFIER = "io.aio.codinghub.fork.dev";
const TAURI_CONFIG_ENV = "TAURI_CONFIG";

const __dirname = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(__dirname, "..");
const localDir = resolve(projectRoot, ".local");
const overlayPath = resolve(localDir, "tauri.dev.local.json");
const defaultTargetDir = resolve(projectRoot, "src-tauri", "target-dev");

function sanitizeWindowsPath(rawPath) {
  if (process.platform !== "win32" || typeof rawPath !== "string") {
    return rawPath;
  }

  return rawPath
    .split(path.delimiter)
    .filter((entry) => !/Windows Performance Toolkit/i.test(entry))
    .join(path.delimiter);
}

function ensureDevOverlayFileExists() {
  if (existsSync(overlayPath)) {
    // Upgrade the original dev identity while preserving local window settings.
    try {
      const overlay = JSON.parse(readFileSync(overlayPath, "utf8"));
      let changed = false;
      if (overlay.identifier === "io.aio.codinghub.dev") {
        overlay.identifier = DEV_TAURI_IDENTIFIER;
        changed = true;
      }
      if (overlay.productName === "AIO Coding Hub (Dev)") {
        overlay.productName = "AIO Coding Hub Fork (Dev)";
        changed = true;
      }
      for (const window of overlay.app?.windows ?? []) {
        if (window.title === "AIO Coding Hub (Dev)") {
          window.title = "AIO Coding Hub Fork (Dev)";
          changed = true;
        }
      }
      if (changed) {
        writeFileSync(overlayPath, JSON.stringify(overlay, null, 2) + "\n", "utf8");
        console.log(`[tauri:dev] Updated local overlay for Fork: ${overlayPath}`);
      }
    } catch {
      // run() reports invalid overlays; leave the file available for manual repair.
    }
    return;
  }

  mkdirSync(localDir, { recursive: true });

  const overlay = {
    // Note: this is an overlay merged into `src-tauri/tauri.conf.json` at runtime by the Tauri CLI.
    // It is intentionally kept in `.local/` (gitignored) so each developer can customize if needed.
    productName: "AIO Coding Hub Fork (Dev)",
    identifier: DEV_TAURI_IDENTIFIER,
    app: {
      windows: [
        {
          title: "AIO Coding Hub Fork (Dev)",
          width: 1500,
          height: 900,
        },
      ],
    },
  };

  writeFileSync(overlayPath, JSON.stringify(overlay, null, 2) + "\n", "utf8");
  console.log(`[tauri:dev] Created local overlay: ${overlayPath}`);
}

function run() {
  ensureDevOverlayFileExists();

  const userArgs = process.argv.slice(2);
  if (userArgs[0] === "--") {
    userArgs.shift();
  }

  let tauriConfigEnvValue = null;
  try {
    const overlayText = readFileSync(overlayPath, "utf8");
    const overlay = JSON.parse(overlayText);
    tauriConfigEnvValue = JSON.stringify(overlay);
    if (overlay?.identifier !== DEV_TAURI_IDENTIFIER) {
      console.warn(
        `[tauri:dev] Warning: overlay identifier is "${overlay?.identifier}". Expected "${DEV_TAURI_IDENTIFIER}" for release/dev coexistence.`
      );
    }
  } catch (err) {
    console.warn(
      `[tauri:dev] Warning: failed to read/parse overlay (${overlayPath}): ${err?.message ?? err}`
    );
  }

  console.log(`[tauri:dev] Using overlay: ${overlayPath}`);
  console.log(
    `[tauri:dev] Setting ${APP_DOTDIR_NAME_ENV}=${DEV_APP_DOTDIR_NAME} (dev data isolation)`
  );

  const tauriArgs = ["dev", "-c", overlayPath, ...userArgs];

  const child = spawn("tauri", tauriArgs, {
    cwd: projectRoot,
    stdio: "inherit",
    shell: process.platform === "win32",
    env: {
      ...process.env,
      PATH: sanitizeWindowsPath(process.env.PATH),
      CARGO_TARGET_DIR: process.env.CARGO_TARGET_DIR || defaultTargetDir,
      ...(process.platform === "win32" && !process.env.CARGO_BUILD_JOBS
        ? { CARGO_BUILD_JOBS: "1" }
        : {}),
      ...(process.platform === "win32" && !process.env.CARGO_INCREMENTAL
        ? { CARGO_INCREMENTAL: "0" }
        : {}),
      [APP_DOTDIR_NAME_ENV]: DEV_APP_DOTDIR_NAME,
      // Extra safety: make sure tauri-build sees the dev identifier even if CLI config merging is skipped.
      ...(tauriConfigEnvValue ? { [TAURI_CONFIG_ENV]: tauriConfigEnvValue } : {}),
    },
  });

  child.on("exit", (code, signal) => {
    if (signal) {
      console.error(`[tauri:dev] exited with signal: ${signal}`);
      process.exit(1);
    }
    process.exit(code ?? 1);
  });

  child.on("error", (err) => {
    console.error(`[tauri:dev] failed to spawn tauri: ${err?.message ?? err}`);
    process.exit(1);
  });
}

run();
