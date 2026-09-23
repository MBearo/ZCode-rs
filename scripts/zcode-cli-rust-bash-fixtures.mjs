// Bash permission parity data for the Rust `zcode-cli-bash` crate, computed by
// the TS implementations. Imported by generate-zcode-cli-rust-fixtures.mjs.
import { readFileSync } from "node:fs";
import * as callbacks from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-readonly-policy-callbacks.ts";
import {
  GIT_GLOBAL_DANGEROUS_FLAGS,
  GIT_GLOBAL_NO_VALUE_FLAGS,
  GIT_GLOBAL_VALUE_FLAGS,
  GIT_READONLY_SUBCOMMAND_POLICIES,
  READONLY_ALLOW_ANY_ARG_COMMAND_PREFIXES,
  READONLY_ALLOW_ANY_ARG_COMMANDS,
  READONLY_COMMAND_POLICIES,
  READONLY_MULTIWORD_COMMAND_POLICIES,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-readonly-policy-commands.ts";
import { analyzeBashCommand } from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-command-parser.ts";
import { resolveBashPermissionRulePolicy } from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-command-permission-policy.ts";
import { isRuntimeReadOnlyBashCommand } from "../apps/zcode-cli/packages/core/src/tool/handlers/bash-semantics.ts";
import {
  BASH_COMMAND_REGISTRY,
  BASH_COMMAND_REGISTRY_HASH,
  BASH_COMMAND_REGISTRY_VERSION,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/generated/bash-command-registry.ts";

const NAMED_CALLBACKS = new Map(
  Object.entries(callbacks)
    .filter(([, value]) => typeof value === "function")
    .map(([name, fn]) => [fn, name]),
);

function policy(prefix, p) {
  const callback = p.additionalCommandIsDangerousCallback;
  if (p.regex && p.regex.flags) throw new Error(`Unsupported regex flags for ${prefix}`);
  return {
    prefix,
    ...(p.allowAnyArgs ? { allowAnyArgs: true } : {}),
    ...(p.commandOnly ? { commandOnly: true } : {}),
    ...(p.allowCompactNumericCountFlag ? { allowCompactNumericCountFlag: true } : {}),
    ...(p.respectsDoubleDash === false ? { respectsDoubleDash: false } : {}),
    ...(p.safeFlags ? { safeFlags: { ...p.safeFlags } } : {}),
    ...(p.regex ? { regex: p.regex.source } : {}),
    // 内联匿名回调没有可导出的名字，按前缀命名，由 Rust 侧同名实现。
    ...(callback ? { callback: NAMED_CALLBACKS.get(callback) ?? `inline:${prefix}` } : {}),
  };
}

/** Resolved read-only policy tables, in Map insertion order. */
export function bashPolicyData() {
  return {
    commands: [...READONLY_COMMAND_POLICIES].map(([k, p]) => policy(k, p)),
    git: [...GIT_READONLY_SUBCOMMAND_POLICIES].map(([k, p]) => policy(k, p)),
    multiword: [...READONLY_MULTIWORD_COMMAND_POLICIES].map(([k, p]) => policy(k, p)),
    allowAnyArgCommands: [...READONLY_ALLOW_ANY_ARG_COMMANDS],
    allowAnyArgCommandPrefixes: READONLY_ALLOW_ANY_ARG_COMMAND_PREFIXES.map((p) => [...p]),
    gitGlobalNoValueFlags: [...GIT_GLOBAL_NO_VALUE_FLAGS],
    gitGlobalValueFlags: [...GIT_GLOBAL_VALUE_FLAGS],
    gitGlobalDangerousFlags: [...GIT_GLOBAL_DANGEROUS_FLAGS],
  };
}

/** The fig command registry used for stable "always allow" prefixes. */
export function bashRegistry() {
  return {
    version: BASH_COMMAND_REGISTRY_VERSION,
    hash: BASH_COMMAND_REGISTRY_HASH,
    registry: BASH_COMMAND_REGISTRY,
  };
}

function flagCases(prefix, safeFlags) {
  const out = [];
  const shortNone = [];
  for (const [flag, kind] of Object.entries(safeFlags ?? {})) {
    switch (kind) {
      case "none":
        out.push(`${prefix} ${flag} x`, `${prefix} ${flag}=x`);
        if (/^-[A-Za-z0-9]$/.test(flag)) shortNone.push(flag);
        break;
      case "number":
        out.push(`${prefix} ${flag} 3 x`, `${prefix} ${flag} x`, `${prefix} ${flag}=3`);
        if (/^-[A-Za-z0-9]$/.test(flag)) out.push(`${prefix} ${flag}3`);
        break;
      case "string":
        out.push(`${prefix} ${flag} v x`, `${prefix} ${flag} -v`, `${prefix} ${flag}=v`);
        if (/^-[A-Za-z0-9]$/.test(flag)) out.push(`${prefix} ${flag}v`, `${prefix} ${flag}-x`);
        out.push(`${prefix} ${flag}`);
        break;
      default:
        out.push(`${prefix} ${flag} , x`, `${prefix} ${flag} {} x`, `${prefix} ${flag} EOF x`);
    }
  }
  if (shortNone.length >= 2) out.push(`${prefix} -${shortNone[0][1]}${shortNone[1][1]}`);
  return out;
}

function tableCommands() {
  const commands = [];
  const sweep = (prefix, p) => {
    commands.push(prefix, `${prefix} x`, `${prefix} --zz-unknown`, `${prefix} -- --zz`);
    commands.push(`${prefix} -- x`, `${prefix} -9`, `${prefix} '' x`, `${prefix} $X`);
    commands.push(...flagCases(prefix, p.safeFlags));
  };
  for (const [name, p] of READONLY_COMMAND_POLICIES) sweep(name, p);
  for (const [prefix, p] of GIT_READONLY_SUBCOMMAND_POLICIES) sweep(prefix, p);
  for (const [prefix, p] of READONLY_MULTIWORD_COMMAND_POLICIES) sweep(prefix, p);
  for (const name of READONLY_ALLOW_ANY_ARG_COMMANDS) commands.push(`${name} --anything x`);
  return [...new Set(commands)];
}

// 手写语料：危险参数回调、包装命令、重定向、git 全局参数、建议前缀与规则求值的边界。
const CORPUS = JSON.parse(
  readFileSync(
    new URL("../apps/zcode-cli-rust/crates/bash/fixtures/corpus.json", import.meta.url),
    "utf8",
  ),
);

function ruleMatrix(command, analysis, suggestions) {
  const trimmed = command.trim();
  const argv = analysis.commands[0]?.argv ?? [];
  const contents = new Set();
  if (trimmed) contents.add(trimmed);
  for (const content of suggestions) contents.add(content);
  if (argv[0]) {
    contents.add(`${argv[0]}:*`);
    contents.add(`${argv[0]} *`);
    contents.add(`${argv[0]}*`);
  }
  if (argv[1]) contents.add(`${argv[0]} ${argv[1]}:*`);
  contents.add("zzz:*");
  const sets = [[""], ...[...contents].map((c) => [c])];
  if (suggestions.length > 1) sets.push(suggestions);
  return sets;
}

/** Oracle verdicts: read-only for every command, rule policy for the corpus. */
export function bashFixtures() {
  const readOnly = tableCommands().map((c) => [c, isRuntimeReadOnlyBashCommand(c, undefined)]);
  const policies = CORPUS.map((command) => {
    const analysis = analyzeBashCommand(command);
    let policy;
    try {
      policy = resolveBashPermissionRulePolicy({ command }, undefined);
    } catch {
      // Node 在原型链键（如 `constructor foo`）上抛异常；Rust 按规格退回精确规则。
      return { command, readOnly: isRuntimeReadOnlyBashCommand(command, undefined), throws: true };
    }
    const suggestions = policy.suggestedPermissionUpdates.flatMap((u) =>
      u.rules.map((r) => r.ruleContent),
    );
    const rules = [];
    for (const contents of ruleMatrix(command, analysis, suggestions)) {
      const values = contents.map((c) =>
        c === "" ? { toolName: "Bash" } : { toolName: "Bash", ruleContent: c },
      );
      for (const behavior of ["allow", "deny", "ask"])
        rules.push([behavior, contents, policy.evaluateRules(behavior, values)]);
    }
    return {
      command,
      readOnly: isRuntimeReadOnlyBashCommand(command, undefined),
      suggestions: policy.suggestedPermissionUpdates,
      rules,
    };
  });
  return { readOnly, policies };
}
