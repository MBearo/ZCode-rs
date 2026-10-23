#!/usr/bin/env node
// rust-cli Team Plan 的 App 对照夹具（docs/specs/rust-cli/platform.md §7、§9，工作线 F47、F49）：用 App 的 TS 账号栈
// 生成 golden，本工程的测试逐项比较。不连任何服务：网络全是脚本里的假回复；HOME 指向临时目录。
//
//   node_modules/.bin/tsx scripts/rust-cli/team-plan-golden.mts \
//     packages/rust-cli/crates/platform/tests/fixtures/team-plan-golden.json [--node-root <装好依赖的检出>]
//
// 四部分：
// - tokens：`ProjectAccessTokenClient`（packages/shared）的团队与个人换取——请求序列（方法、路径、鉴权与团队头、
//   请求体）、项目位置键与值；
// - entitlements：`validateZaiAccountProviderAvailability` / `validateBigModelAccountProviderAvailability` 与
//   `fetchTeamCodingPlanEntitlement` 对一组回复的判定与请求；
// - teamLists（F49）：`BigModelCodingPlanSubscriptionProvider` / `ZaiCodingPlanSubscriptionProvider` 的
//   `getEnterprisePricing`（pricing + customerInfo 补全）再经输入框的 `buildComposerPlanGroups` 投影出的团队行。
// F47 的 selections（`setting.json` 的连接选择读法）随“当前连接”删掉（F49：CLI 不读 App 的当前连接）。
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const args = process.argv.slice(2);
const rootFlag = args.indexOf("--node-root");
const nodeRoot =
  rootFlag >= 0 ? resolve(args.splice(rootFlag, 2)[1]!) : resolve(import.meta.dirname, "../..");
const [outFile] = args;
if (!outFile) throw new Error("usage: team-plan-golden.mts <out.json> [--node-root <dir>]");

// 不碰真实的 ~/.zcode：App 的模块在调用时读这些变量。
const home = mkdtempSync(join(tmpdir(), "rust-cli-team-golden-"));
process.env.HOME = home;
process.env.USERPROFILE = home;
const ZAI_ORIGIN = "https://api.example.com";
const BIGMODEL_ORIGIN = "https://big.example.com";
process.env.ZAI_BUSINESS_BASE_URL = ZAI_ORIGIN;
process.env.BIGMODEL_API_BASE_URL = BIGMODEL_ORIGIN;

const load = (path: string) => import(pathToFileURL(join(nodeRoot, path)).href);
const { ProjectAccessTokenClient } = await load(
  "packages/shared/src/project-access-token-client.ts",
);
const { validateZaiAccountProviderAvailability, validateBigModelAccountProviderAvailability } =
  await load("packages/services/src/model-provider/codingPlanProviderAvailability.ts");
const { fetchTeamCodingPlanEntitlement } = await load(
  "packages/services/src/bigmodel/codingPlanEntitlement.ts",
);
const { BigModelCodingPlanSubscriptionProvider } = await load(
  "packages/services/src/coding-plan-subscription/bigmodelCodingPlanSubscriptionProvider.ts",
);
const { ZaiCodingPlanSubscriptionProvider } = await load(
  "packages/services/src/coding-plan-subscription/zaiCodingPlanSubscriptionProvider.ts",
);
// 输入框的团队行在 packages/ui（`@/` 是 ui 的 tsconfig 路径）。
const { tsImport } = await import("tsx/esm/api");
const { buildComposerPlanGroups } = await tsImport(
  pathToFileURL(join(nodeRoot, "packages/ui/src/lib/composerPlanGroups.ts")).href,
  { parentURL: import.meta.url, tsconfig: join(nodeRoot, "packages/ui/tsconfig.json") },
);

type Json = unknown;
const ok = (data: Json) => ({ code: 200, data });

// ---- tokens ----

interface Recorded {
  method: string;
  path: string;
  headers: Record<string, string>;
  body: Json;
}

const RECORDED_HEADERS = [
  "authorization",
  "bigmodel-organization",
  "bigmodel-project",
  "content-type",
];

