// Network egress parity cases for the Rust `net` crate, computed by the TS
// implementations. Imported by generate-zcode-cli-rust-fixtures.mjs.
import { prepareCliRuntimeEnv } from "../apps/zcode-cli/packages/cli/src/env.ts";
import {
  resolveProxyForRequest,
  resolveWebFetchProxyForRequest,
} from "../apps/zcode-cli/packages/adapters/src/network/http-config.ts";
import { applyNetworkEgressEnv } from "../apps/zcode-cli/packages/adapters/src/network/subprocess-env.ts";
import { resolveOfficialCodingPlanGatewayUrl } from "../apps/zcode-cli/packages/adapters/src/model/official-coding-plan-gateway.ts";
import { mergeModelRequestHeaders } from "../apps/zcode-cli/packages/adapters/src/model/model-request-headers.ts";
import { createModelRequestAttributionHeaders } from "../apps/zcode-cli/packages/adapters/src/model/runner-attribution.ts";
import { createRuntimeAiSdkModelExecutionConfig } from "../apps/zcode-cli/packages/bootstrap/src/model-config.ts";
import { withOpenRouterAttributionHeaders } from "../packages/shared/src/openrouter-attribution.ts";
import { sanitizeZCodeRuntimeEnv } from "../packages/shared/src/runtimeEnv.ts";

const passthrough = JSON.stringify({ HTTPS_PROXY: "http://corp:3128", NO_PROXY: "intra" });
const proxyCases = [
  ["https://api.x.com/v1", { httpProxy: "proxy.local:8080" }],
  ["https://api.x.com", { httpProxy: " ", env: { ZCODE_HTTP_PROXY: "socks5://s:1080" } }],
  ["https://api.x.com", { httpProxy: "http://u:p@proxy:8080" }],
  ["https://api.x.com", { httpProxy: "HTTP://Proxy.Local:80" }],
  ["https://api.x.com", { httpProxy: "http://[bad" }],
  ["ftp://x.test/a", { httpProxy: "p:1" }],
  ["not a url", { httpProxy: "p:1" }],
  ["https://x.test", { httpProxy: "p:1", noProxy: "*" }],
  ["https://x.test", { httpProxy: "p:1", env: { ZCODE_NO_PROXY: " x.test " } }],
  ["https://x.test", { httpProxy: "p:1", noProxy: " ", env: { ZCODE_NO_PROXY: "x.test" } }],
  ["http://[::]/", { httpProxy: "p:1", noProxy: "[::1" }],
  ["http://[fe80::1]/", { httpProxy: "p:1", noProxy: "fe80::1" }],
  ["https://intra.site", { env: { ZCODE_TOOL_ENV_PASSTHROUGH_JSON: passthrough } }],
];
const noProxy =
  "localhost,.internal.com,*.corp,[::1],10.0.0.1:8443,http://svc:9000,  , example.org:";
for (const url of [
  "http://localhost:3000",
  "https://a.internal.com",
  "https://internal.com",
  "https://x.corp",
  "http://[::1]:8080",
  "https://10.0.0.1:8443",
  "https://10.0.0.1",
  "http://svc:9000/",
  "http://svc",
  "https://example.org",
  "https://notexample.org",
  "https://LOCALHOST./a",
])
  proxyCases.push([url, { httpProxy: "p:1", noProxy }]);
for (const url of ["https://a.intra", "https://example.com", "http://example.com"])
  proxyCases.push([url, { env: { ZCODE_TOOL_ENV_PASSTHROUGH_JSON: passthrough } }]);
proxyCases.push([
  "https://example.com",
  { httpProxy: "explicit:1", env: { ZCODE_TOOL_ENV_PASSTHROUGH_JSON: passthrough } },
]);
const proxy = proxyCases.map(([url, options]) => ({
  url,
  options,
  request: resolveProxyForRequest(url, options),
  webFetch: resolveWebFetchProxyForRequest(url, options),
}));

const shellPassthrough = JSON.stringify({
  HTTP_PROXY: "http://shell",
  npm_config_proxy: "x",
  OTEL_X: "y",
  "bad-key": "z",
  NODE_ENV: "q",
  SSL_CERT_FILE: 3,
});
const childCases = [
  [
    "linux",
    {
      PATH: "/bin",
      HTTP_PROXY: "http://inherited",
      NODE_ENV: "dev",
      ZCODE_TOOL_ENV_PASSTHROUGH_JSON: shellPassthrough,
      ZCODE_HTTP_PROXY: "",
      ZCODE_NO_PROXY: " corp ",
      ZCODE_AGENT_CA_CERT: "/ca.pem",
      npm_config_https_proxy: "p",
      ZCODE_CUA_PERMISSION_BROKER_SOCKET: "/s",
      OTEL_SERVICE_NAME: "svc",
    },
    {},
  ],
  [
    "linux",
    { PATH: "/bin", ZCODE_HTTP_PROXY: "env-proxy:1" },
    { httpProxy: "cfg:1", noProxy: "a", caCertFile: " /c.pem " },
  ],
  ["linux", { PATH: "/bin", ZCODE_HTTP_PROXY: " socks5://e:1 " }, { httpProxy: "  " }],
  [
    "win32",
    {
      Path: "C:\\bin",
      http_proxy: "x",
      No_Proxy: "old",
      zcode_http_proxy: "win-env:8",
      ZCODE_TOOL_ENV_PASSTHROUGH_JSON: shellPassthrough,
    },
    { noProxy: "n" },
  ],
];
const childEnv = childCases.map(([platform, sourceEnv, network]) => ({
  platform,
  sourceEnv,
  network,
  expected: applyNetworkEgressEnv(sanitizeZCodeRuntimeEnv(sourceEnv), {
    network,
    platform,
    sourceEnv,
  }),
}));

