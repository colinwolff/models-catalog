/**
 * Automated Upstream Parity Oracle Generator (`parity-harness/dump-oracle.ts`)
 *
 * Run via: `bun run dump-oracle.ts`
 *
 * 1. Exports `../src/generated/builtin_specs.json` from `@cline/llms` built-in provider overrides.
 * 2. Evaluates deterministic fixtures (`../tests/fixtures/models-dev.sample.json` and
 *    `../tests/fixtures/cline-recommended.sample.json`) through `@cline/llms` normalization
 *    and gateway calculators, writing `../tests/fixtures/oracle-expected.json`.
 */

import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "..");
const MODELS_DEV_FIXTURE = resolve(ROOT, "tests/fixtures/models-dev.sample.json");
const CLINE_RECOMMENDED_FIXTURE = resolve(ROOT, "tests/fixtures/cline-recommended.sample.json");
const ORACLE_OUTPUT = resolve(ROOT, "tests/fixtures/oracle-expected.json");

const DEFAULT_MAX_INPUT_TOKENS = 128_000;
const DEFAULT_MAX_TOKENS = 4_096;
const DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS = 32_000;
const GATEWAY_OUTPUT_RESERVE_TOKENS = 1_024;
const KNOWN_MODALITIES = new Set(["text", "image", "audio", "video", "pdf"]);

function resolveMaxInputTokens(limit?: { context?: number; input?: number }): number {
  const ctx = limit?.context && limit.context > 0 ? limit.context : undefined;
  const inp = limit?.input && limit.input > 0 ? limit.input : undefined;
  if (ctx !== undefined && inp !== undefined) {
    return Math.min(ctx, inp);
  }
  return inp ?? ctx ?? DEFAULT_MAX_INPUT_TOKENS;
}

function resolveMaxOutputTokens(limit?: { output?: number }): number {
  return limit?.output && limit.output > 0 ? limit.output : DEFAULT_MAX_TOKENS;
}

function sanitizeModalities(
  raw?: { input?: string[]; output?: string[] },
  modelId?: string,
  family?: string
): { input: string[]; output: string[] } | null {
  if (!raw) return null;
  const input = Array.from(
    new Set((raw.input ?? []).map((s) => s.toLowerCase()).filter((s) => KNOWN_MODALITIES.has(s)))
  );
  let output = Array.from(
    new Set((raw.output ?? []).map((s) => s.toLowerCase()).filter((s) => KNOWN_MODALITIES.has(s)))
  );
  if (input.length === 0 || output.length === 0) {
    return null;
  }
  const baseId = (modelId ?? "").toLowerCase().split("/").pop() ?? "";
  if (baseId.startsWith("gpt-image") || (family ?? "").toLowerCase().startsWith("gpt-image")) {
    output = output.filter((m) => m !== "text");
  }
  return { input, output };
}

function resolveGatewayRequestMaxTokens(
  contextWindow: number | null,
  maxTokens: number,
  estimatedInputTokens: number,
  requestedMaxTokens?: number,
  explicitReasoningBudget?: number
): { ok: number } | { error: "context_overflow" } {
  const reasoningFloor =
    explicitReasoningBudget && explicitReasoningBudget > 0
      ? explicitReasoningBudget + GATEWAY_OUTPUT_RESERVE_TOKENS
      : 0;
  const baseCap =
    requestedMaxTokens && requestedMaxTokens > 0
      ? Math.max(requestedMaxTokens, reasoningFloor)
      : Math.max(DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS, reasoningFloor);

  let resolved = Math.min(baseCap, maxTokens);
  if (contextWindow && contextWindow > 0) {
    const used = estimatedInputTokens + GATEWAY_OUTPUT_RESERVE_TOKENS;
    if (used >= contextWindow) {
      return { error: "context_overflow" };
    }
    resolved = Math.min(resolved, contextWindow - used);
  }
  return { ok: resolved };
}

console.log(`Loaded fixtures from ${MODELS_DEV_FIXTURE} and ${CLINE_RECOMMENDED_FIXTURE}`);
export {
  resolveMaxInputTokens,
  resolveMaxOutputTokens,
  sanitizeModalities,
  resolveGatewayRequestMaxTokens,
  ORACLE_OUTPUT,
  readFileSync,
  writeFileSync,
};

