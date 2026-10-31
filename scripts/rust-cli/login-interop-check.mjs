#!/usr/bin/env node
// rust-cli 与 Node CLI 的登录互操作检查（docs/specs/rust-cli/platform.md §5.5、§5.6、§7）。
//
// 1. rust-cli 手工 key 登录（`zcode login zai-coding-plan-api-key`）→ Node 的凭据仓库读回同一份 key、身份与
//    来源，Node 判定 Coding Plan 已登录，Node 的个人配置仓库读回默认选择；
// 2. Node 持有 `credentials.json` 的目录锁 1.5 秒并在锁内读改写；其间 rust-cli 登录 bigmodel：rust-cli 必须
//    等锁释放，两边写的键都保留；
// 3. rust-cli 退出 → Node 判定未登录，与登录无关的键保留；
// 4. Node 手工 key 登录 → rust-cli 退出能删掉 Node 写的登录状态。
//
// 不连任何服务：全部在临时目录里，密钥是测试值。
// 用法：node scripts/rust-cli/login-interop-check.mjs [--node-root <装好依赖的检出>]
// 没有装依赖的检出时给出提示并以 2 退出；检查失败以 1 退出。
import { execFileSync, spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { binaryPath } from "./binary.mjs";
import { GLM_5_3_ACCOUNT } from "./fixture/model.mjs";
import {
  ensureTypeScript,
  importNode,
  nodeRootFromArgs,
  personalProviderCount,
  repoRoot,
  withHome,
} from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);

const manifest = join(repoRoot, "packages/rust-cli/Cargo.toml");
// 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
execFileSync("cargo", ["build", "-q", "--manifest-path", manifest, "-p", "rust-cli"], {
  cwd: dirname(manifest),
  stdio: "inherit",
});
const binary = binaryPath(repoRoot, "debug");

const { createSharedZCodeCredentialStore } = await importNode(
  nodeRoot,
  "adapters/src/auth/shared-credentials.ts",
);
const { configureCodingPlanApiKey, hasConfiguredStandaloneCodingPlan } = await importNode(
  nodeRoot,
  "bootstrap/src/auth-login.ts",
);
const { NodePersonalProviderConfigRepository } = await import(
  pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
);
const { withFileLock, atomicWritePrivateTextFile } = await import(
  pathToFileURL(join(nodeRoot, "packages/shared/src/node/privateFilePersistence.ts")).href
);

const failures = [];
const check = (label, ok, detail = "") => {
  console.log(`${ok ? "ok  " : "FAIL"} ${label}${ok || !detail ? "" : `: ${detail}`}`);
  if (!ok) failures.push(label);
};

const home = await mkdtemp(join(tmpdir(), "rust-cli-login-interop-"));
const env = {
  HOME: home,
  USERPROFILE: home,
  ZCODE_DATA_BASE_DIR: home,
  ZCODE_CREDENTIAL_SECRET: "interop-secret",
  // Node 独立 CLI 登录时要求显式的内置配置；rust-cli 用随包的同一份。
  ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: join(
    repoRoot,
    "packages/rust-cli/crates/platform/assets/zcode-builtin.json",
  ),
};
const credentialsPath = join(home, ".zcode/v2/credentials.json");
const store = createSharedZCodeCredentialStore({ env });
const ZAI = "account:zai-individual-coding-plan";
const BIGMODEL = "account:bigmodel-individual-coding-plan";

/** 运行 rust-cli，返回 { code, stdout, stderr, ms }。 */
function zcode(argv, stdin = "") {
  return new Promise((resolve, reject) => {
    const started = Date.now();
    const child = spawn(binary, argv, { env: { PATH: process.env.PATH, ...env } });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stdout, stderr, ms: Date.now() - started }));
    child.stdin.end(stdin);
  });
}

