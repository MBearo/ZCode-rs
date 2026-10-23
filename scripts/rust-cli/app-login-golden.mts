#!/usr/bin/env node
// rust-cli 个人套餐照 App 的对照夹具（docs/specs/rust-cli/platform.md §5.2–§5.7、§7，工作线 F50）：用 App 的 TS 账号栈
// 生成 golden，本工程的测试逐项比较。不连任何服务：网络全是脚本里的假回复；HOME 与数据目录指向临时目录，凭据密钥是
// 测试值。
//
//   node_modules/.bin/tsx scripts/rust-cli/app-login-golden.mts \
//     packages/rust-cli/crates/platform/tests/fixtures/app-login-golden.json [--node-root <装好依赖的检出>]
//
// 五部分：
// - logins：App 的轮询登录（`OAuthService.startOAuthWithPolling` + `pollPendingOAuth`，真实的 `createCredentialService`
//   加密写盘）写出的 `credentials.json` 原文、解密后的键值、`loadUserProfile` 读出的 profile id；再按 App 请求期的
//   个人换取（`createAccountProjectTokenClient` + `AccountProviderApiKeyResolver` + `createAccountProviderCredentialService`）
//   建项目位置：请求序列、位置键与值、之后的文件原文；同一份文件上的新客户端只签发（直接复用位置）。
// - subscriptions：`validateCodingPlanProviderAvailability`（个人）对一组订阅列表回复的结论与请求；
// - profiles：`OAuthCredentialRepo.loadUserProfile` 对各种 user_info 读出的 id；
// - logout：`OAuthService.logoutAll`（加 App 的派生 key 清理）之后剩下的键。
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const rootFlag = args.indexOf("--node-root");
const nodeRoot =
  rootFlag >= 0 ? resolve(args.splice(rootFlag, 2)[1]!) : resolve(import.meta.dirname, "../..");
const [outFile] = args;
if (!outFile) throw new Error("usage: app-login-golden.mts <out.json> [--node-root <dir>]");

// 不碰真实的 ~/.zcode：App 的模块在加载与调用时读这些变量。
const home = mkdtempSync(join(tmpdir(), "rust-cli-app-login-golden-"));
const SECRET = "golden-secret";
const ZAI_ORIGIN = "https://api.example.com";
const BIGMODEL_ORIGIN = "https://big.example.com";
const ZCODE_ORIGIN = "https://zcode.example.com";
process.env.HOME = home;
process.env.USERPROFILE = home;
process.env.ZCODE_DATA_BASE_DIR = home;
process.env.ZCODE_CREDENTIAL_SECRET = SECRET;
process.env.ZAI_BUSINESS_BASE_URL = ZAI_ORIGIN;
process.env.BIGMODEL_API_BASE_URL = BIGMODEL_ORIGIN;
process.env.ZCODE_BASE_URL = ZCODE_ORIGIN;

const load = (path: string) => import(pathToFileURL(join(nodeRoot, path)).href);
const { setDataBaseDir } = await load("packages/services/src/paths.ts");
const { createCredentialService } = await load(
  "packages/services/src/credential/credentialService.ts",
);
const { OAuthService } = await load("packages/services/src/oauth/oauthService.ts");
const { OAuthCredentialRepo } = await load(
  "packages/services/src/oauth/repo/oauthCredentialRepo.ts",
);
const { createAccountProjectTokenClient } = await load(
  "packages/services/src/model-provider/accountProjectTokenClient.ts",
);
const { AccountProviderApiKeyResolver } = await load(
  "packages/services/src/model-provider/accountProviderApiKeyResolver.ts",
);
const { createAccountProviderCredentialService } = await load(
  "packages/services/src/model-provider/accountProviderCredentialService.ts",
);
const { validateCodingPlanProviderAvailability } = await load(
  "packages/services/src/model-provider/codingPlanProviderAvailability.ts",
);
const { createOAuthProviderLogoutHandler } = await load(
  "packages/services/src/oauth/oauthProviderLogout.ts",
);
const { createAccountProviderCredentialStore } = await load(
  "packages/services/src/model-provider/accountProviderCredentialStore.ts",
);

type Json = unknown;
type Family = "zai" | "bigmodel";

interface Recorded {
  method: string;
  path: string;
  authorization: string | null;
  body: Json;
}

function record(url: string, init: RequestInit): Recorded {
  const headers = Object.fromEntries(
    Object.entries((init.headers ?? {}) as Record<string, string>).map(
      ([name, value]) => [name.toLowerCase(), value] as const,
    ),
  );
  return {
    method: (init.method ?? "GET").toUpperCase(),
    path: new URL(url).pathname,
    authorization: headers.authorization ?? null,
    body: typeof init.body === "string" ? JSON.parse(init.body) : null,
  };
}

