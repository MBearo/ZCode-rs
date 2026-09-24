// Edit matching parity cases computed by Node `edit-matchers.ts`.
// Imported by generate-zcode-cli-rust-fixtures.mjs.
import {
  findEditMatch,
  normalizeReplacementForMatch,
  preserveQuoteStyle,
} from "../apps/zcode-cli/packages/core/src/tool/edit-matchers.ts";

const CASES = [
  ["alpha beta alpha", "beta", false, "gamma"],
  ["a x a", "a", false, "b"],
  ["it’s “quoted” text", `it's "quoted"`, false, `was "new" and 'x' don't`],
  ["say ‘hi’ now", "say 'hi'", false, "say 'bye' (x) 'y' —'z'"],
  ["“a” and ”a“", '"a"', false, "b"],
  ["line one\nline two\n", "1: line one\n2: line two", false, "x"],
  ["line one\nline two", "1\tline one\n2\tline two", false, "x"],
  ["line one\r", "1: line one\r", false, "x"],
  ["12: literal", "12: literal", false, "x"],
  ["a\tb", "a\\tb", false, "c\\td\\q"],
  ['say "hi"', 'say \\"hi\\"', false, "x"],
  ["price $5 and \\n", "price \\$5 and \\\\n", false, "cost \\$6"],
  ["é and \\u0041", "\\u00e9 and \\\\u0041", false, "e"],
  ["😀 face", "\\ud83d\\ude00 face", false, "x"],
  ["x", "\\ud83d", false, "y"],
  ["x\\u00zz", "x\\u00zz", false, "y"],
  ["  foo()\n    bar()\n  baz()", "foo()\nbar()\nbaz()", false, "q"],
  ["  foo()\n    bar()\n  baz()", "foo()\nbar()\nbaz()", true, "q"],
  ["  x\n\tx", " x ", false, "y"],
  ["  x\n\tx", " x ", true, "y"],
  ["﻿foo", "  foo  ", false, "bar"],
  ["\u0085foo", " foo", false, "bar"],
  ["fn a() {\n  let x = 1;\n  return x;\n}", "fn a() {\n  let y = 1;\n  return x;\n}", false, "z"],
  ["fn a() {\n  completely different;\n}", "fn a() {\n  let y = 1;\n}", false, "z"],
  ["start\n  😀😀 middle\nend", "start\n😀x middle\nend", false, "q"],
  ["first\nsecond\n", "first\nsecond\n\n", false, "z"],
  ["one\n\ntwo", "one\n\ntwo\n", false, "z"],
  ["don’t stop", "don't stop", false, "can't 'go' \"now\""],
  ["(“x”)", '("x")', false, '("y") [\'z\'] {"w"}'],
];

/** Deterministic PRNG (mulberry32). */
function random(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ALPHABET = [
  "a",
  "b",
  " ",
  "\t",
  "\n",
  "'",
  '"',
  "‘",
  "’",
  "“",
  "”",
  "\\",
  "n",
  "u",
  "0",
  ":",
  "é",
  "😀",
  "—",
  "(",
  "1",
];

function perturb(next, text) {
  switch (Math.floor(next() * 6)) {
    case 0:
      return text.replace(/[‘’]/g, "'").replace(/[“”]/g, '"');
    case 1:
      return text
        .split("\n")
        .map((line, index) => `${index + 1}${next() < 0.5 ? ": " : "\t"}${line}`)
        .join("\n");
    case 2:
      return text.replace(/\t/g, "\\t").replace(/\n/g, "\\n");
    case 3:
      return text.replace(/é/g, "\\u00e9");
    case 4:
      return text
        .split("\n")
        .map((line) => (next() < 0.5 ? `  ${line.trim()}` : line.trim()))
        .join("\n");
    default:
      return text;
  }
}

function generated(count) {
  const next = random(20260924);
  const pick = () => ALPHABET[Math.floor(next() * ALPHABET.length)];
  const cases = [];
  for (let i = 0; i < count; i += 1) {
    const length = 1 + Math.floor(next() * 40);
    const content = Array.from({ length }, pick).join("");
    const chars = [...content];
    const from = Math.floor(next() * chars.length);
    const to = from + 1 + Math.floor(next() * (chars.length - from));
    const search = perturb(next, chars.slice(from, to).join(""));
    const newString = Array.from({ length: Math.floor(next() * 8) }, pick).join("");
    cases.push([content, search, next() < 0.3, newString]);
  }
  return cases;
}

export function editMatchFixtures() {
  return [...CASES, ...generated(400)]
    .filter(([, search]) => search.length > 0)
    .map(([content, search, replaceAll, newString]) => {
      const result = findEditMatch({ content, search, replaceAll });
      const replacement =
        result.status === "matched"
          ? preserveQuoteStyle(
              search,
              result.actualString,
              normalizeReplacementForMatch(result.strategy, newString),
            )
          : undefined;
      return { content, search, replaceAll, newString, result, replacement };
    });
}
