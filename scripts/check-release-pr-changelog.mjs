#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const logger = console;

const scriptPath = fileURLToPath(import.meta.url);
const repoRoot = resolve(scriptPath, "../..");

function run(command, args, options = {}) {
  return execFileSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  }).trim();
}

function parseArgs(argv) {
  const args = new Map();

  for (let index = 0; index < argv.length; index += 1) {
    const item = argv[index];

    if (!item.startsWith("--")) {
      throw new Error(`Unexpected argument: ${item}`);
    }

    const key = item.slice(2);
    const value = argv[index + 1];

    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }

    args.set(key, value);
    index += 1;
  }

  return args;
}

function getPrNumbers(args) {
  const prNumber = args.get("pr");

  if (prNumber) {
    return [Number(prNumber)];
  }

  const prsJson = args.get("prs-json") ?? process.env.RELEASE_PLEASE_PRS ?? "";

  if (!prsJson.trim()) {
    return [];
  }

  const prs = JSON.parse(prsJson);
  return prs
    .map((item) => {
      if (typeof item.number === "number") {
        return item.number;
      }

      const url = item.html_url ?? item.url ?? "";
      const match = url.match(/\/pull\/(\d+)$/u);
      return match ? Number(match[1]) : NaN;
    })
    .filter((item) => Number.isInteger(item) && item > 0);
}

export function getReleaseBase({ root = repoRoot, baseRef = "HEAD", baseTag } = {}) {
  const git = (...args) => run("git", args, { cwd: root });
  const verifyBase = (ref) => {
    try {
      git("rev-parse", "--verify", "--end-of-options", `${ref}^{commit}`);
      git("merge-base", "--is-ancestor", ref, baseRef);
    } catch {
      throw new Error(`Release baseline ${ref} is missing or is not an ancestor of ${baseRef}.`);
    }
    return ref;
  };

  if (baseTag) {
    return verifyBase(baseTag);
  }

  const manifest = JSON.parse(readFileSync(resolve(root, ".release-please-manifest.json"), "utf8"));
  const config = JSON.parse(readFileSync(resolve(root, "release-please-config.json"), "utf8"));
  const rootPackage = config.packages?.["."] ?? {};
  const packageName = rootPackage["package-name"];
  const version = manifest["."];

  if (!packageName || !version) {
    throw new Error("Cannot derive release tag from release-please config and manifest.");
  }

  if (config["last-release-sha"]) {
    return verifyBase(config["last-release-sha"]);
  }

  const component = rootPackage["include-component-in-tag"] === false ? "" : `${packageName}-`;
  const tag = `${component}${rootPackage["include-v-in-tag"] === true ? "v" : ""}${version}`;
  const tagRef = `refs/tags/${tag}`;
  let tagExists = false;
  try {
    git("rev-parse", "--verify", "--end-of-options", `${tagRef}^{commit}`);
    tagExists = true;
  } catch {
    // A new fork has the upstream commits, but may not have its release tags.
  }
  if (tagExists) {
    return verifyBase(tagRef);
  }

  const bootstrapSha = config["bootstrap-sha"];
  if (
    bootstrapSha &&
    (!rootPackage["initial-version"] || version === rootPackage["initial-version"])
  ) {
    logger.info(`发布标签 ${tag} 不存在，使用首次发布基线 ${bootstrapSha}。`);
    return verifyBase(bootstrapSha);
  }

  throw new Error(
    `Release tag ${tag} is missing. Fetch the release tags, or configure bootstrap-sha and initial-version for the first release of this fork.`
  );
}

export function getAllowedCommitPrefixes(baseTag, baseRef, root = repoRoot) {
  const commits = run("git", ["rev-list", `${baseTag}..${baseRef}`], { cwd: root });

  return new Set(
    commits
      .split("\n")
      .map((item) => item.trim())
      .filter(Boolean)
      .flatMap((sha) => [sha, sha.slice(0, 7), sha.slice(0, 8), sha.slice(0, 12)])
  );
}

function getPrBody(repo, prNumber) {
  return run("gh", [
    "pr",
    "view",
    String(prNumber),
    "--repo",
    repo,
    "--json",
    "body",
    "--jq",
    ".body",
  ]);
}

