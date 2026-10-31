// rust-cli 与 Node 独立 CLI、App 的 Team Plan / z.ai 登录互操作（docs/specs/rust-cli/platform.md §9.4、§9.8，工作线 F47、
// F49）。
// node-interop-check.mjs 的第十四段。全部是本地假服务与临时 HOME，不连任何真实服务：
//
// A. rust-cli 登录 zai（真实的 `zcode login zai --no-browser --json`）→ `oauth:zai:access_token` 存的是业务 JWT；
//    Node 独立 CLI 的换证（`createCodingPlanApiKeyResolver`）读到它，会把它当 OAuth token 再
//    `POST /api/auth/z/login` 一次（记录下来：真实服务接不接受未验证）。
// B. Node 独立 CLI 登录 zai（真实的 `loginZCodeCli`，网络是注入的假 HttpClient）→ 存原始 OAuth token；
//    rust-cli（真实的 `zcode -p`）读到它直接当业务 JWT 用：不再换，customerInfo 收到原始 token（假服务按真实服务
//    的预期拒绝它），个人套餐不可用（F50：取目录时的订阅查询换不到项目令牌）、要重新登录。
// C. App 的 `settingService`（带锁与 CAS 的写者）选团队 / 切回个人 → rust-cli 的 `zcode -p` 不跟随（F49：不读 App 的
//    当前连接）：两次都用默认的个人套餐；`--model <团队>` 用团队项目令牌，与 App 选了什么无关。
// D. 团队模型的会话（F49，platform.md §9.4）：rust-cli（真实的 `zcode -p --model <团队>[p-u]`）写会话 → Node 读出模型
//    选择（团队 provider，Node 的 strict schema 能解析）、每条消息过 App 协议 schema、Node 用自己的写入代码续写一轮
//    → rust-cli 读回，团队项目仍是 p-u；Node 换了模型 → rust-cli 不再套用这个团队项目。
import { execFileSync, spawn } from "node:child_process";
import { mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { BINARY_NAME, binaryPath } from "./binary.mjs";
import { appSchemaRejections } from "./node-interop-history.mjs";
import { importNode, personalProviderCount, withHome } from "./node-root.mjs";
import { GLM_5_3_ACCOUNT, GLM_5_3_FLASH_ACCOUNT } from "./fixture/model.mjs";

const SSE = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}\n\n',
  'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
].join("");

/** 业务 JWT 的形状：`header.<{exp, sub}>.signature`（sub 是换它的 token，便于看出谁换了什么）。 */
function businessJwt(token) {
  const exp = Math.floor(Date.now() / 1000) + 7200;
  const payload = Buffer.from(JSON.stringify({ exp, sub: token })).toString("base64url");
  return `header.${payload}.signature`;
}