function record(url: string, init: RequestInit): Recorded {
  const headers = Object.fromEntries(
    Object.entries((init.headers ?? {}) as Record<string, string>)
      .map(([name, value]) => [name.toLowerCase(), value] as const)
      .filter(([name]) => RECORDED_HEADERS.includes(name))
      .sort(([a], [b]) => a.localeCompare(b)),
  );
  return {
    method: init.method ?? "GET",
    path: new URL(url).pathname,
    headers,
    body: typeof init.body === "string" ? JSON.parse(init.body) : null,
  };
}

const CUSTOMER = {
  organizations: [
    {
      organizationId: "org-1",
      organizationName: "默认机构（个人）",
      projects: [
        { projectId: "p-team", projectName: "默认项目", projectType: 2 },
        { projectId: "p-own", projectName: "个人", projectType: "1" },
      ],
    },
    {
      organizationId: "org-t",
      organizationName: "团队",
      projects: [{ projectId: "p-t", projectName: "团队项目", projectType: "2" }],
    },
  ],
};

async function tokenCase(input: {
  family: "zai" | "bigmodel";
  team?: { organizationId: string; projectId: string };
  keys: Json[];
}) {
  const requests: Recorded[] = [];
  const saved: [string, string][] = [];
  const origin = input.family === "zai" ? ZAI_ORIGIN : BIGMODEL_ORIGIN;
  const client = new ProjectAccessTokenClient({
    request: async (url: string, init: RequestInit) => {
      const entry = record(url, init);
      requests.push(entry);
      if (entry.path === "/api/biz/customer/getCustomerInfo") return ok(CUSTOMER);
      if (entry.path.endsWith("/api_keys") && entry.method === "GET") return ok(input.keys);
      if (entry.path.endsWith("/api_keys") && entry.method === "POST") {
        const body = entry.body as { name: string; keyType?: number };
        return ok({
          name: body.name,
          apiKey: "key-new",
          ...(body.keyType ? { keyType: body.keyType } : {}),
        });
      }
      if (entry.path.endsWith("/access_tokens")) {
        const now = Math.floor(Date.now() / 1000);
        return ok({
          accessToken: "pat-1",
          tokenType: "Bearer",
          expiresIn: 3600,
          expiresAt: now + 3600,
        });
      }
      return { code: 404 };
    },
    locationStore: {
      load: async () => null,
      save: async (key: string, value: string) => {
        saved.push([key, value]);
      },
      delete: async () => {},
    },
  });
  const material = await client.resolve({
    origin,
    family: input.family,
    loginToken: input.family === "zai" ? "jwt-zai" : "at-big",
    accountId: "u-1",
    ...(input.team ? { team: input.team } : {}),
  });
  return { input, origin, requests, saved, material };
}

const tokens = {
  teamZaiCreated: await tokenCase({
    family: "zai",
    team: { organizationId: "org-t", projectId: "p-t" },
    // 同名但 keyType 不是 2、名字不同的条目都不复用。
    keys: [
      { name: "zcode-team-api-key", apiKey: "key-wrong-type", keyType: 1 },
      { name: "zcode-api-key", apiKey: "key-personal" },
    ],
  }),
  teamBigmodelReused: await tokenCase({
    family: "bigmodel",
    team: { organizationId: "org-t", projectId: "p-t" },
    keys: [{ name: "zcode-team-api-key", apiKey: " key-team ", keyType: 2 }],
  }),
  personalZaiNonTeam: await tokenCase({ family: "zai", keys: [] }),
  personalBigmodelNonTeam: await tokenCase({
    family: "bigmodel",
    keys: [{ name: "zcode-api-key", apiKey: "key-own" }],
  }),
};

// ---- entitlements ----

interface Scripted {
  status: number;
  body: Json;
}