const json = (status: number, body: Json) => new Response(JSON.stringify(body), { status });
const ok = (data: Json) => json(200, { code: 200, data });

/** 轮询登录的 ready 回复里的用户（原样：App 存 zai 的 rawProfile 与 bigmodel 的 rawProfile）。 */
const POLL_USER = {
  user_id: "u-1",
  name: " Dev ",
  email: "dev@example.com",
  avatar: " https://avatar.example.com/a.png ",
  locale: "zh-CN",
};
const CUSTOMER = {
  organizations: [
    {
      organizationId: "org-t",
      organizationName: "团队",
      projects: [{ projectId: "p-t", projectName: "默认项目", projectType: 2 }],
    },
    {
      organizationId: "org-1",
      organizationName: "默认机构",
      projects: [{ projectId: "p-1", projectName: "默认项目", projectType: "1" }],
    },
  ],
};

/** 账号服务与 ZCode 服务的假回复（一个 family 一份，记录请求）。 */
function fakeApi(family: Family) {
  const requests: Recorded[] = [];
  let issued = 0;
  const apiClient = {
    request: async (input: string | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      const entry = record(url, init);
      requests.push(entry);
      const now = Math.floor(Date.now() / 1000);
      switch (entry.path) {
        case "/api/v1/oauth/cli/init":
          return json(200, {
            code: 0,
            data: {
              flow_id: "f1",
              authorize_url: "https://open.example.com/authorize?state=s1",
              expires_at: now + 600,
              poll_interval_sec: 1,
            },
          });
        case "/api/v1/oauth/cli/poll/f1":
          return json(200, {
            code: 0,
            data: {
              status: "ready",
              token: " zcode-jwt ",
              user: POLL_USER,
              ...(family === "zai"
                ? { zai: { access_token: "zai-oauth" } }
                : { bigmodel: { access_token: "at-big", refresh_token: "rt-big" } }),
            },
          });
        case "/api/auth/z/login":
          return json(200, { code: 0, data: { access_token: "jwt-zai" } });
        case "/api/biz/customer/getCustomerInfo":
          return ok(CUSTOMER);
        case "/api/biz/v1/organization/org-1/projects/p-1/api_keys":
          return entry.method === "GET"
            ? ok([{ name: "other", apiKey: "key-0" }])
            : ok({ name: "zcode-api-key", apiKey: "key-1" });
        case "/api/biz/v1/organization/org-1/projects/p-1/api_keys/key-1/access_tokens":
          issued += 1;
          return ok({
            accessToken: `pat-${issued}`,
            tokenType: "Bearer",
            expiresIn: 3600,
            expiresAt: now + 3600,
          });
        default:
          return json(404, { code: 404 });
      }
    },
  };
  return { apiClient, requests };
}

/** App 的个人换取（请求期的装配，`packages/services/src/node.ts` 的 `loadIndividualPlanMaterial`）。 */
function appIndividual(apiClient: Json, credentialService: Json, repo: typeof OAuthCredentialRepo) {
  const tokenClient = createAccountProjectTokenClient(apiClient, credentialService);
  const resolver = new AccountProviderApiKeyResolver(async () => null, tokenClient);
  const service = createAccountProviderCredentialService({
    loadOAuthAccessToken: async (family: Family) =>
      (await repo.loadTokenSet(family))?.accessToken ?? null,
    resolveProviderMaterial: (
      family: Family,
      accessToken: string,
      accountIdentity: string,
      rejected?: string,
    ) => resolver.resolveProviderMaterial(family, accessToken, accountIdentity, rejected),
  });
  return async (family: Family) => {
    const accountIdentity = (await repo.loadUserProfile(family))?.id;
    return service.loadCodingPlanMaterial({
      providerId: `account:${family}-individual-coding-plan`,
      family,
      accountIdentity,
    });
  };
}