/** 账号服务与模型的回复（rust-cli 的真实 HTTP 与 Node 的注入 HttpClient 共用）。 */
function createService() {
  const seen = [];
  const state = { oauthToken: "zai-oauth-1", issued: 0 };
  const now = () => Math.floor(Date.now() / 1000);
  const ok = (data) => ({ status: 200, body: JSON.stringify({ code: 200, data }) });
  function reply({ method, url, headers, body }) {
    const path = new URL(url).pathname.replace(/^\/+/, "/");
    const auth = headers.authorization ?? headers.Authorization ?? null;
    seen.push({ method, path, headers, body });
    if (path === "/api/v1/oauth/cli/init") {
      const data = {
        flow_id: "f1",
        authorize_url: "https://open.example.com/authorize?flow=f1",
        expires_at: now() + 600,
        poll_interval_sec: 1,
      };
      return { status: 200, body: JSON.stringify({ code: 0, data }) };
    }
    if (path === "/api/v1/oauth/cli/poll/f1") {
      return {
        status: 200,
        body: JSON.stringify({
          code: 0,
          data: {
            status: "ready",
            token: "zcode-jwt",
            user: { user_id: "u-1", name: "Dev" },
            zai: { access_token: state.oauthToken },
          },
        }),
      };
    }
    if (path === "/api/auth/z/login") {
      const token = JSON.parse(body || "{}").token;
      return {
        status: 200,
        body: JSON.stringify({ code: 0, data: { access_token: businessJwt(token) } }),
      };
    }
    if (path.startsWith("/api/biz/")) {
      // 真实服务不认原始 OAuth token：/api/biz/ 只收业务 JWT（zai）或 access token（bigmodel，这里也是 JWT 形状）。
      const raw = (auth ?? "").replace(/^Bearer\s+/i, "");
      if (!raw.startsWith("header.")) return { status: 401, body: JSON.stringify({ code: 401 }) };
    }
    if (path === "/api/biz/customer/getCustomerInfo") {
      return ok({
        organizations: [
          {
            organizationId: "org-1",
            organizationName: "默认机构",
            projects: [{ projectId: "p-1", projectName: "默认项目", projectType: "1" }],
          },
          {
            organizationId: "org-t",
            organizationName: "团队",
            projects: [
              { projectId: "p-t", projectType: 2 },
              { projectId: "p-u", projectType: 2 },
            ],
          },
        ],
      });
    }
    if (path === "/api/biz/subscription/list") {
      // 个人订阅（工作线 F50，照 App）：有效的 Coding Plan。
      return ok([{ productName: "GLM Coding Plan", status: "VALID", inCurrentPeriod: true }]);
    }
    if (path === "/api/biz/subscription/enterprise/v2/pricing") {
      return ok({
        productList: [
          { productId: "prod-1", tier: "PRO", subscribed: true, organizationId: "org-t" },
        ],
      });
    }
    if (path === "/api/biz/team/subscribe/product/querySubscribeDetail") {
      return ok({ hasSubscription: true, status: "EFFECTIVE", memberGrantStatus: "VALID" });
    }
    if (path === "/api/biz/v1/organization/org-1/projects/p-1/api_keys") {
      return ok([{ name: "zcode-api-key", apiKey: "key-1" }]);
    }
    if (path === "/api/biz/v1/organization/org-t/projects/p-t/api_keys") {
      return ok([{ name: "zcode-team-api-key", apiKey: "key-team", keyType: 2 }]);
    }
    if (path === "/api/biz/v1/organization/org-t/projects/p-u/api_keys") {
      return ok([{ name: "zcode-team-api-key", apiKey: "key-team-u", keyType: 2 }]);
    }
    if (path.endsWith("/access_tokens")) {
      state.issued += 1;
      return ok({
        accessToken: `pat-${state.issued}`,
        tokenType: "Bearer",
        expiresIn: 3600,
        expiresAt: now() + 3600,
      });
    }
    if (path.endsWith("/v1/messages")) return { status: 200, body: SSE, type: "text/event-stream" };
    return { status: 404, body: JSON.stringify({ code: 404 }) };
  }
  return { seen, state, reply };
}

/** 给 rust-cli 的真实 HTTP 服务。 */
async function listen(service) {
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      const headers = Object.fromEntries(
        Object.entries(request.headers).map(([name, value]) => [name.toLowerCase(), String(value)]),
      );
      const reply = service.reply({
        method: request.method,
        url: `http://127.0.0.1${request.url}`,
        headers,
        body,
      });
      response.writeHead(reply.status, {
        "content-type": reply.type ?? "application/json",
        connection: "close",
      });
      response.end(reply.body);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return { server, url: `http://127.0.0.1:${server.address().port}` };
}

/** Node 的 HttpClientPort 替身：同一份回复，不出网络（Node 独立 CLI 的 zai 地址写死 https://api.z.ai）。 */
function nodeHttpClient(service) {
  return {
    async request(request) {
      const headers = Object.fromEntries(
        Object.entries(request.headers ?? {}).map(([name, value]) => [name.toLowerCase(), value]),
      );
      const body = request.body ? new TextDecoder().decode(request.body) : "";
      const reply = service.reply({
        method: request.method ?? "GET",
        url: request.url,
        headers,
        body,
      });
      const bytes = new TextEncoder().encode(reply.body);
      return {
        url: request.url,
        status: reply.status,
        statusText: "",
        headers: {},
        body: bytes,
        bytes: bytes.length,
        durationMs: 0,
      };
    },
  };
}

/** 内置配置：账号 provider 的模型地址都指向假服务（rust-cli 与 Node 都不会连真实模型）。 */
async function builtinConfig(repoRoot, url, file) {
  const builtin = JSON.parse(
    await readFile(
      join(repoRoot, "packages/rust-cli/crates/platform/assets/zcode-builtin.json"),
      "utf8",
    ),
  );
  for (const rule of builtin.config.providerConfigRules.providerRules) {
    if (rule.config?.access?.type === "zhipu-account")
      rule.config.api.baseUrl = `${url}/api/anthropic`;
  }
  await writeFile(file, JSON.stringify(builtin));
  return file;
}

function run(binary, args, env, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env, cwd });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (code) => resolve({ code, stdout, stderr }));
    child.stdin.end();
  });
}

