// WebFetch parity cases computed by Node `webfetch-url.ts`, `webfetch-egress-guard.ts`,
// `webfetch-content.ts` and `webfetch-processing.ts`.
// Imported by generate-zcode-cli-rust-fixtures.mjs.
import { STATUS_CODES } from "node:http";
import {
  isPermittedRedirect,
  normalizeWebFetchUrl,
  redactUrlCredentials,
  resolveRedirectUrl,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/webfetch-url.ts";
import { assertWebFetchLiteralEgress } from "../apps/zcode-cli/packages/core/src/tool/handlers/webfetch-egress-guard.ts";
import {
  extractReadableContent,
  truncateContentForModel,
} from "../apps/zcode-cli/packages/core/src/tool/handlers/webfetch-content.ts";
import { processFetchedContent } from "../apps/zcode-cli/packages/core/src/tool/handlers/webfetch-processing.ts";

function failure(error) {
  return { code: error.context?.webFetchCode ?? null, message: error.message };
}

const NORMALIZE = [
  "http://example.com/a?b#c",
  "  https://example.com  ",
  "https://EXAMPLE.com/Path",
  "https://例子.测试/",
  "https://0x7f.1/",
  "https://2130706433/",
  "https://example.com./",
  "https://intranet/",
  "http://example.com:8080/x",
  "http://example.com:443/x",
  "https://example.com:443/x",
  "ftp://example.com/",
  "https://user:pw@example.com/",
  "https://user@example.com/",
  "https://localhost/",
  "https://a.localhost/",
  "https://printer.local/",
  "https://[::1]/",
  "https://10.0.0.1/",
  "https://[2001:4860:4860::8888]/",
  "not a url",
  "https://",
  `https://example.com/${"a".repeat(1990)}`,
  `https://example.com/${"a".repeat(1980)}`,
  "https://exa mple.com/",
  "HTTPS://Example.COM:0443/%7euser?q=a b",
];

const REDIRECTS = [
  ["https://example.com/a", "/b"],
  ["https://example.com/a", "https://www.example.com/a"],
  ["https://www.example.com/a", "https://example.com/a"],
  ["https://example.com/a", "http://example.com/a"],
  ["https://example.com/a", "https://other.com/a"],
  ["https://example.com/a", "https://example.com:8443/a"],
  ["https://example.com/a", "https://user:pw@example.com/a"],
  ["https://example.com/a", "https://127.0.0.1/a"],
  ["https://example.com/a", "https://sub.example.com/a"],
  ["https://example.com:443/a", "https://example.com/b"],
  ["https://example.com/a", "https://localhost/a"],
  ["https://example.com/a", "//example.com/c?x#y"],
  ["https://example.com/a/b", "../c"],
  ["https://example.com/a", "https://[::1]/"],
];

const EGRESS = [
  "https://10.0.0.1/",
  "https://127.0.0.1/",
  "https://[::1]/",
  "https://169.254.169.254/",
  "https://[::ffff:127.0.0.1]/",
  "https://[::ffff:8.8.8.8]/",
  "https://[64:ff9b::a00:1]/",
  "https://[64:ff9b::808:808]/",
  "https://198.18.0.1/",
  "https://198.20.0.1/",
  "https://100.64.0.1/",
  "https://8.8.8.8/",
  "https://[2001:4860:4860::8888]/",
  "https://0.0.0.0/",
  "https://255.255.255.255/",
  "https://224.0.0.1/",
  "https://172.16.5.4/",
  "https://172.32.0.1/",
  "https://192.168.1.1/",
  "https://192.0.0.8/",
  "https://192.0.2.1/",
  "https://192.88.99.1/",
  "https://198.51.100.1/",
  "https://203.0.113.9/",
  "https://240.0.0.1/",
  "https://[fe80::1]/",
  "https://[ff02::1]/",
  "https://[fc00::1]/",
  "https://[fd12::1]/",
  "https://[::]/",
  "https://[2002::1]/",
  "https://[2001::1]/",
  "https://[2001:db8::1]/",
  "https://[64:ff9b:1::1]/",
  "https://[100::1]/",
  "https://[2001:2::1]/",
  "https://[2001:10::1]/",
  "https://[2001:20::1]/",
  "https://[2001:30::1]/",
  "https://localhost/",
  "https://x.localhost/",
  "https://example.com/",
  "https://printer.local/",
];

const PAGE = `<!DOCTYPE html><html><head><title>T</title><style>p{color:red}</style>
<script>var x = "<p>";</script></head><body><!-- hidden -->
<h1 class="x">Hello &amp; World</h1><p>Para one<br>line2</p>
<ul><li>a</li><li>b <a href="https://x.com/y">link</a></li></ul><noscript>no</noscript>
<p>&lt;tag&gt; &quot;q&quot; &#39;s&#39; &#65;&#x42; x</p><table><tr><td>c1</td><td>c2</td></tr></table>
<img alt="pic" src="p.png"><pre>code
   block</pre><div>end</div></body></html>`;

const EXTRACT = [
  [PAGE, "text/html; charset=utf-8"],
  ["&amp;lt;b&amp;gt; x", "text/html"],
  ["a < b and c > d", "text/html"],
  [
    "<A HREF='https://u.test/p'>Up<B>!</B></A> <a href=https://x>unq</a> <a class=x>nohref</a>",
    "text/html",
  ],
  ["<ul><li>one<ul><li>two</li></ul></li></ul>", "text/html"],
  ["<p>l1</p>\r\n\r\n\r\n<p>l2</p>", "text/html"],
  ["<scripts>keep</scripts><script-x>drop</script>", "text/html"],
  ["x&#99999999;y", "text/html"],
  ["x&#x110000;y", "text/html"],
  ["&#xD83D;&#xDE00; &#128512; &#X41;", "text/html"],
  ["<br class=x>a<br>b<BR/>c", "text/html"],
  ["  lead \t tail  ", "text/html"],
  ["<SCRIPT type=x>a</SCRIPT >b<STYLE>c</style>d", "text/html"],
  ["<h2>Two</h2><h6 id=z>Six</h6>text", "text/x-html-ish"],
  ["<p>kept raw</p>", ""],
  ["﻿  hi\r\n", "text/plain"],
  ['{"a": 1}', "application/json"],
  ["<x/>", "application/atom+xml"],
  ["png", "image/png"],
  ["pdf", "Application/PDF; x=1"],
  ["&#x26;#65;", "text/html"],
  ["<p>\n\n</p>\n\n\n<p>x</p>", "TEXT/HTML"],
];

function base64(text) {
  return Buffer.from(text, "utf8").toString("base64");
}

const BYTES = [
  [Buffer.from([0xef, 0xbb, 0xbf, 0x68, 0x69]).toString("base64"), "text/plain"],
  [Buffer.from([0x63, 0x61, 0x66, 0xe9]).toString("base64"), "text/plain; charset=iso-8859-1"],
  [Buffer.from([0x61, 0xf0, 0x9f, 0x98, 0x62]).toString("base64"), "text/plain"],
];

async function prompts() {
  const cases = [
    { content: "page body", prompt: "Summarize", preapproved: false, contentType: "text/html" },
    { content: "doc body", prompt: "What?", preapproved: true, contentType: "text/html" },
    { content: "# md", prompt: "What?", preapproved: true, contentType: "text/markdown" },
    { content: "# md", prompt: "What?", preapproved: false, contentType: "text/markdown" },
    { content: "x".repeat(100_001), prompt: "Big", preapproved: false, contentType: "text/plain" },
  ];
  const out = [];
  for (const c of cases) {
    const captured = [];
    const context = {
      toolCallId: "call",
      traceId: "trace",
      model: {
        optionSpecs: {
          reasoningLevel: { values: ["low", "high"] },
          maxOutputTokens: { max: 2048 },
        },
        generateText: async (request) => {
          captured.push(request);
          return { text: "  answer  " };
        },
      },
    };
    const fetched = { content: c.content, contentType: c.contentType, finalUrl: "https://x/" };
    const result = await processFetchedContent(
      { url: "https://x/", prompt: c.prompt },
      fetched,
      context,
      {
        preapprovedUrl: c.preapproved,
      },
    );
    const message = captured[0]?.messages?.[0]?.content;
    out.push({
      ...c,
      content: c.content.length > 1000 ? `x*${c.content.length}` : c.content,
      result,
      message:
        message && message.length > 1000
          ? { length: message.length, head: message.slice(0, 40), tail: message.slice(-700) }
          : (message ?? null),
      options: captured[0]?.options ?? null,
    });
  }
  return out;
}

/** Node `http.STATUS_CODES`: WebFetch's reason phrase fallback. */
export function httpStatusCodes() {
  return STATUS_CODES;
}

export async function webFixtures() {
  const run = (fn) => {
    try {
      return { ok: fn() };
    } catch (error) {
      return { error: failure(error) };
    }
  };
  return {
    normalize: NORMALIZE.map((input) => ({
      input,
      ...run(() => normalizeWebFetchUrl(input).href),
    })),
    redirects: REDIRECTS.map(([from, location]) => {
      const next = resolveRedirectUrl(location, new URL(from));
      return {
        from,
        location,
        next: next.href,
        redacted: redactUrlCredentials(next),
        permitted: isPermittedRedirect(new URL(from), next),
      };
    }),
    egress: EGRESS.map((url) => ({
      url,
      ...run(() => (assertWebFetchLiteralEgress(new URL(url)), true)),
    })),
    extract: [
      ...EXTRACT.map(([text, contentType]) => ({
        body: base64(text),
        contentType,
        ...run(() => extractReadableContent(Buffer.from(text, "utf8"), contentType)),
      })),
      ...BYTES.map(([body, contentType]) => ({
        body,
        contentType,
        ...run(() => extractReadableContent(Buffer.from(body, "base64"), contentType)),
      })),
    ],
    truncate: [99_999, 100_000, 100_001, 250_000].map((length) => {
      const { content, truncated } = truncateContentForModel("y".repeat(length));
      return { length, truncated, resultLength: content.length, tail: content.slice(-60) };
    }),
    prompts: await prompts(),
  };
}