async function appLogin(family: Family) {
  // 每个 family 一个新的数据目录（App 登录一个 family 时会清掉另一个）。
  const dir = mkdtempSync(join(home, `${family}-`));
  setDataBaseDir(dir);
  const credentialService = createCredentialService();
  const { apiClient, requests } = fakeApi(family);
  const service = new OAuthService(credentialService, {
    apiClient,
    env: { ...process.env },
  });
  await service.startOAuthWithPolling(family);
  const result = await service.pollPendingOAuth();
  if (result?.kind !== "session") throw new Error(`${family}: App login did not finish`);
  const read = () => {
    const file = readFileSync(join(dir, ".zcode/v2/credentials.json"), "utf8");
    return file;
  };
  const loginFile = read();
  const loginRecord: Record<string, string | null> = {};
  for (const key of Object.keys(JSON.parse(loginFile)))
    loginRecord[key] = await credentialService.load(key);
  const repo = new OAuthCredentialRepo(credentialService);
  const profileId = (await repo.loadUserProfile(family))?.id ?? null;
  const loginRequests = requests.splice(0);

  // App 请求期的个人换取：建项目位置。
  const material = await appIndividual(apiClient, credentialService, repo)(family);
  const locationRequests = requests.splice(0);
  const locationFile = read();
  const keys = Object.keys(JSON.parse(locationFile)).filter((key) =>
    key.startsWith("project-key-location:v1:"),
  );
  if (keys.length !== 1) throw new Error(`${family}: expected one location, got ${keys.length}`);
  const location = { key: keys[0], value: await credentialService.load(keys[0]!) };

  // 新进程（新客户端）读到位置：只签发，不查 customerInfo。
  const again = await appIndividual(apiClient, credentialService, repo)(family);
  const reuseRequests = requests.splice(0);
  return {
    family,
    origin: family === "zai" ? ZAI_ORIGIN : BIGMODEL_ORIGIN,
    pollUser: POLL_USER,
    loginRequests,
    loginFile,
    loginRecord,
    profileId,
    material: { token: material?.token ?? null, apiKeyId: material?.apiKeyId ?? null },
    locationRequests,
    location,
    locationFile,
    reuse: { token: again?.token ?? null, requests: reuseRequests },
  };
}

const logins = { zai: await appLogin("zai"), bigmodel: await appLogin("bigmodel") };

// ---- subscriptions ----

interface Scripted {
  status: number;
  body: string;
}
const listOf = (data: Json, envelope: Record<string, Json> = { code: 200 }): Scripted => ({
  status: 200,
  body: JSON.stringify({ ...envelope, data }),
});
const coding = (fields: Record<string, Json> = {}) => ({
  productId: "prod-c",
  productName: "GLM Coding Plan Pro",
  status: "VALID",
  inCurrentPeriod: true,
  ...fields,
});
const subscriptionCases: Record<string, Scripted | null> = {
  available: listOf([coding()]),
  availableByProductId: listOf([
    { productId: "Coding-Lite", status: "VALID", inCurrentPeriod: true },
  ]),
  availableUpperCase: listOf([coding({ productId: "x", productName: "GLM CODING PLAN" })]),
  availableWithExtras: listOf([
    coding({ autoRenew: 1, nextRenewTime: "2026-12-01", valid: "Y", billingCycle: "MONTH", x: [] }),
  ]),
  availableAfterOthers: listOf([
    { productName: "Token Pack", status: "VALID", inCurrentPeriod: true },
    coding({ status: "EXPIRED", inCurrentPeriod: false }),
    coding(),
  ]),
  availableAfterMalformed: listOf([coding({ inCurrentPeriod: "yes" }), coding()]),
  expired: listOf([coding({ status: "EXPIRED", inCurrentPeriod: false })]),
  notInPeriod: listOf([coding({ inCurrentPeriod: false })]),
  otherProductOnly: listOf([{ productName: "Token Pack", status: "VALID", inCurrentPeriod: true }]),
  emptyList: listOf([]),
  malformedCoding: listOf([coding({ inCurrentPeriod: undefined })]),
  malformedCodingNullName: listOf([coding({ productId: "coding-1", productName: null })]),
  malformedCodingStatus: listOf([coding({ status: 1 })]),
  malformedAutoRenew: listOf([coding({ autoRenew: "yes" })]),
  malformedOther: listOf([{ productName: "Token Pack", status: 1 }]),
  notObjectItems: listOf(["coding", 1, null, ["coding"]]),
  codingIdNotString: listOf([{ productId: 7, productName: "coding", status: "VALID" }]),
  dataObject: listOf({ list: [coding()] }),
  dataMissing: { status: 200, body: JSON.stringify({ code: 200 }) },
  code0: listOf([coding()], { code: 0 }),
  noCode: listOf([coding()], {}),
  code500: listOf([coding()], { code: 500 }),
  codeString: listOf([coding()], { code: "200" }),
  codeNull: listOf([coding()], { code: null }),
  successFalse: listOf([coding()], { code: 200, success: false }),
  successString: listOf([coding()], { code: 200, success: "true" }),
  envelopeArray: { status: 200, body: JSON.stringify([coding()]) },
  notJson: { status: 200, body: "not json" },
  http401: { status: 401, body: JSON.stringify({ code: 401 }) },
  http403: { status: 403, body: "{}" },
  http404: { status: 404, body: "{}" },
  http500: { status: 500, body: "{}" },
  http429: { status: 429, body: "{}" },
  // 没有项目令牌（换不到）：不发请求。
  noProjectToken: null,
};

async function subscriptionCase(family: Family, script: Scripted | null) {
  const requests: Recorded[] = [];
  const apiClient = {
    request: async (input: string | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      requests.push(record(url, init));
      return new Response(script?.body ?? "{}", { status: script?.status ?? 404 });
    },
  };
  const result = await validateCodingPlanProviderAvailability(
    {
      providerId: `account:${family}-individual-coding-plan`,
      family,
      planKind: "individual-coding-plan",
      apiKey: script ? "pat-1" : null,
    },
    { apiClient },
  );
  return { result, requests };
}