const TEAM_CUSTOMER = {
  code: 200,
  data: {
    organizations: [
      { organizationId: " org-t ", projects: [{ projectId: "p-t", projectType: 2 }] },
    ],
  },
};
const detail = (data: Json, envelope: Record<string, Json> = { code: 200 }): Scripted => ({
  status: 200,
  body: { ...envelope, data },
});
const effective = (memberGrantStatus: string | null) => ({
  hasSubscription: true,
  status: "EFFECTIVE",
  memberGrantStatus,
  productId: "prod-1",
});
const entitlementCases: Record<string, { customer?: Scripted; detail?: Scripted }> = {
  available: { detail: detail(effective("VALID")) },
  unassigned: { detail: detail(effective("UNASSIGNED")) },
  expired: {
    detail: detail({ hasSubscription: true, status: "EXPIRED", memberGrantStatus: "VALID" }),
  },
  noSubscription: { detail: detail({ hasSubscription: false }) },
  newStatus: {
    detail: detail({ hasSubscription: true, status: "PAUSED", memberGrantStatus: "VALID" }),
  },
  effectiveNullGrant: { detail: detail(effective(null)) },
  badField: { detail: detail({ hasSubscription: true, status: 1 }) },
  missingHas: { detail: detail({ status: "EFFECTIVE", memberGrantStatus: "VALID" }) },
  envelopeFailed: { detail: detail(effective("VALID"), { code: 500 }) },
  envelopeSuccessFalse: { detail: detail(effective("VALID"), { success: false }) },
  envelopeStringCode: { detail: detail(effective("VALID"), { code: "200" }) },
  envelopeNoCode: { detail: detail(effective("VALID"), {}) },
  detail401: { detail: { status: 401, body: { code: 401 } } },
  detail403: { detail: { status: 403, body: {} } },
  detail500: { detail: { status: 500, body: {} } },
  detail404: { detail: { status: 404, body: {} } },
  customer401: { customer: { status: 401, body: {} } },
  customer403: { customer: { status: 403, body: {} } },
  customerFailedEnvelope: { customer: { status: 200, body: { code: 1001, data: null } } },
  customerStringCode: {
    customer: { status: 200, body: { code: "200", data: TEAM_CUSTOMER.data } },
  },
  customerNoOrganizations: { customer: { status: 200, body: { code: 0, data: {} } } },
  customerNotTeamProject: {
    customer: {
      status: 200,
      body: {
        code: 200,
        data: {
          organizations: [
            { organizationId: "org-t", projects: [{ projectId: "p-t", projectType: "1" }] },
          ],
        },
      },
    },
  },
  customerOtherOrganization: {
    customer: {
      status: 200,
      body: {
        code: 200,
        data: {
          organizations: [
            { organizationId: "org-x", projects: [{ projectId: "p-t", projectType: "2" }] },
          ],
        },
      },
    },
  },
  customerBadOrganizationId: {
    customer: {
      status: 200,
      body: { code: 200, data: { organizations: [{ organizationId: 7, projects: [] }] } },
    },
  },
};

async function entitlementCase(
  family: "zai" | "bigmodel",
  script: { customer?: Scripted; detail?: Scripted },
) {
  const requests: Recorded[] = [];
  const apiClient = {
    request: async (input: string | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      const entry = record(url, init);
      requests.push(entry);
      // zai 的 customerInfo 地址 App 写成 `${base}/` + `/api/biz/…`，路径以 `//` 开头（platform.md D-F47-5）。
      const reply = entry.path.endsWith("/api/biz/customer/getCustomerInfo")
        ? (script.customer ?? { status: 200, body: TEAM_CUSTOMER })
        : (script.detail ?? { status: 404, body: {} });
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    },
  };
  const credentials: Record<string, string> = {
    [`oauth:${family}:access_token`]: family === "zai" ? "jwt-zai" : "at-big",
    zcodejwttoken: "zcode-jwt",
  };
  const providerId = `account:${family}-team-coding-plan`;
  const validate =
    family === "zai"
      ? validateZaiAccountProviderAvailability
      : validateBigModelAccountProviderAvailability;
  const result = await validate([{ providerId, family, planKind: "team-coding-plan" }], {
    apiClient,
    credentialService: { load: async (key: string) => credentials[key] ?? null },
    providerFamilyConnectionSelections: {
      [family]: {
        kind: "team-coding-plan",
        productId: "prod-1",
        organizationId: "org-t",
        projectId: "p-t",
      },
    },
  });
  return { availability: result[providerId], requests };
}

const entitlements: Record<string, Json> = {};
for (const [name, script] of Object.entries(entitlementCases)) {
  entitlements[name] = {
    script,
    zai: await entitlementCase("zai", script),
    bigmodel: await entitlementCase("bigmodel", script),
  };
}
// fetchTeamCodingPlanEntitlement 直接给出过期 / 未分配的细分原因（availability 把它们并成 not_entitled）。
const detailReasons: Record<string, Json> = {};
for (const [name, script] of Object.entries(entitlementCases)) {
  if (!script.detail || script.detail.status !== 200) continue;
  const apiClient = {
    request: async () => new Response(JSON.stringify(script.detail!.body), { status: 200 }),
  };
  detailReasons[name] = await fetchTeamCodingPlanEntitlement({
    apiClient,
    authorization: "at-big",
    host: BIGMODEL_ORIGIN,
    teamContext: { organizationId: "org-t", projectId: "p-t" },
    timeoutMs: 1000,
  }).then((result: { kind: string; reason?: string }) => ({
    kind: result.kind,
    reason: result.reason ?? null,
  }));
}