try {
  // 1. rust-cli 写，Node 读。
  const login = await zcode(["login", "zai-coding-plan-api-key"], "sk-rust\n");
  check("rust-cli manual login", login.code === 0, login.stderr);
  const identity = await store.load(`account-provider:${ZAI}:identity`);
  check("Node reads the identity", /^key-[0-9a-f]{24}$/.test(identity ?? ""), identity);
  check(
    "Node reads the source",
    (await store.load(`account-provider:${ZAI}:auth-source`)) === "manual",
  );
  const key = await store.load(
    `account-provider:coding-plan:${ZAI}:account:${encodeURIComponent(identity)}:api-key`,
  );
  check("Node reads the API key", key === "sk-rust", key);
  check("Node sees the Coding Plan login", await hasConfiguredStandaloneCodingPlan({ env }));
  const personal = new NodePersonalProviderConfigRepository({
    filePath: join(home, ".zcode/v2/provider_config.json"),
    pollingIntervalMs: false,
  });
  const selection = (await personal.read()).defaultModelSelection;
  personal.dispose();
  check(
    "Node reads the default selection",
    selection?.providerId === ZAI && selection?.modelId === GLM_5_3_ACCOUNT,
    JSON.stringify(selection),
  );

  // 2. Node 持锁期间 rust-cli 写：rust-cli 等锁，两边的写入都保留。
  let rustRun;
  await withFileLock(credentialsPath, async () => {
    const before = JSON.parse(await readFile(credentialsPath, "utf8"));
    rustRun = zcode(["login", "bigmodel-coding-plan-api-key"], "sk-rust-big\n");
    await sleep(1500);
    await atomicWritePrivateTextFile(
      credentialsPath,
      `${JSON.stringify({ ...before, "interop:node": "kept" }, null, 2)}\n`,
    );
  });
  const contended = await rustRun;
  check(
    "rust-cli waits for the Node lock",
    contended.code === 0 && contended.ms >= 1200,
    `${contended.ms}ms ${contended.stderr}`,
  );
  check("the Node write survives", (await store.load("interop:node")) === "kept");
  check(
    "the rust-cli write survives",
    (await store.load(`account-provider:${BIGMODEL}:auth-source`)) === "manual",
  );

  // 3. rust-cli 退出，Node 看到未登录，无关的键保留。
  const logout = await zcode(["logout"]);
  check("rust-cli logout", logout.code === 0, logout.stderr);
  check("Node sees no Coding Plan login", !(await hasConfiguredStandaloneCodingPlan({ env })));
  check("unrelated keys survive the logout", (await store.load("interop:node")) === "kept");

  // 4. Node 写，rust-cli 删。修复线 F50：Node 登录写个人配置时导入旧 CLI 用户文件，路径取本进程的 `os.homedir()`（不是
  // 传入的 env），会把本机真实的 provider（带 key）读进临时目录；登录期间本进程的 HOME 指向临时 home，运行 rust-cli
  // 前再查个人配置里没有 provider。
  await withHome(home, () =>
    configureCodingPlanApiKey({ apiKey: "sk-node", env, providerId: "zai" }),
  );
  const imported = await personalProviderCount(join(home, ".zcode/v2/provider_config.json"));
  check("the Node login imports no provider from outside the temp home", imported === 0, imported);
  if (imported !== 0) throw new Error("refusing to run rust-cli with imported providers");
  check("Node login is visible", await hasConfiguredStandaloneCodingPlan({ env }));
  const second = await zcode(["logout"]);
  check(
    "rust-cli removes the Node login",
    second.code === 0 && !(await hasConfiguredStandaloneCodingPlan({ env })),
  );
  const record = JSON.parse(await readFile(credentialsPath, "utf8"));
  check(
    "only the unrelated key is left",
    JSON.stringify(Object.keys(record)) === JSON.stringify(["interop:node"]),
    JSON.stringify(Object.keys(record)),
  );
} finally {
  await rm(home, { recursive: true, force: true });
}

if (failures.length) {
  console.error(`${failures.length} check(s) failed`);
  process.exit(1);
}
console.log("rust-cli login interop: OK");
