#!/usr/bin/env node
// Runs the extract-skills and generate-llms-txt subcommands of the
// workspace's hermes-parity-tools crate before docusaurus build/start so that:
//   - website/src/data/skills.json (imported by src/pages/skills/index.tsx)
//   - website/static/llms.txt (agent-friendly short docs index)
//   - website/static/llms-full.txt (full docs concat for LLM context)
// all exist without contributors remembering to run the generators manually.
//
// We also try to pull a fresh copy of skills-index.json from the live docs
// site if it's not already on disk. That keeps local builds fast while still
// giving the Skills Hub the same external-catalog metadata as production.
//
// If cargo isn't available on the local machine, we fall back to writing an
// empty skills.json so `npm run build` still succeeds — the Skills Hub page
// just shows an empty state, and llms.txt generation is skipped.

import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync, existsSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const websiteDir = resolve(scriptDir, "..");
const repoRoot = resolve(websiteDir, "..");
const outputFile = join(websiteDir, "src", "data", "skills.json");
const metaFile = join(websiteDir, "src", "data", "skills-meta.json");
const unifiedIndexFile = join(websiteDir, "static", "api", "skills-index.json");
const UNIFIED_INDEX_URL =
  "https://hermes-agent.nousresearch.com/docs/api/skills-index.json";
const UNIFIED_INDEX_MAX_AGE_MS = 24 * 60 * 60 * 1000;

function writeEmptyFallback(reason) {
  mkdirSync(dirname(outputFile), { recursive: true });
  writeFileSync(outputFile, "[]\n");
  writeFileSync(
    metaFile,
    JSON.stringify(
      {
        extractedAt: new Date().toISOString(),
        indexGeneratedAt: "",
        totalSkills: 0,
        externalSource: "fallback-empty",
        indexHealth: { status: "degraded", detail: reason },
        bySource: {},
      },
      null,
      2,
    ) + "\n",
  );
  console.warn(
    `[prebuild] extract-skills skipped (${reason}); wrote empty skills.json and skills-meta.json. ` +
      `Install a Rust toolchain (cargo) locally for a populated Skills Hub page.`,
  );
}

// Runs one hermes-parity-tools subcommand against the repository root.
// Returns "ok", "missing" (cargo not found) or the failure status.
function runTool(command) {
  const r = spawnSync(
    "cargo",
    ["run", "--locked", "-q", "-p", "hermes-parity-tools", "--", command, "--repo-root", repoRoot],
    { stdio: "inherit", cwd: repoRoot },
  );
  if (r.error && r.error.code === "ENOENT") return "missing";
  if (r.status !== 0) return `exited with status ${r.status}`;
  return "ok";
}

async function ensureUnifiedIndex() {
  if (existsSync(unifiedIndexFile)) {
    try {
      const age = Date.now() - statSync(unifiedIndexFile).mtimeMs;
      if (age < UNIFIED_INDEX_MAX_AGE_MS) {
        return true;
      }
      console.log(
        `[prebuild] skills-index.json is ${(age / 3600000).toFixed(1)}h old; ` +
          `refreshing from ${UNIFIED_INDEX_URL}`,
      );
    } catch {
      // Fall through to re-fetch.
    }
  }

  try {
    const resp = await fetch(UNIFIED_INDEX_URL, {
      headers: { accept: "application/json" },
    });
    if (!resp.ok) {
      console.warn(
        `[prebuild] skills-index.json fetch returned HTTP ${resp.status}; using local copy if any`,
      );
      return existsSync(unifiedIndexFile);
    }
    const text = await resp.text();
    try {
      const parsed = JSON.parse(text);
      if (!parsed || !Array.isArray(parsed.skills)) {
        console.warn("[prebuild] skills-index.json from live site has no skills array; ignoring");
        return existsSync(unifiedIndexFile);
      }
    } catch (e) {
      console.warn(`[prebuild] skills-index.json from live site is not valid JSON: ${e}`);
      return existsSync(unifiedIndexFile);
    }
    mkdirSync(dirname(unifiedIndexFile), { recursive: true });
    writeFileSync(unifiedIndexFile, text);
    console.log(
      `[prebuild] downloaded skills-index.json from ${UNIFIED_INDEX_URL} ` +
        `(${(text.length / 1024).toFixed(0)} KB)`,
    );
    return true;
  } catch (e) {
    console.warn(`[prebuild] skills-index.json fetch failed: ${e}`);
    return existsSync(unifiedIndexFile);
  }
}

// 0) Pull unified index if we do not have a fresh one.
await ensureUnifiedIndex();

// 1) skills.json — required for the Skills Hub page.
const extract = runTool("extract-skills");
if (extract === "missing") {
  writeEmptyFallback("cargo not found");
} else if (extract !== "ok") {
  writeEmptyFallback(`extract-skills ${extract}`);
}

// 2) llms.txt + llms-full.txt — agent-friendly docs entrypoints. Non-fatal.
const llms = extract === "missing" ? "missing" : runTool("generate-llms-txt");
if (llms !== "ok") {
  console.warn(`[prebuild] generate-llms-txt skipped (${llms === "missing" ? "cargo not found" : llms})`);
}
