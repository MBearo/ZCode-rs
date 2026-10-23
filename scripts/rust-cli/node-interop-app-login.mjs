// rust-cli 与 App 的个人套餐登录互操作（docs/specs/rust-cli/platform.md §5.2–§5.7，工作线 F50）。
// node-interop-check.mjs 的第十五段。全部是本地假服务与临时 HOME，凭据密钥是测试值，不连任何真实服务。App 一侧是 App
// 的真实代码（`OAuthService` 的轮询登录、`createCredentialService` 的加密读写、请求期的个人换取
// `createAccountProjectTokenClient` + `AccountProviderApiKeyResolver` + `createAccountProviderCredentialService`），网络经
// 注入的 ApiClient 走同一份假回复；rust-cli 一侧是真实的 `zcode` 进程。只用 bigmodel：App 的 bigmodel 地址在调用时
// 读环境变量（zai 的地址在模块加载时就定了，同一进程里前面的段可能已经加载过）。
//
// A. 只有 App 的登录：rust-cli（`zcode -p`）直接能用个人套餐（App 的 profile id、订阅查询），项目位置由 rust-cli 先建；
//    App 的请求期换取复用它（不查 customerInfo）。
// B. App 先建项目位置：rust-cli 复用（不查 customerInfo）。
// C. rust-cli 登录（`zcode login bigmodel --no-browser --json`）：App 的 `loadUserProfile` 读出 `user_id`，App 的换取能用，
//    与 rust-cli 共用同一份位置；Node 独立 CLI 读 rust-cli 登录的凭据不再算已登录（以 App 为准的已知差异）。
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { importNode } from "./node-root.mjs";

const SECRET = "interop-secret";
const SSE = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}\n\n',
  'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
].join("");

/** 假服务：ZCode 的 CLI 轮询登录、bigmodel 的个人项目与订阅、模型。记下每个请求是谁发的（app / rust）。 */
function createService() {
  const seen = [];
  let issued = 0;
  const now = () => Math.floor(Date.now() / 1000);
  const ok = (data) => ({ status: 200, body: JSON.stringify({ code: 200, data }) });
  function reply({ who, method, url, headers, body }) {
    const path = new URL(url).pathname.replace(/^\/+/, "/");
    seen.push({ who, method, path, authorization: headers.authorization ?? null, body });
    switch (path) {
      case "/api/v1/oauth/cli/init":
        return {
          status: 200,
          body: JSON.stringify({
            code: 0,
            data: {
              flow_id: "f1",
              authorize_url: "https://open.example.com/authorize?state=s1",
              expires_at: now() + 600,
              poll_interval_sec: 1,
            },
          }),
        };
      case "/api/v1/oauth/cli/poll/f1":
        return {
          status: 200,
          body: JSON.stringify({
            code: 0,
            data: {
              status: "ready",
              token: "zcode-jwt",
              user: { user_id: "u-1", name: "Dev", email: "dev@example.com" },
              bigmodel: { access_token: "at-big", refresh_token: "rt-big" },
            },
          }),
        };
      case "/api/biz/customer/getCustomerInfo":
        return ok({
          organizations: [
            {
              organizationId: "org-1",
              organizationName: "默认机构",
              projects: [{ projectId: "p-1", projectName: "默认项目", projectType: "1" }],
            },
          ],
        });
      case "/api/biz/v1/organization/org-1/projects/p-1/api_keys":
        return method === "GET" ? ok([]) : ok({ name: "zcode-api-key", apiKey: "key-1" });
      case "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens":
        issued += 1;
        return ok({
          accessToken: `pat-${issued}`,
          tokenType: "Bearer",
          expiresIn: 3600,
          expiresAt: now() + 3600,
        });
      case "/api/biz/subscription/list":
        return ok([{ productName: "GLM Coding Plan", status: "VALID", inCurrentPeriod: true }]);
      case "/api/biz/subscription/enterprise/v2/pricing":
        return ok({ productList: [] });
      default:
        if (path.endsWith("/v1/messages"))
          return { status: 200, body: SSE, type: "text/event-stream" };
        return { status: 404, body: JSON.stringify({ code: 404 }) };
    }
  }
  return { seen, reply };
}

