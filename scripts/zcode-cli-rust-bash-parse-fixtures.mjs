// Node oracle for the Rust `zcode-cli-bash-parse` crate.
//
// Runs the real `analyzeBashCommand` (unbash 4.0.1, resolved from packages/core) over a
// hand-written corpus plus a deterministic fuzz corpus. Run from the repo root:
//   node --import tsx scripts/zcode-cli-rust-bash-parse-fixtures.mjs
// which writes apps/zcode-cli-rust/crates/bash-parse/fixtures/{analysis,analysis-fuzz}.json.
// The static command lists live in that directory's corpus.json.
//
// unbash 4.0.1 loops forever on an ANSI-C string that ends in a lone backslash (e.g. `$'\`),
// so every analysis runs inside a vm timeout; commands that hang or throw are excluded from the
// fixtures and reported (the Rust crate treats them as parse errors, see its tests).
import { writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import vm from "node:vm";
import { format } from "oxfmt";
import {
  analyzeBashCommand,
  isBashCommandPermissionSafe,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-command-parser.ts";
import corpus from "../apps/zcode-cli-rust/crates/bash-parse/fixtures/corpus.json" with { type: "json" };

const FIXTURE_DIR = new URL("../apps/zcode-cli-rust/crates/bash-parse/fixtures/", import.meta.url);
const ORACLE_TIMEOUT_MS = 500;
const FUZZ_SEED = 0x5eed_2026;
const FUZZ_COUNT = 3000;
const CANARY = "a | b (c) <d >e && f";

export const NODE_HANGING_COMMANDS = corpus.nodeHangs;

const context = vm.createContext({
  analyzeBashCommand,
  isBashCommandPermissionSafe,
  input: "",
  out: "",
});
const oracleScript = new vm.Script(
  "out = JSON.stringify({ safe: isBashCommandPermissionSafe(a = analyzeBashCommand(input)), analysis: a })",
);
let canaryJson;

const wellFormed = (value) =>
  typeof value === "string"
    ? value.isWellFormed()
    : typeof value !== "object" || value === null || Object.values(value).every(wellFormed);

function runOracle(command) {
  const result = evaluate(command);
  if (!result.analysis || wellFormed(result.analysis)) return result;
  // unbash 在未闭合的 `${ …` / `$((…` 里按码元截断，可能切出孤立代理项；这类单词必然是动态的。
  // Rust 字符串无法表示孤立代理项，一旦出现在安全结果里就是无法复刻的差异，必须显式暴露。
  if (result.safe) throw new Error(`safe analysis with lone surrogate: ${JSON.stringify(command)}`);
  return { command, loneSurrogate: true };
}

function evaluate(command) {
  context.input = command;
  context.out = "";
  try {
    oracleScript.runInContext(context, { timeout: ORACLE_TIMEOUT_MS });
    return { command, ...JSON.parse(context.out) };
  } catch (error) {
    if (error?.code !== "ERR_SCRIPT_EXECUTION_TIMEOUT") return { command, throws: String(error) };
    // 超时终止可能打断 unbash 的 try/finally（readTestRegexWord 会临时改全局 charType），
    // 用金丝雀确认 oracle 状态仍然可信。
    if (JSON.stringify(analyzeBashCommand(CANARY)) !== canaryJson) {
      throw new Error(`oracle state corrupted after timeout on ${JSON.stringify(command)}`);
    }
    return { command, hang: true };
  }
}

/** Runs the oracle over `commands`; returns `{ fixtures, rejected }` (rejected = Node hangs/throws). */
export function collect(commands) {
  canaryJson ??= JSON.stringify(analyzeBashCommand(CANARY));
  const seen = new Set();
  const fixtures = [];
  const rejected = [];
  for (const command of commands) {
    if (seen.has(command)) continue;
    seen.add(command);
    const result = runOracle(command);
    if (result.hang || result.throws || result.loneSurrogate) rejected.push(result);
    else fixtures.push({ command, safe: result.safe, analysis: result.analysis });
  }
  return { fixtures, rejected };
}

function redirectMatrix() {
  const out = [];
  for (const op of corpus.redirectMatrix.operators) {
    for (const target of corpus.redirectMatrix.targets) out.push(`ls ${op}${target}`);
    out.push(`${op}f ls`, `ls ${op}f x`, `2${op}f ls`);
  }
  return out;
}

function longCommands() {
  const repeat = (text, count) => text.repeat(count);
  return [
    `echo ${repeat("a", 9990)}`,
    repeat("a", 10000),
    repeat("a", 10001),
    `echo ${repeat("é", 4990)}`,
    `echo ${repeat("😀", 2495)}`,
    `echo ${repeat("😀", 5000)}`,
    `ls${repeat(" | ls", 60)}`,
    `ls${repeat(" && ls", 60)}`,
    `echo ${repeat("x ", 300)}`,
    `echo ${repeat('"$(', 800)}`,
    `a=(${repeat('"$(', 800)}) ls`,
    `> ${repeat('"$(', 800)}`,
    `echo ${repeat("{", 3000)}`,
    `echo ${repeat("{a,", 1000)}`,
    `echo ${repeat("$(", 1500)}`,
    `echo ${repeat("'a' ", 300)}`,
    `cat ${repeat("<<E ", 60)}\n${repeat("x\nE\n", 60)}`,
    `echo ${repeat("a\\ ", 300)}`,
    `echo "${repeat('\\"', 300)}"`,
    `${repeat("A=1 ", 100)}ls`,
    `ls ${repeat("2>&1 ", 100)}`,
    `echo ${repeat("@(", 1000)}`,
    `echo ${repeat("(", 1000)}`,
    `${repeat("( ", 2000)}ls`,
    `${repeat("! ", 2000)}ls`,
    `echo ${repeat("`", 1001)}`,
    `echo ${repeat("${", 1000)}`,
  ];
}

export function handwrittenCommands() {
  const lists = corpus.handwritten;
  return [
    ...lists.basics,
    ...lists.quoting,
    ...lists.dollar,
    ...lists.literalish,
    ...lists.keywords,
    ...lists.operators,
    ...lists.bangTime,
    ...redirectMatrix(),
    ...lists.redirects,
    ...lists.heredocs,
    ...lists.assignments,
    ...lists.compound,
    ...lists.comments,
    ...lists.unicode,
    ...lists.misc,
    ...longCommands(),
    ...NODE_HANGING_COMMANDS,
  ];
}

// ---------------------------------------------------------------------------------------------
// Deterministic fuzz corpus
// ---------------------------------------------------------------------------------------------

function mulberry32(seed) {
  let state = seed | 0;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const pick = (rand, list) => list[Math.floor(rand() * list.length)];

function fuzzSimple(rand) {
  const pools = corpus.fuzz;
  const parts = [];
  if (rand() < 0.2) parts.push(pick(rand, pools.prefix));
  if (rand() < 0.1) parts.push(pick(rand, pools.redirects) + pick(rand, pools.targets));
  parts.push(pick(rand, pools.names));
  const words = Math.floor(rand() * 4);
  for (let i = 0; i < words; i++) parts.push(pick(rand, pools.words));
  if (rand() < 0.3) {
    const target = pick(rand, pools.targets);
    parts.push(pick(rand, pools.redirects) + (rand() < 0.5 ? " " : "") + target);
  }
  return parts.join(rand() < 0.9 ? " " : "");
}

function fuzzStructured(rand) {
  let command = fuzzSimple(rand);
  const extra = Math.floor(rand() * 3);
  for (let i = 0; i < extra; i++)
    command += ` ${pick(rand, corpus.fuzz.operators)} ${fuzzSimple(rand)}`;
  if (command.includes("<<") && rand() < 0.7) {
    command += `\n${pick(rand, corpus.fuzz.bodies)}\n${rand() < 0.8 ? "E" : "EOF"}`;
    if (rand() < 0.3) command += `\n${fuzzSimple(rand)}`;
  }
  return command;
}

function fuzzSoup(rand) {
  const { names, words, operators, redirects, targets, prefix } = corpus.fuzz;
  const pools = [names, words, operators, redirects, targets, prefix];
  const count = 1 + Math.floor(rand() * 8);
  let command = "";
  for (let i = 0; i < count; i++) {
    command += pick(rand, pick(rand, pools));
    command += pick(rand, [" ", " ", " ", "", "\t", "\n"]);
  }
  return command;
}

function mutate(rand, command) {
  // 按码点编辑，避免切出孤立代理项（Rust 的 &str 无法表示，也就不会成为真实输入）。
  let out = Array.from(command);
  const edits = Math.floor(rand() * 3);
  for (let i = 0; i < edits; i++) {
    const at = Math.floor(rand() * (out.length + 1));
    const op = rand();
    if (op < 0.6) out.splice(at, 0, pick(rand, corpus.fuzz.mutations));
    else if (op < 0.85) out.splice(at, 1);
    else out = out.slice(0, at);
  }
  return out.join("");
}

export function fuzzCommands(count = FUZZ_COUNT, seed = FUZZ_SEED) {
  const rand = mulberry32(seed);
  const commands = [];
  for (let i = 0; i < count; i++) {
    const base = rand() < 0.65 ? fuzzStructured(rand) : fuzzSoup(rand);
    commands.push(rand() < 0.45 ? mutate(rand, base) : base);
  }
  return commands;
}

// ---------------------------------------------------------------------------------------------
// Exports and fixture writer
// ---------------------------------------------------------------------------------------------

export function bashAnalysisFixtures() {
  return collect(handwrittenCommands()).fixtures;
}

export function bashAnalysisFuzzFixtures() {
  return collect(fuzzCommands()).fixtures;
}

function summarize(label, { fixtures, rejected }) {
  const safe = fixtures.filter((fixture) => fixture.safe).length;
  console.log(`${label}: ${fixtures.length} cases (${safe} safe), ${rejected.length} rejected`);
  for (const item of rejected) {
    console.log(
      `  ${item.hang ? "hang" : (item.throws ?? "lone surrogate")}: ${JSON.stringify(item.command).slice(0, 120)}`,
    );
  }
}

async function writeFixtures() {
  const hand = collect(handwrittenCommands());
  const handPath = new URL("analysis.json", FIXTURE_DIR);
  const formatted = await format(handPath.pathname, `${JSON.stringify(hand.fixtures, null, 2)}\n`);
  if (formatted.errors.length) throw new Error("Cannot format analysis.json");
  await writeFile(handPath, formatted.code);
  const fuzz = collect(fuzzCommands());
  // 模糊用例体积较大，与 command-registry.json 一样保持紧凑 JSON。
  await writeFile(new URL("analysis-fuzz.json", FIXTURE_DIR), `${JSON.stringify(fuzz.fixtures)}\n`);
  summarize("hand-written", hand);
  summarize("fuzz", fuzz);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  await writeFixtures();