// ---- teamLists（F49）----

const product = (fields: Record<string, Json>) => ({
  tier: "PRO",
  subscribeMode: "CONTINUOUS",
  subscribePeriod: "MONTHLY",
  ...fields,
});
const pricing = (productList: Json, envelope: Record<string, Json> = { code: 200 }): Scripted => ({
  status: 200,
  body: { ...envelope, data: { productList } },
});
const customerOf = (organizations: Json): Scripted => ({
  status: 200,
  body: { code: 200, data: { organizations } },
});
const TEAM_ORGS = [
  {
    organizationId: " org-a ",
    organizationName: " 团队 A ",
    projects: [
      { projectId: "p-a1", projectName: "一", projectType: 2 },
      { projectId: " p-a2 ", projectName: "二", projectType: " 2 " },
      { projectId: "p-own", projectName: "个人", projectType: "1" },
    ],
  },
  {
    organizationId: "org-b",
    organizationName: "团队 B",
    projects: [{ projectId: "p-b", projectType: "2" }],
  },
  {
    organizationId: "org-c",
    organizationName: null,
    projects: [{ projectId: "p-c", projectType: 2 }],
  },
];
const teamListCases: Record<
  string,
  { family?: "zai" | "bigmodel"; pricing: Scripted; customer?: Scripted }
> = {
  completeContext: {
    pricing: pricing([
      product({
        productId: "prod-1",
        subscribed: true,
        organizationId: "org-a",
        organizationName: "团队 A",
        projectId: "p-a1",
      }),
      product({
        productId: "prod-2",
        subscribed: false,
        organizationId: "org-b",
        organizationName: "团队 B",
        projectId: "p-b",
      }),
    ]),
    customer: { status: 500, body: {} },
  },
  completeContextZai: {
    family: "zai",
    pricing: pricing([
      product({
        productId: "prod-z",
        subscribed: true,
        organizationId: "org-a",
        organizationName: "团队 A",
        projectId: "p-a1",
      }),
    ]),
  },
  completeContextWithoutName: {
    pricing: pricing([
      product({
        productId: "prod-1",
        subscribed: true,
        organizationId: "org-a",
        projectId: "p-a1",
      }),
    ]),
  },
  organizationOnly: {
    pricing: pricing([product({ productId: "prod-1", subscribed: true, organizationId: "org-a" })]),
  },
  organizationOnlyZai: {
    family: "zai",
    pricing: pricing([product({ productId: "prod-1", subscribed: true, organizationId: "org-a" })]),
  },
  nameFromProduct: {
    pricing: pricing([
      product({
        productId: "prod-1",
        subscribed: true,
        organizationId: "org-c",
        organizationName: "产品上的名字",
      }),
    ]),
  },
  noSubscribedFallback: {
    pricing: pricing([
      product({ productId: "lite", tier: "LITE", subscribed: false, subscribePeriod: "QUARTERLY" }),
      product({ productId: "pro-once", subscribed: false, subscribeMode: "ONE_TIME" }),
      product({ productId: "pro", subscribed: null }),
    ]),
  },
  noSubscribedNoTeamProjects: {
    pricing: pricing([product({ productId: "pro", subscribed: false })]),
    customer: customerOf([
      { organizationId: "org-a", projects: [{ projectId: "p-own", projectType: 1 }] },
    ]),
  },
  twoProductsOneProject: {
    pricing: pricing([
      product({
        productId: "prod-1",
        subscribed: true,
        organizationId: "org-b",
        organizationName: "团队 B",
        projectId: "p-b",
      }),
      product({
        productId: "prod-2",
        subscribed: true,
        organizationId: "org-b",
        organizationName: "团队 B",
        projectId: "p-b",
      }),
    ]),
  },
  blankProductId: {
    pricing: pricing([
      product({
        productId: "  ",
        subscribed: true,
        organizationId: "org-b",
        organizationName: "团队 B",
        projectId: "p-b",
      }),
    ]),
  },
  emptyProductList: { pricing: pricing([]) },
  pricingCode0: { pricing: pricing([], { code: 0 }) },
  pricingSuccessFalse: { pricing: pricing([], { code: 200, success: false }) },
  pricingNoData: { pricing: { status: 200, body: { code: 200 } } },
  pricing500: { pricing: { status: 500, body: {} } },
  pricing401: { pricing: { status: 401, body: { code: 401 } } },
  customerNeededFails: {
    pricing: pricing([product({ productId: "prod-1", subscribed: true, organizationId: "org-a" })]),
    customer: { status: 500, body: {} },
  },
  customerNoEnvelope: {
    pricing: pricing([product({ productId: "prod-1", subscribed: true, organizationId: "org-b" })]),
    customer: { status: 200, body: { code: 1001, data: { organizations: TEAM_ORGS } } },
  },
  customerBadOrganizationId: {
    pricing: pricing([product({ productId: "prod-1", subscribed: true })]),
    customer: customerOf([{ organizationId: 7, projects: [] }]),
  },
};

