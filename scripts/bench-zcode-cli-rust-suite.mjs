// Serial paired runs avoid comparing different concurrent build/test loads.
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";

const [
  baseline,
  candidate,
  destination = ".zcode-runtime/rust-bench/requests",
  candidateContextWindow,
  baselineContextWindow,
  candidateApiType,
  baselineApiType,
] = process.argv.slice(2);
if (!baseline || !candidate)
  throw new Error(
    "Usage: node scripts/bench-zcode-cli-rust-suite.mjs <baseline> <candidate> [output-directory] [candidate-window] [baseline-window] [candidate-api] [baseline-api]",
  );
const directory = resolve(destination);
await mkdir(directory, { recursive: true });
const scenarios = [
  { name: "stream", args: [8, 1, 2048] },
  { name: "history", args: [100, 1, 64] },
  { name: "sessions", args: [8, 4, 512] },
];
const results = [];
for (const scenario of scenarios) {
  for (let repetition = 1; repetition <= 5; repetition++) {
    const versions =
      repetition % 2
        ? [
            ["baseline", baseline],
            ["candidate", candidate],
          ]
        : [
            ["candidate", candidate],
            ["baseline", baseline],
          ];
    for (const [version, binary] of versions) {
      const contextWindow =
        version === "candidate" ? candidateContextWindow : baselineContextWindow;
      const apiType = version === "candidate" ? candidateApiType : baselineApiType;
      const sample = await new Promise((done, fail) => {
        const child = spawn(
          process.execPath,
          [
            resolve("scripts/bench-zcode-cli-rust.mjs"),
            binary,
            ...scenario.args.map(String),
            ...(contextWindow || apiType ? [contextWindow ?? "256000"] : []),
            ...(apiType ? [apiType] : []),
          ],
          { stdio: ["ignore", "pipe", "pipe"] },
        );
        let output = "",
          stderr = "";
        child.stdout.setEncoding("utf8").on("data", (s) => {
          output += s;
        });
        child.stderr.setEncoding("utf8").on("data", (s) => {
          stderr += s;
        });
        child.once("error", fail);
        child.once("close", (code) => {
          if (code) fail(new Error(`${version}/${scenario.name}: ${stderr}`));
          else {
            try {
              done(JSON.parse(output));
            } catch (error) {
              fail(error);
            }
          }
        });
      });
      await writeFile(
        join(directory, `${scenario.name}-${version}-${repetition}.json`),
        `${JSON.stringify(sample, null, 2)}\n`,
      );
      results.push({ scenario: scenario.name, version, repetition, ...sample });
      console.log(`${scenario.name} ${version} ${repetition}/5: ${Math.round(sample.totalMs)} ms`);
    }
  }
}
function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}
const summary = scenarios.map(({ name }) => ({
  scenario: name,
  ...Object.fromEntries(
    ["baseline", "candidate"].map((version) => {
      const samples = results.filter((r) => r.scenario === name && r.version === version);
      return [
        version,
        {
          repetitions: samples.length,
          ...Object.fromEntries(
            [
              "startupMs",
              "totalMs",
              "rpcP95Ms",
              "steadyRpcP95Ms",
              "idleRssKiB",
              "finalRssKiB",
              "sampledPeakRssKiB",
              "protocolFrames",
              "storageBytes",
              "durableStorageBytes",
            ].map((key) => [key, median(samples.map((s) => s[key]))]),
          ),
          firstTurnTtftMs: median(
            samples.map((s) => s.samples.find((t) => t.session === 0 && t.turn === 1).ttftMs),
          ),
          steadyTtftMs: median(
            samples.flatMap((s) => s.samples.filter((t) => t.turn > 1).map((t) => t.ttftMs)),
          ),
        },
      ];
    }),
  ),
}));
await writeFile(join(directory, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
console.log(`Results: ${directory}`);