const runtimeCases = [
  {
    PATH: "/bin",
    HTTP_PROXY: "http://a",
    https_proxy: "http://b",
    SSL_CERT_FILE: "/c",
    OTEL_EXPORTER_OTLP_ENDPOINT: "x",
    ZCODE_TELEMETRY_DEVICE_MID: "d",
    ZCODE_TOOL_ENV_PASSTHROUGH_JSON: JSON.stringify({ NO_PROXY: "n", NODE_ENV: "x", _a: "u" }),
    NODE_ENV: "production",
    ZCODE_RUNTIME_ENV: " TEST ",
    yarn_https_proxy: "y",
    ZCODE_CUA_PERMISSION_BROKER_TOKEN: "t",
    GIT_SSL_CAINFO: "/g",
    NODE_EXTRA_CA_CERTS: "/n",
    all_proxy: "socks5://z",
  },
  {},
  { ZCODE_TOOL_ENV_PASSTHROUGH_JSON: "{bad", ZCODE_RUNTIME_ENV: "staging" },
  { ZCODE_TOOL_ENV_PASSTHROUGH_JSON: "[1]", NO_PROXY: "", ZCODE_STORAGE_DIR: "/s" },
];
const runtimeEnv = runtimeCases.map((env) => ({
  env,
  expected: prepareCliRuntimeEnv(env, ["node", "/usr/bin/zcode", "app-server"]),
}));

const gatewayUrls = [
  "https://open.bigmodel.cn/api/anthropic/v1/messages",
  "https://OPEN.BIGMODEL.CN:443/api/anthropic/v1/messages/?beta=true",
  "https://api.z.ai/api/anthropic/v1/messages?x=1#frag",
  "http://api.z.ai/api/anthropic/v1/messages",
  "https://api.z.ai:8443/api/anthropic/v1/messages",
  "https://api.z.ai/api/anthropic/v1/messages//",
  "https://open.bigmodel.cn/api/anthropic/v1/messages?",
  "https://user:pw@api.z.ai/api/anthropic/v1/messages",
  "https://api.z.ai/api/anthropic/v1",
  "not a url",
];
const gateway = [{}, { ZCODE_BASE_URL: " ", ZCODE_ENDPOINT_ORIGIN: "http://localhost:8080/x?y" }]
  .flatMap((env) => gatewayUrls.map((url) => ({ env, url })))
  .map((item) => ({ ...item, expected: resolveOfficialCodingPlanGatewayUrl(item.url, item.env) }));

const mergeCases = [
  [{ A: "1", "content-type": "x" }, { a: "2" }, undefined, { "Content-Type": "y" }],
  [{ Authorization: "Bearer k" }, { authorization: "Bearer other", "X-Extra": "e" }],
];
const mergeHeaders = mergeCases.map((sources) => ({
  sources: sources.map((s) => s ?? null),
  expected: mergeModelRequestHeaders(...sources),
}));

const attributionCases = [
  {
    requestId: "r",
    traceId: "t",
    queryId: "query_abc",
    sessionId: "sess_xyz",
    modelRequestSessionType: "main",
  },
  {
    requestId: "r2",
    traceId: "t2",
    queryId: "query_",
    sessionId: "subagent_agent_123",
    modelRequestSessionType: "bogus",
    baseURL: "https://opencode.ai/zen/go/v1/",
  },
  {
    requestId: "r3",
    traceId: "t3",
    sessionId: "sess_subagent_agent_q",
    modelRequestSessionType: "subagent",
    baseURL: "https://api.opencode.ai/zen/go/v2",
  },
  { requestId: "r4", traceId: "t4", modelRequestSessionType: "other" },
];
const attribution = attributionCases.map((context) => ({
  context,
  expected: createModelRequestAttributionHeaders(context),
}));

const openRouter = [
  "https://openrouter.ai/api/v1",
  "http://openrouter.ai",
  "https://EU.OpenRouter.ai/v1",
  "https://evilopenrouter.ai",
  null,
].map((baseUrl) => ({
  baseUrl,
  expected: withOpenRouterAttributionHeaders(
    { "X-Title": "Z Code@electron" },
    baseUrl ?? undefined,
  ),
}));

const identityKeys = [
  "HTTP-Referer",
  "User-Agent",
  "X-ZCode-App-Version",
  "X-Title",
  "X-Release-Channel",
  "X-ZCode-Agent",
];
const identity = [
  {},
  {
    ZCODE_APP_VERSION: "",
    ZCODE_ENV: " Test ",
    ZCODE_BASE_URL: " ",
    ZCODE_ENDPOINT_ORIGIN: "http://127.0.0.1:9/p?q",
  },
  { ZCODE_APP_VERSION: " 9.9 ", ZCODE_BASE_URL: "https://a.example.com/x" },
  { ZCODE_APP_VERSION: "版本" },
].map((env) => {
  const { defaultHeaders } = createRuntimeAiSdkModelExecutionConfig(env, {
    appVersion: "1.2.3",
    sourceTitle: "electron",
  });
  return {
    env,
    expected: Object.fromEntries(
      identityKeys.filter((key) => key in defaultHeaders).map((key) => [key, defaultHeaders[key]]),
    ),
  };
});

export function egressFixtures() {
  return {
    proxy,
    childEnv,
    runtimeEnv,
    gateway,
    mergeHeaders,
    attribution,
    openRouter,
    identity,
  };
}