async function listen(service) {
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => (body += chunk));
    request.on("end", () => {
      const headers = Object.fromEntries(
        Object.entries(request.headers).map(([name, value]) => [name.toLowerCase(), String(value)]),
      );
      const reply = service.reply({
        who: "rust",
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

/** App 的 ApiClient 替身：同一份回复，不出网络。 */
function appClient(service) {
  return {
    request: async (input, init = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      const headers = Object.fromEntries(
        Object.entries(init.headers ?? {}).map(([name, value]) => [name.toLowerCase(), value]),
      );
      const reply = service.reply({
        who: "app",
        method: (init.method ?? "GET").toUpperCase(),
        url,
        headers,
        body: typeof init.body === "string" ? init.body : "",
      });
      return new Response(reply.body, { status: reply.status });
    },
  };
}

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

const LOCATION = "project-key-location:v1:";
const KEY_LIST = "/api/biz/v1/organization/org-1/projects/p-1/api_keys";

export async function checkAppLogin({ nodeRoot, repoRoot, base, expectEqual, zcode }) {
  const service = createService();
  const { server, url } = await listen(service);
  const findings = {};
  // App 的 bigmodel 地址在调用时读 process.env（`resolveBigModelApiOrigin`）：只在这一段里指向假服务。
  const saved = process.env.BIGMODEL_API_BASE_URL;
  process.env.BIGMODEL_API_BASE_URL = url;
  try {
    const services = (path) =>
      import(pathToFileURL(join(nodeRoot, "packages/services/src", path)).href);
    const { setDataBaseDir } = await services("paths.ts");
    const { createCredentialService } = await services("credential/credentialService.ts");
    const { createCredentialCipherProvider } = await services(
      "credential/providers/credentialCipherProvider.ts",
    );
    const { OAuthService } = await services("oauth/oauthService.ts");
    const { OAuthCredentialRepo } = await services("oauth/repo/oauthCredentialRepo.ts");
    const { createAccountProjectTokenClient } = await services(
      "model-provider/accountProjectTokenClient.ts",
    );
    const { AccountProviderApiKeyResolver } = await services(
      "model-provider/accountProviderApiKeyResolver.ts",
    );
    const { createAccountProviderCredentialService } = await services(
      "model-provider/accountProviderCredentialService.ts",
    );
    const { hasConfiguredStandaloneCodingPlan } = await importNode(
      nodeRoot,
      "bootstrap/src/auth-login.ts",
    );
    const apiClient = appClient(service);
    const cipherProvider = createCredentialCipherProvider({
      env: { ZCODE_CREDENTIAL_SECRET: SECRET },
    });
    // App 的凭据仓库指向这个 home（`setDataBaseDir` 改 App 的数据根，只在本进程里）。
    const appCredentials = (home) => {
      setDataBaseDir(home);
      return createCredentialService({ cipherProvider });
    };
    const appLogin = async (home) => {
      const credentialService = appCredentials(home);
      const oauth = new OAuthService(credentialService, {
        apiClient,
        env: { ...process.env, ZCODE_BASE_URL: url },
      });
      await oauth.startOAuthWithPolling("bigmodel");
      const result = await oauth.pollPendingOAuth();
      if (result?.kind !== "session") throw new Error("App login did not finish");
    };
    // App 请求期的个人换取（`packages/services/src/node.ts` 的 `loadIndividualPlanMaterial`），每次一个新客户端（新进程）。
    const appIndividual = async (home) => {
      const credentialService = appCredentials(home);
      const repo = new OAuthCredentialRepo(credentialService);
      const tokenClient = createAccountProjectTokenClient(apiClient, credentialService);
      const resolver = new AccountProviderApiKeyResolver(async () => null, tokenClient);
      const accounts = createAccountProviderCredentialService({
        loadOAuthAccessToken: async (family) =>
          (await repo.loadTokenSet(family))?.accessToken ?? null,
        resolveProviderMaterial: (family, token, identity, rejected) =>
          resolver.resolveProviderMaterial(family, token, identity, rejected),
      });
      const accountIdentity = (await repo.loadUserProfile("bigmodel"))?.id;
      const material = await accounts.loadCodingPlanMaterial({
        providerId: "account:bigmodel-individual-coding-plan",
        family: "bigmodel",
        accountIdentity,
      });
      return { material, profileId: accountIdentity };
    };
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
          ZCODE_CREDENTIAL_SECRET: SECRET,
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
    const locations = async (home) =>
      Object.keys(JSON.parse(await readFile(join(home, ".zcode/v2/credentials.json"), "utf8")))
        .filter((key) => key.startsWith(LOCATION))
        .sort();
    const since = (mark, who) => service.seen.slice(mark).filter((r) => r.who === who);
    const print = async (home, env) => {
      const mark = service.seen.length;
      const result = await run(zcode, ["-p", "hello"], env, join(home, "work"));
      return { result, requests: since(mark, "rust") };
    };

    // A. 只有 App 的登录：rust-cli 能用个人套餐，先建位置；App 复用。
    {
      const { home, env } = await homeEnv("app-a");
      await appLogin(home);
      const before = await locations(home);
      expectEqual("app A: the App login writes no project location", [], before);
      const rust = await print(home, env);
      expectEqual(
        "app A: rust-cli answers with the App login alone",
        "done",
        rust.result.stdout.trim() || rust.result.stderr,
      );
      const subscription = rust.requests.find((r) => r.path === "/api/biz/subscription/list");
      expectEqual(
        "app A: rust-cli checks the individual subscription with the project token",
        true,
        /^pat-\d+$/.test(subscription?.authorization ?? ""),
      );
      const model = rust.requests.find((r) => r.path.endsWith("/v1/messages"));
      expectEqual(
        "app A: the model request carries the individual project token",
        `Bearer ${subscription?.authorization}`,
        model?.authorization,
      );
      const written = await locations(home);
      expectEqual("app A: rust-cli wrote one project location", 1, written.length);
      const mark = service.seen.length;
      const app = await appIndividual(home);
      const appRequests = since(mark, "app");
      findings.appReusesRustLocation = !appRequests.some(
        (r) => r.path === "/api/biz/customer/getCustomerInfo" || r.path === KEY_LIST,
      );
      expectEqual(
        "app A: the App reuses the location rust-cli created (no customerInfo, no key list)",
        true,
        findings.appReusesRustLocation,
      );
      expectEqual("app A: the App gets a project token", true, Boolean(app.material?.token));
      expectEqual("app A: still one shared location", written, await locations(home));
      expectEqual(
        "app A: rust-cli wrote no account-provider keys",
        [],
        Object.keys(
          JSON.parse(await readFile(join(home, ".zcode/v2/credentials.json"), "utf8")),
        ).filter((key) => key.startsWith("account-provider:")),
      );
    }

    // B. App 先建位置：rust-cli 复用。
    {
      const { home, env } = await homeEnv("app-b");
      await appLogin(home);
      await appIndividual(home);
      const created = await locations(home);
      expectEqual("app B: the App created one location", 1, created.length);
      const rust = await print(home, env);
      expectEqual(
        "app B: rust-cli answers",
        "done",
        rust.result.stdout.trim() || rust.result.stderr,
      );
      // rust-cli 的团队列表另取 customerInfo（D-F49-1），所以看 key 列表：复用位置时不查。
      findings.rustReusesAppLocation = !rust.requests.some((r) => r.path === KEY_LIST);
      expectEqual(
        "app B: rust-cli reuses the App location (no key list)",
        true,
        findings.rustReusesAppLocation,
      );
      expectEqual("app B: still one shared location", created, await locations(home));
    }

    // C. rust-cli 登录，App 读；Node 独立 CLI 不再算已登录。
    {
      const { home, env } = await homeEnv("app-c");
      const login = await run(zcode, ["login", "bigmodel", "--no-browser", "--json"], env, home);
      expectEqual("app C: rust-cli login", 0, login.code === 0 ? 0 : login.stderr);
      const credentialService = appCredentials(home);
      const repo = new OAuthCredentialRepo(credentialService);
      const profile = await repo.loadUserProfile("bigmodel");
      expectEqual("app C: the App reads the profile id rust-cli wrote", "u-1", profile?.id);
      expectEqual(
        "app C: the bigmodel profile carries the App schema version (no userinfo migration)",
        2,
        profile?.rawProfile?.zcodeProfileSchemaVersion,
      );
      const app = await appIndividual(home);
      expectEqual("app C: the App gets a project token", true, Boolean(app.material?.token));
      const rust = await print(home, env);
      expectEqual(
        "app C: rust-cli answers",
        "done",
        rust.result.stdout.trim() || rust.result.stderr,
      );
      expectEqual(
        "app C: rust-cli reuses the location the App created (no key list)",
        false,
        rust.requests.some((r) => r.path === KEY_LIST),
      );
      expectEqual("app C: one shared location", 1, (await locations(home)).length);
      // 以 App 为准（用户决定 2026-10-21）：Node 独立 CLI 要 `account-provider:<id>:identity`，rust-cli 不再写。
      findings.nodeSeesRustLogin = await hasConfiguredStandaloneCodingPlan({ env });
      expectEqual(
        "app C: the standalone Node CLI does not see the rust-cli login (known difference)",
        false,
        findings.nodeSeesRustLogin,
      );
    }
  } finally {
    const { setDataBaseDir } = await import(
      pathToFileURL(join(nodeRoot, "packages/services/src/paths.ts")).href
    );
    setDataBaseDir(null);
    if (saved === undefined) delete process.env.BIGMODEL_API_BASE_URL;
    else process.env.BIGMODEL_API_BASE_URL = saved;
    await new Promise((resolve) => server.close(resolve));
  }
  return findings;
}