async function teamListCase(
  family: "zai" | "bigmodel",
  script: { pricing: Scripted; customer?: Scripted },
) {
  const requests: Recorded[] = [];
  const apiClient = {
    request: async (input: string | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" ? input : input.toString();
      const entry = record(url, init);
      requests.push(entry);
      const path = entry.path.replace(/^\/+/, "/");
      const reply =
        path === "/api/biz/subscription/enterprise/v2/pricing"
          ? script.pricing
          : path === "/api/biz/customer/getCustomerInfo" && script.customer
            ? script.customer
            : path.endsWith("/api_keys") && entry.method === "GET"
              ? // App 列出时为每个团队项目预建 key（本工程不预建，D-F49-2）：给它一个合法的团队 key。
                {
                  status: 200,
                  body: ok([{ name: "zcode-team-api-key", apiKey: "key-team", keyType: 2 }]),
                }
              : { status: 404, body: {} };
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    },
  };
  const credentials: Record<string, string> = {
    [`oauth:${family}:access_token`]: family === "zai" ? "jwt-zai" : "at-big",
    zcodejwttoken: "zcode-jwt",
  };
  const Provider =
    family === "zai" ? ZaiCodingPlanSubscriptionProvider : BigModelCodingPlanSubscriptionProvider;
  const provider = new Provider({
    apiClient,
    credentialService: { load: async (key: string) => credentials[key] ?? null },
  });
  let response: { productList: Record<string, Json>[] };
  try {
    response = await provider.getEnterprisePricing();
  } catch {
    return {
      rows: null,
      pricingRequest: requests.find((r) => r.path.endsWith("/pricing")) ?? null,
    };
  }
  const providerId = `account:${family}-team-coding-plan`;
  const groups = [
    {
      key: `registry-provider:${providerId}`,
      label: family,
      items: [{ key: "m", value: "m", name: "GLM-5.3" }],
    },
  ];
  const products = response.productList.map((p) => ({ ...p, family }));
  const rows = buildComposerPlanGroups(groups, products)
    .filter(
      (group: { connection?: { selection?: { kind: string } } }) =>
        group.connection?.selection?.kind === "team-coding-plan",
    )
    .map((group: { connection: { label: string; selection: Record<string, string> } }) => ({
      productId: group.connection.selection.productId,
      organizationId: group.connection.selection.organizationId,
      projectId: group.connection.selection.projectId,
      name: group.connection.label,
    }));
  return { rows, pricingRequest: requests.find((r) => r.path.endsWith("/pricing")) ?? null };
}

const teamLists: Record<string, Json> = {};
for (const [name, script] of Object.entries(teamListCases)) {
  const family = script.family ?? "bigmodel";
  // 缺省的 customerInfo 也写进夹具（本工程的测试照它回复）。
  const effective = { ...script, customer: script.customer ?? customerOf(TEAM_ORGS) };
  teamLists[name] = { family, script: effective, ...(await teamListCase(family, effective)) };
}

writeFileSync(
  outFile,
  `${JSON.stringify({ tokenCustomer: CUSTOMER, tokens, entitlements, detailReasons, teamLists }, null, 2)}\n`,
);
process.exit(0);
