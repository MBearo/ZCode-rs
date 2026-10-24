// 指标脚本的纯函数（docs/specs/rust-cli/remote-exec.md §15.6）：
//   node --test scripts/rust-cli/boundaries/metrics.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";

import { abstractness, nccd, propagationCost, referencedNames } from "./metrics.mjs";

const graphOf = (edges) =>
  new Map(Object.entries(edges).map(([crate, to]) => [crate, new Set(to)]));

test("传播成本是可达矩阵（含对角线）的密度；NCCD 以平衡二叉树为 1", () => {
  // 一条链 a → b → c：a 到 3 个、b 到 2 个、c 到 1 个，共 6 / 9。
  const chain = graphOf({ a: ["b"], b: ["c"], c: [] });
  assert.equal(propagationCost(chain), 6 / 9);
  // 三个节点的平衡二叉树：根依赖两个叶子，CCD = 3 + 1 + 1 = 5 = (3+1)·log2(4) − 3。
  const tree = graphOf({ root: ["left", "right"], left: [], right: [] });
  assert.equal(nccd(tree), 1);
  assert.equal(propagationCost(graphOf({ a: [], b: [] })), 0.5);
});

test("抽象度只数产品代码里的声明；注释与字符串里的不算", () => {
  const sources = [
    'pub trait Port { fn x(&self); }\npub struct A;\nenum B { C }\n// struct NotCounted\nconst S: &str = "trait Fake";',
  ];
  assert.deepEqual(abstractness(sources), { traits: 1, concrete: 2, a: 1 / 3 });
});

test("接口宽度：经 rust_cli_<crate>:: 用到的不同名字，use 的花括号展开", () => {
  const sources = [
    'use rust_cli_kernel_api::env::{EnvError, EnvPath as Path};\nuse rust_cli_kernel_api::machine::MachineEnv;\nfn f() { rust_cli_kernel_api::ids::SessionId::new("x"); }',
  ];
  const names = [...referencedNames(sources, "kernel-api")].sort();
  assert.deepEqual(names, ["EnvError", "EnvPath", "MachineEnv", "SessionId"]);
});