export async function checkTeam({ nodeRoot, repoRoot, base, expectEqual, zcode, rust }) {
  const service = createService();
  const { server, url } = await listen(service);
  const findings = {};
  try {
    const { createSharedZCodeCredentialStore } = await importNode(
      nodeRoot,
      "adapters/src/auth/shared-credentials.ts",
    );
    const { createCodingPlanApiKeyResolver } = await importNode(
      nodeRoot,
      "adapters/src/auth/coding-plan-api-key.ts",
    );
    const { loginZCodeCli } = await importNode(nodeRoot, "bootstrap/src/auth-login.ts");
    const homeEnv = async (name) => {
      const home = join(base, name);
      await mkdir(join(home, "work/.git"), { recursive: true });
      return {
        home,
        env: {
          HOME: home,
          USERPROFILE: home,
          PATH: process.env.PATH,
          ZCODE_DATA_BASE_DIR: home,
          ZCODE_CREDENTIAL_SECRET: "interop-secret",
          ZCODE_BASE_URL: url,
          ZAI_BUSINESS_BASE_URL: url,
          BIGMODEL_API_BASE_URL: url,
          ZCODE_BUILTIN_PROVIDER_CONFIG_FILE: await builtinConfig(
            repoRoot,
            url,
            join(home, "builtin.json"),
          ),
        },
      };
    };

    // A. rust-cli 登录，Node 读。
    {
      const { home, env } = await homeEnv("team-a");
      const login = await run(zcode, ["login", "zai", "--no-browser", "--json"], env, home);
      expectEqual("team A: rust-cli zai login --json", 0, login.code === 0 ? 0 : login.stderr);
      const result = login.code === 0 ? JSON.parse(login.stdout) : {};
      expectEqual(
        "team A: login JSON",
        { status: "ready", provider: "zai", user: { user_id: "u-1", name: "Dev" } },
        {
          status: result.status,
          provider: result.provider,
          user: result.user,
        },
      );
      const store = createSharedZCodeCredentialStore({ env });
      const stored = await store.load("oauth:zai:access_token");
      if (!stored) throw new Error(`team A: rust-cli login wrote no zai token: ${login.stderr}`);
      // 假服务换出的业务 JWT 在载荷里记着换它的 token：存的是用 OAuth token 换来的业务 JWT。
      const sub = (token) => {
        try {
          return JSON.parse(Buffer.from(token.split(".")[1], "base64url")).sub;
        } catch {
          return null;
        }
      };
      expectEqual(
        "team A: rust-cli stores the business JWT exchanged from the OAuth token",
        "zai-oauth-1",
        sub(stored ?? ""),
      );
      const before = service.seen.length;
      const resolver = createCodingPlanApiKeyResolver({ httpClient: nodeHttpClient(service) });
      const material = await resolver.resolveMaterial({
        accessToken: stored,
        family: "zai",
        accountIdentity: "u-1",
      });
      const exchange = service.seen
        .slice(before)
        .find((request) => request.path === "/api/auth/z/login");
      // Node 独立 CLI 把业务 JWT 当 OAuth token 再换一次（platform.md §9.8，真实服务接不接受未验证）。
      findings.nodeReExchangesTheBusinessJwt = exchange
        ? JSON.parse(exchange.body).token === stored
        : false;
      expectEqual(
        "team A: Node re-exchanges the business JWT",
        true,
        findings.nodeReExchangesTheBusinessJwt,
      );
      expectEqual(
        "team A: Node gets a project token from the fake service",
        "pat-",
        material.token.slice(0, 4),
      );
    }

    // B. Node 独立 CLI 登录，rust-cli 读。
    {
      const { home, env } = await homeEnv("team-b");
      service.state.oauthToken = "zai-oauth-raw";
      // 修复线 F50 查出的隔离缺口：Node 登录写个人配置时导入旧 CLI 用户文件，路径取本进程的 `os.homedir()`（不是
      // 传入的 env），以前会把本机真实的旧 provider（带 key）导入临时 home。F50 之后个人套餐在这里不可用，`zcode -p`
      // 会落到导入的 provider 上真的联网。登录期间本进程的 HOME 指向临时 home（`withHome`），运行 rust-cli 前再查。
      await withHome(home, () =>
        loginZCodeCli({
          env,
          providerId: "zai",
          noBrowser: true,
          httpClient: nodeHttpClient(service),
          pollToken: "a".repeat(64),
          sleep: async () => {},
        }),
      );
      const imported = await personalProviderCount(join(home, ".zcode/v2/provider_config.json"));
      expectEqual(
        "team B: the Node login imports no provider from outside the temp home",
        0,
        imported,
      );
      if (imported !== 0)
        throw new Error("team B: refusing to run rust-cli with imported providers");
      const store = createSharedZCodeCredentialStore({ env });
      expectEqual(
        "team B: Node stores the raw OAuth token",
        "zai-oauth-raw",
        await store.load("oauth:zai:access_token"),
      );
      const before = service.seen.length;
      const print = await run(zcode, ["-p", "hello"], env, join(home, "work"));
      const rustRequests = service.seen.slice(before);
      const customer = rustRequests.find(
        (request) => request.path === "/api/biz/customer/getCustomerInfo",
      );
      findings.rustUsesTheRawTokenDirectly = customer?.headers.authorization === "zai-oauth-raw";
      expectEqual(
        "team B: rust-cli does not exchange again",
        0,
        rustRequests.filter((r) => r.path === "/api/auth/z/login").length,
      );
      expectEqual(
        "team B: rust-cli sends the stored token as the business JWT",
        true,
        findings.rustUsesTheRawTokenDirectly,
      );
      // 工作线 F50：个人套餐取目录时就换项目令牌、查订阅（照 App），换取被拒是 unknown、第一次不可用，所以 `-p`
      // 选不出模型（以前在请求时报 project_token_request_failed）。都要重新登录。
      expectEqual(
        "team B: the request fails (log in again)",
        true,
        print.code !== 0 && /no model provider configured/.test(print.stdout + print.stderr),
      );
      expectEqual(
        "team B: no model request",
        0,
        rustRequests.filter((r) => r.path.endsWith("/v1/messages")).length,
      );
    }

    // C. App 选团队 / 切回个人，rust-cli 跟随。
    {
      const { home, env } = await homeEnv("team-c");
      const store = createSharedZCodeCredentialStore({ env });
      await store.saveMany({
        "oauth:active_provider": "bigmodel",
        "oauth:bigmodel:access_token": "header.bigmodel-access.signature",
        "oauth:bigmodel:user_info": JSON.stringify({
          id: "key-1",
          username: "dev",
          displayName: "Dev",
        }),
        zcodejwttoken: "zcode-jwt",
        "account-provider:account:bigmodel-individual-coding-plan:identity": "key-1",
        "account-provider:account:bigmodel-individual-coding-plan:auth-source": "oauth",
      });
      // App 的设置写者在创建时取 home（ZCODE_DESKTOP_HOME_DIR 优先），只指向临时目录。
      const previous = process.env.ZCODE_DESKTOP_HOME_DIR;
      process.env.ZCODE_DESKTOP_HOME_DIR = home;
      let settings;
      try {
        const { createSettingServiceWithMigrations } = await import(
          pathToFileURL(join(nodeRoot, "packages/services/src/setting/settingService.ts")).href
        );
        settings = createSettingServiceWithMigrations().service;
      } finally {
        if (previous === undefined) delete process.env.ZCODE_DESKTOP_HOME_DIR;
        else process.env.ZCODE_DESKTOP_HOME_DIR = previous;
      }
      const choose = async (selection) => {
        const current = await settings.get();
        await settings.update(
          {
            providerFamilyDomain: "bigmodel",
            providerFamilyDomainUpdatedAt: Date.now(),
            providerFamilyDomainMigrated: true,
            providerFamilyConnectionSelections: {
              ...current.providerFamilyConnectionSelections,
              bigmodel: selection,
            },
          },
          {
            providerFamilyDomain: current.providerFamilyDomain,
            providerFamilyConnectionSelections: current.providerFamilyConnectionSelections,
            providerFamilyDomainUpdatedAt: current.providerFamilyDomainUpdatedAt ?? 0,
          },
        );
      };
      const print = async () => {
        const before = service.seen.length;
        const result = await run(zcode, ["-p", "hello"], env, join(home, "work"));
        return { result, requests: service.seen.slice(before) };
      };
      const personalIssue =
        "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens";
      const teamIssue =
        "/api/biz/v1/organization/org-t/projects/p-t/api_keys/key-team/access_tokens";
      const issued = (run) => run.requests.find((r) => r.path.endsWith("/access_tokens"))?.path;
      // App 选了团队：rust-cli 不读，仍用默认的个人套餐（F49）。
      await choose({
        kind: "team-coding-plan",
        productId: "prod-1",
        organizationId: "org-t",
        projectId: "p-t",
      });
      const afterTeam = await print();
      expectEqual(
        "team C: rust-cli answers after the App chose a team",
        "done",
        afterTeam.result.stdout.trim() || afterTeam.result.stderr,
      );
      expectEqual(
        "team C: the App's team choice does not switch rust-cli",
        personalIssue,
        issued(afterTeam),
      );
      await choose({ kind: "individual-coding-plan" });
      const afterPersonal = await print();
      expectEqual(
        "team C: the App's individual choice does not matter either",
        personalIssue,
        issued(afterPersonal),
      );
      // 团队模型由 rust-cli 自己的选择决定：团队项目令牌（团队权益的头照 App）。
      const before = service.seen.length;
      const teamRun = await run(
        zcode,
        ["-p", "hello", "--model", `account:bigmodel-team-coding-plan/${GLM_5_3_ACCOUNT}[p-t]`],
        env,
        join(home, "work"),
      );
      const teamRequests = service.seen.slice(before);
      expectEqual(
        "team C: rust-cli answers with the team model",
        "done",
        teamRun.stdout.trim() || teamRun.stderr,
      );
      const detail = teamRequests.find(
        (r) => r.path === "/api/biz/team/subscribe/product/querySubscribeDetail",
      );
      expectEqual(
        "team C: entitlement headers",
        ["header.bigmodel-access.signature", "org-t"],
        [detail?.headers.authorization, detail?.headers["bigmodel-organization"]],
      );
      // 个人套餐的令牌在取目录时为订阅查询换好（F50），所以这一轮先有个人的签发；团队模型用团队项目的签发。
      expectEqual(
        "team C: the team project token",
        true,
        teamRequests.some((r) => r.path === teamIssue),
      );
      const model = teamRequests.find((r) => r.path.endsWith("/v1/messages"));
      expectEqual(
        "team C: the model request carries the team token",
        true,
        /^Bearer pat-\d+$/.test(model?.headers.authorization ?? ""),
      );
      const raw = JSON.parse(await readFile(join(home, ".zcode/v2/setting.json"), "utf8"));
      expectEqual(
        "team C: rust-cli never writes the App connection",
        { kind: "individual-coding-plan" },
        raw.providerFamilyConnectionSelections?.bigmodel,
      );
    }

    // D. 团队模型的会话：rust-cli 写，Node / App 读与续写，rust-cli 读回团队项目。
    {
      const { home, env } = await homeEnv("team-d");
      const store = createSharedZCodeCredentialStore({ env });
      await store.saveMany({
        "oauth:active_provider": "bigmodel",
        "oauth:bigmodel:access_token": "header.bigmodel-access.signature",
        "oauth:bigmodel:user_info": JSON.stringify({
          id: "key-1",
          username: "dev",
          displayName: "Dev",
        }),
        zcodejwttoken: "zcode-jwt",
      });
      const team = "account:bigmodel-team-coding-plan";
      const before = service.seen.length;
      const written = await run(
        zcode,
        ["-p", "hello", "--model", `${team}/${GLM_5_3_ACCOUNT}[p-u]`],
        env,
        join(home, "work"),
      );
      expectEqual(
        "team D: rust-cli answers with the named team project",
        "done",
        written.stdout.trim() || written.stderr,
      );
      expectEqual(
        "team D: the p-u project token",
        true,
        service.seen
          .slice(before)
          .some(
            (r) =>
              r.path ===
              "/api/biz/v1/organization/org-t/projects/p-u/api_keys/key-team-u/access_tokens",
          ),
      );
      const { SqliteSessionStore } = await importNode(
        nodeRoot,
        "adapters/src/storage/session-store/sqlite-session-store.ts",
      );
      const { readSessionModelSelection } = await importNode(
        nodeRoot,
        "bootstrap/src/app/session-store.ts",
      );
      const persistence = await importNode(
        nodeRoot,
        "core/src/runtime/methods/message-persistence.ts",
      );
      const { createMessageId, createPartId, createTurnId } = await importNode(
        nodeRoot,
        "contracts/src/interfaces/shared.ts",
      );
      const { toTokenUsageInfo } = await importNode(
        nodeRoot,
        "core/src/runtime/helpers/tool-result.ts",
      );
      const nodeStore = await SqliteSessionStore.openStartup({
        dbPath: join(home, ".zcode/cli/db/db.sqlite"),
      });
      let sessionID;
      try {
        // rust-cli 记的是规范路径（macOS 的临时目录在 /private 下）。
        const listed = await nodeStore.listSessions({
          directory: await realpath(join(home, "work")),
          roots: true,
        });
        expectEqual("team D: Node lists the session", 1, listed.length);
        sessionID = listed[0]?.id;
        if (!sessionID)
          throw new Error(`team D: no session written by rust-cli: ${written.stderr}`);
        const selection = await readSessionModelSelection(nodeStore, sessionID);
        // Node 的 strict schema 能解析（团队项目不在选择里，另存在 rust-cli/model_connection）。
        expectEqual(
          "team D: Node reads the team model",
          { providerId: team, modelId: GLM_5_3_ACCOUNT },
          {
            providerId: selection?.providerId,
            modelId: selection?.modelId,
          },
        );
        expectEqual(
          "team D: every message passes the App protocol schema",
          [],
          await appSchemaRejections(nodeRoot, nodeStore, [sessionID]),
        );
        // Node 用自己的写入代码续写一轮（App 托管时也是这一套写入）。
        const runtime = {
          sessionId: sessionID,
          sessionStore: nodeStore,
          config: { mode: "build" },
          getTools: () => [],
          getSessionModelSelection: () => selection,
          getPlanEnabled: () => false,
          workingDirectory: join(home, "work"),
          workspaceRoot: join(home, "work"),
        };
        runtime.persistMessage = persistence.persistMessage.bind(runtime);
        runtime.persistPart = persistence.persistPart.bind(runtime);
        const trace = { traceId: sessionID, turnId: createTurnId() };
        const userID = createMessageId();
        await persistence.persistUserPrompt.call(
          runtime,
          userID,
          "Node continues",
          undefined,
          trace,
          { sourceCommandId: "node_cmd" },
        );
        const assistantID = createMessageId();
        const created = Date.now();
        const tokens = toTokenUsageInfo({ inputTokens: 3, outputTokens: 1, totalTokens: 4 });
        await persistence.persistAssistantMessage.call(
          runtime,
          assistantID,
          userID,
          created,
          { completed: Date.now(), finish: "stop", tokens },
          trace,
          { providerId: selection.providerId, modelId: selection.modelId },
        );
        await runtime.persistPart({
          id: createPartId(),
          sessionID,
          messageID: assistantID,
          type: "text",
          text: "Node answer",
          time: { start: created, end: Date.now() },
        });
        expectEqual(
          "team D: the continued session still passes the App schema",
          [],
          await appSchemaRejections(nodeRoot, nodeStore, [sessionID]),
        );
      } finally {
        nodeStore.close();
      }
      const loaded = rust("read", join(home, ".zcode"), sessionID);
      expectEqual(
        "team D: rust-cli reads the Node turn",
        true,
        JSON.stringify(loaded.messages).includes("Node answer"),
      );
      expectEqual(
        "team D: rust-cli keeps the team project",
        { productId: "prod-1", organizationId: "org-t", projectId: "p-u" },
        loaded.model?.connection,
      );
      // Node 换了模型（Node `persistRuntimeModelSelection` 的写法）：rust-cli 不再套用记下的团队项目。
      const switched = await SqliteSessionStore.openStartup({
        dbPath: join(home, ".zcode/cli/db/db.sqlite"),
      });
      try {
        const now = Date.now();
        await switched.saveSessionEntry({
          id: `${sessionID}:runtime-model-selection`,
          sessionID,
          type: "runtime/model_selection",
          touchSession: false,
          time: { created: now, updated: now },
          data: { providerId: team, modelId: GLM_5_3_FLASH_ACCOUNT },
        });
      } finally {
        switched.close();
      }
      const after = rust("read", join(home, ".zcode"), sessionID);
      expectEqual(
        "team D: a model chosen by Node drops the team project",
        { model: GLM_5_3_FLASH_ACCOUNT, connection: null },
        {
          model: after.model?.model,
          connection: after.model?.connection ?? null,
        },
      );
    }
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
  return findings;
}

/** 单独运行（调试用）：`node scripts/rust-cli/node-interop-team.mjs --node-root <检出>`。 */
export function buildZcode(repoRoot) {
  const manifest = join(repoRoot, "packages/rust-cli/Cargo.toml");
  execFileSync(
    "cargo",
    ["build", "-q", "--manifest-path", manifest, "-p", "rust-cli", "--bin", BINARY_NAME],
    {
      cwd: join(repoRoot, "packages/rust-cli"),
      stdio: "inherit",
    },
  );
  return binaryPath(repoRoot, "debug");
}