const subscriptions: Record<string, Json> = {};
for (const [name, script] of Object.entries(subscriptionCases)) {
  subscriptions[name] = {
    script,
    zai: await subscriptionCase("zai", script),
    bigmodel: await subscriptionCase("bigmodel", script),
  };
}

// ---- profiles ----

const profileCases: Record<string, string> = {
  zaiRaw: JSON.stringify({ user_id: "u-1", name: "Dev" }),
  zaiRawUntrimmed: JSON.stringify({ user_id: " u-1 ", email: "dev@example.com" }),
  appShape: JSON.stringify({ id: "u-2", username: "dev", displayName: "Dev" }),
  appShapeWithRaw: JSON.stringify({
    id: "u-3",
    username: "dev",
    displayName: "Dev",
    rawProfile: { user_id: "u-3", zcodeProfileSchemaVersion: 2 },
  }),
  nodeBigmodel: JSON.stringify({
    id: "key-0123456789abcdef01234567",
    username: "dev",
    displayName: "dev",
    rawProfile: { user_id: "u-4" },
  }),
  idNotString: JSON.stringify({ id: 1, username: "dev", displayName: "Dev" }),
  missingDisplayName: JSON.stringify({ id: "u-5", username: "dev" }),
  rawNoUserId: JSON.stringify({ name: "Dev" }),
  rawUserIdNumber: JSON.stringify({ user_id: 7, email: "dev@example.com" }),
  rawEmpty: JSON.stringify({}),
  rawNameBlank: JSON.stringify({ name: "  " }),
  blankId: JSON.stringify({ id: "  ", username: "dev", displayName: "Dev" }),
  array: JSON.stringify([]),
  string: JSON.stringify("u-1"),
  notJson: "u-1",
};

const profiles: Record<string, Json> = {};
{
  const dir = mkdtempSync(join(home, "profiles-"));
  setDataBaseDir(dir);
  const credentialService = createCredentialService();
  const repo = new OAuthCredentialRepo(credentialService);
  for (const [name, value] of Object.entries(profileCases)) {
    const row: Record<string, Json> = { value };
    for (const family of ["zai", "bigmodel"] as const) {
      await credentialService.save(`oauth:${family}:user_info`, value);
      row[family] = (await repo.loadUserProfile(family))?.id ?? null;
    }
    profiles[name] = row;
  }
}

// ---- logout ----

const logoutRecord: Record<string, string> = {
  "oauth:active_provider": "zai",
  "oauth:zai:access_token": "jwt-zai",
  "oauth:zai:refresh_token": "rt-zai",
  "oauth:zai:user_info": JSON.stringify({ user_id: "u-zai" }),
  "oauth:bigmodel:access_token": "at-big",
  "oauth:bigmodel:refresh_token": "rt-big",
  "oauth:bigmodel:user_info": JSON.stringify({
    id: "u-big",
    username: "dev",
    displayName: "Dev",
  }),
  zcodejwttoken: "zcode-jwt",
  "oauth:login_attribution": JSON.stringify({ source: "x" }),
  "account-provider:coding-plan:account:zai-individual-coding-plan:account:u-zai:api-key":
    "legacy-zai",
  "account-provider:coding-plan:account:bigmodel-individual-coding-plan:account:u-big:api-key":
    "legacy-big",
  "account-provider:coding-plan:account:zai-individual-coding-plan:account:other:api-key": "other",
  [logins.zai.location.key!]: logins.zai.location.value!,
  "ssh:host": "kept",
};
let logout: Json;
{
  const dir = mkdtempSync(join(home, "logout-"));
  setDataBaseDir(dir);
  const credentialService = createCredentialService();
  for (const [key, value] of Object.entries(logoutRecord)) await credentialService.save(key, value);
  const store = createAccountProviderCredentialStore({ credentialService });
  const service = new OAuthService(credentialService, {
    // 退出不联网；App 的 adapter 要求注入 ApiClient。
    apiClient: { request: async () => json(404, {}) },
    env: { ...process.env },
    onProviderLogout: createOAuthProviderLogoutHandler({ accountProviderCredentialStore: store }),
  });
  await service.logoutAll();
  const file = readFileSync(join(dir, ".zcode/v2/credentials.json"), "utf8");
  logout = { record: logoutRecord, remaining: Object.keys(JSON.parse(file)).sort() };
}

writeFileSync(
  outFile,
  `${JSON.stringify({ secret: SECRET, customer: CUSTOMER, logins, subscriptions, profiles, logout }, null, 2)}\n`,
);
process.exit(0);
