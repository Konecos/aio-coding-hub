import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { getAllowedCommitPrefixes, getReleaseBase } from "./check-release-pr-changelog.mjs";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "aio-release-baseline-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (...args) => execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
  git("init", "--quiet");
  git("config", "user.name", "Release test");
  git("config", "user.email", "release-test@example.invalid");
  git("-c", "core.hooksPath=", "commit", "--quiet", "--allow-empty", "-m", "chore: 上游发布基线");
  const bootstrapSha = git("rev-parse", "HEAD");
  git("-c", "core.hooksPath=", "commit", "--quiet", "--allow-empty", "-m", "fix: Fork 修改");
  const forkSha = git("rev-parse", "HEAD");
  const config = {
    "bootstrap-sha": bootstrapSha,
    packages: {
      ".": {
        "package-name": "aio-coding-hub",
        "include-v-in-tag": true,
        "initial-version": "100.60.20",
      },
    },
  };
  const writeConfig = () =>
    writeFileSync(join(root, "release-please-config.json"), JSON.stringify(config));
  const writeVersion = (version) =>
    writeFileSync(join(root, ".release-please-manifest.json"), JSON.stringify({ ".": version }));
  writeConfig();
  writeVersion("100.60.20");
  return { root, git, bootstrapSha, forkSha, config, writeConfig, writeVersion };
}

test("a fork without release tags uses its configured bootstrap commit", (t) => {
  const { root, bootstrapSha, forkSha } = fixture(t);
  const base = getReleaseBase({ root });
  assert.equal(base, bootstrapSha);
  const allowed = getAllowedCommitPrefixes(base, "HEAD", root);
  assert.ok(allowed.has(forkSha));
  assert.ok(allowed.has(forkSha.slice(0, 7)));
  assert.ok(!allowed.has(bootstrapSha));
});

test("subsequent releases use the manifest tag instead of the bootstrap commit", (t) => {
  const { root, git, forkSha, writeVersion } = fixture(t);
  git("tag", "aio-coding-hub-v100.60.21");
  git("-c", "core.hooksPath=", "commit", "--quiet", "--allow-empty", "-m", "fix: 下一轮修改");
  const nextSha = git("rev-parse", "HEAD");
  writeVersion("100.60.21");
  const base = getReleaseBase({ root });
  assert.equal(base, "refs/tags/aio-coding-hub-v100.60.21");
  const allowed = getAllowedCommitPrefixes(base, "HEAD", root);
  assert.ok(allowed.has(nextSha));
  assert.ok(!allowed.has(forkSha), "old fork changes must not reappear in the changelog");
});

test("missing tags after the initial release fail instead of widening the changelog range", (t) => {
  const { root, writeVersion } = fixture(t);
  writeVersion("100.60.21");
  assert.throws(
    () => getReleaseBase({ root }),
    /Release tag aio-coding-hub-v100\.60\.21 is missing/
  );
});

test("a missing tag without an explicit bootstrap commit has an actionable error", (t) => {
  const { root, config, writeConfig } = fixture(t);
  delete config["bootstrap-sha"];
  writeConfig();
  assert.throws(() => getReleaseBase({ root }), /configure bootstrap-sha and initial-version/);
});

test("an invalid bootstrap commit is rejected", (t) => {
  const { root, config, writeConfig } = fixture(t);
  config["bootstrap-sha"] = "0".repeat(40);
  writeConfig();
  assert.throws(() => getReleaseBase({ root }), /missing or is not an ancestor/);
});

test("an existing release tag outside the target history cannot fall back to bootstrap", (t) => {
  const { root, git, bootstrapSha } = fixture(t);
  git("checkout", "--quiet", "--orphan", "unrelated");
  git("-c", "core.hooksPath=", "commit", "--quiet", "--allow-empty", "-m", "chore: 无关发布");
  git("tag", "aio-coding-hub-v100.60.20");
  assert.throws(() => getReleaseBase({ root, baseRef: bootstrapSha }), /not an ancestor/);
});

test("an explicit last release SHA takes precedence over tags and bootstrap", (t) => {
  const { root, git, forkSha, config, writeConfig } = fixture(t);
  git("tag", "aio-coding-hub-v100.60.20");
  config["last-release-sha"] = forkSha;
  writeConfig();
  assert.equal(getReleaseBase({ root }), forkSha);
});

test("a command-line baseline is validated and overrides configured history", (t) => {
  const { root, bootstrapSha } = fixture(t);
  assert.equal(getReleaseBase({ root, baseTag: bootstrapSha }), bootstrapSha);
  assert.throws(
    () => getReleaseBase({ root, baseTag: "missing-tag" }),
    /missing or is not an ancestor/
  );
});