function extractCommitReferences(body) {
  const matches = body.matchAll(/\/commit\/([0-9a-f]{7,40})\b/giu);
  return [...new Set([...matches].map((match) => match[1].toLowerCase()))];
}

function checkPr({ repo, prNumber, baseTag, baseRef, allowedPrefixes }) {
  /*
   * ========================================================================
   * 步骤1：读取 release PR 正文
   * ========================================================================
   * 目标：拿到 release-please 生成的 changelog 内容
   * 数据源：GitHub PR body
   * 操作要点：
   *   1) 用 gh 读取指定 PR
   *   2) 从正文中的 commit 链接提取提交号
   */
  logger.info(`开始检查 release PR #${prNumber}...`);

  // 1.1 读取 PR 正文
  const body = getPrBody(repo, prNumber);

  // 1.2 提取 changelog 中引用的提交号
  const referencedCommits = extractCommitReferences(body);
  logger.info(`读取完成, 引用提交数: ${referencedCommits.length}`);

  /*
   * ========================================================================
   * 步骤2：校验提交范围
   * ========================================================================
   * 目标：阻止旧提交被重新塞进新版本 changelog
   * 数据源：上个版本 tag 到当前 baseRef 的 Git 提交范围
   * 操作要点：
   *   1) 每个正文引用提交都必须属于允许范围
   *   2) 发现越界提交立即失败
   */
  logger.info(`开始校验提交范围: ${baseTag}..${baseRef}`);

  // 2.1 找出不属于本轮发布范围的提交
  const staleCommits = referencedCommits.filter((sha) => !allowedPrefixes.has(sha));

  // 2.2 越界提交会让 workflow 失败
  if (staleCommits.length > 0) {
    throw new Error(
      [
        `Release PR #${prNumber} contains commits outside ${baseTag}..${baseRef}.`,
        `Stale commits: ${staleCommits.slice(0, 20).join(", ")}`,
        "Close the stale release PR, delete the release-please branch, then rerun release workflow.",
      ].join("\n")
    );
  }

  logger.info(`校验完成, PR #${prNumber} 未发现旧提交。`);
}

function main() {
  /*
   * ========================================================================
   * 步骤1：准备校验上下文
   * ========================================================================
   * 目标：确定待检查 PR 和合法提交范围
   * 数据源：命令行参数、release-please manifest、Git 历史
   * 操作要点：
   *   1) 从 action 输出或参数解析 PR 编号
   *   2) 从 manifest 推导上个发布 tag，首次发布可使用配置的提交基线
   */
  logger.info("开始准备 release PR 校验上下文...");

  // 1.1 解析命令行参数
  const args = parseArgs(process.argv.slice(2));

  // 1.2 解析待检查 PR 编号
  const prNumbers = getPrNumbers(args);

  if (prNumbers.length === 0) {
    logger.info("没有 release PR 需要检查。");
    return;
  }

  // 1.3 读取仓库和分支参数
  const repo = args.get("repo") ?? process.env.GITHUB_REPOSITORY;
  const baseRef = args.get("base-ref") ?? "HEAD";

  if (!repo) {
    throw new Error("Missing repo. Pass --repo or set GITHUB_REPOSITORY.");
  }

  // 1.4 推导上个版本 tag 和合法提交集合
  const baseTag = getReleaseBase({ baseRef, baseTag: args.get("base-tag") });
  const allowedPrefixes = getAllowedCommitPrefixes(baseTag, baseRef);
  logger.info(`上下文准备完成, PR 数: ${prNumbers.length}, 合法提交数: ${allowedPrefixes.size}`);

  /*
   * ========================================================================
   * 步骤2：逐个检查 release PR
   * ========================================================================
   * 目标：保证每个 release PR 只包含本轮提交
   * 数据源：GitHub PR body
   * 操作要点：
   *   1) 逐个读取 PR body
   *   2) 任一 PR 越界即失败
   */
  logger.info("开始逐个检查 release PR...");

  // 2.1 遍历所有 release PR
  for (const prNumber of prNumbers) {
    checkPr({ repo, prNumber, baseTag, baseRef, allowedPrefixes });
  }

  logger.info("所有 release PR 检查完成。");
}

if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  main();
}
