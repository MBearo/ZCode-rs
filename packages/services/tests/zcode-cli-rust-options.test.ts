import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";
import { compileModelOptionMap } from "@zcode/model-option-map";

test("Native option maps agree with TS for every builtin expression and strict-language edge case", async () => {
  const builtin = JSON.parse(await readFile("config/provider/zcode-builtin.json", "utf8"));
  const cases: {
    source: string;
    variable: "reasoningLevel" | "maxOutputTokens";
    input: string | number;
  }[] = [];
  for (const rules of Object.values(builtin.config.modelConfigRules) as any[][])
    for (const rule of rules) {
      for (const variable of ["reasoningLevel", "maxOutputTokens"] as const) {
        const spec = rule.config.optionSpecs?.[variable];
        if (!spec?.map) continue;
        for (const input of variable === "reasoningLevel"
          ? (spec.values ?? ["none", "low", "high"])
          : [1, 123, spec.max ?? 8192])
          cases.push({ source: spec.map, variable, input });
      }
    }
  for (const source of [
    "{'a': maxOutputTokens == 1.0, 'b': [1] == [1.0], 'c': {'a': 1} == {'a': 1.0}}",
    "{'n': (maxOutputTokens + 2) * 3 / 2 % 3, 's': 'a' + '中'}",
    "false && (1 / 0 > 0) ? {} : {'ok': true || (1 / 0 > 0)}",
    "maxOutputTokens > 0 ? {'a': null} : {'b': []}",
    "{'x': '\\ud83d\\ude80', 'order': '🚀' < '\\ue000'}",
    "{'x': 1 / 0}",
    "{'x': maxOutputTokens + '1'}",
    "{'x': 9007199254740992}",
    "{'x': null.foo}",
    "maxOutputTokens ? {} : {}",
    "true ? {} : []",
    "{'x': unknown}",
    "{'x': 1e3}",
    "{'x': +1, 'y': -0}",
  ])
    cases.push({ source, variable: "maxOutputTokens", input: 1 });
  const expected = cases.map((c) => {
    try {
      return {
        value: JSON.parse(
          JSON.stringify(compileModelOptionMap(c.source, c.variable).evaluate(c.input)),
        ),
      };
    } catch {
      return { error: true };
    }
  });
  assert.ok(cases.length > 50);
  const child = spawn(
    resolve(
      `apps/zcode-cli-rust/target/debug/examples/option_map${process.platform === "win32" ? ".exe" : ""}`,
    ),
    [],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  let stdout = "";
  let stderr = "";
  child.stdout.on("data", (d) => (stdout += d));
  child.stderr.on("data", (d) => (stderr += d));
  const exited = new Promise<void>((done, fail) => {
    child.on("error", fail);
    child.on("close", (code) => (code === 0 ? done() : fail(new Error(stderr))));
  });
  child.stdin.end(cases.map((c) => JSON.stringify(c)).join("\n") + "\n");
  await exited;
  const actual = stdout
    .trim()
    .split("\n")
    .map((s) => JSON.parse(s));
  assert.equal(actual.length, cases.length);
  actual.forEach((value, i) => assert.deepEqual(value, expected[i], cases[i]!.source));
});
