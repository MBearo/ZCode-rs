// ============================================================
// workflow 并发天花板：CPU 推导值的唯一实现
// ============================================================
// `max(1, min(16, availableParallelism() − 2))`
// 既是每个 run 自己的并发上界（提交时定、落 dwf_run、resume 照用），也是进程级治理器每个
// provider 桶的**起点与天花板**。公式必须收敛成单一函数：复制在 run service / snippet
// service / legacy `Workflow` 工具三处时各自漂移，会让「run 上界」与「桶天花板」对不上。

import { availableParallelism as osAvailableParallelism } from "node:os";

/** 并发上界的硬顶（沿用 legacy 并发式）。 */
const WORKFLOW_CONCURRENCY_CEILING_MAX = 16;
/** 给主代理与宿主进程留出的核数。 */
const RESERVED_PARALLELISM = 2;
/** 地板：双核机器上 parallelism − 2 == 0，必须至少留一个探针在跑。 */
const WORKFLOW_CONCURRENCY_FLOOR = 1;

/**
 * CPU 推导的并发天花板。`availableParallelism` 可注入，供测试固定核数（地板/硬顶两条用例）。
 */
export function resolveWorkflowConcurrencyCeiling(
  availableParallelism: () => number = osAvailableParallelism,
): number {
  return Math.max(
    WORKFLOW_CONCURRENCY_FLOOR,
    Math.min(WORKFLOW_CONCURRENCY_CEILING_MAX, availableParallelism() - RESERVED_PARALLELISM),
  );
}

/**
 * 请求的并发上界 → 本 run 实际生效的上界。
 *
 * **钳制而不是拒绝**：这个旋钮只为压低并发，一个过大的值表达的意愿是「别限制我」，把它变成
 * 一次工具失败只会让模型去猜机器有几个核。缺席 / 非有限数同样读作「不限制」= 天花板，非整数
 * 向下取整（要「3.7 个在飞的 ask」没有意义，而向上取整会偷偷越过用户说的数）。
 *
 * 与天花板同住一个文件：提交时定上界与中途 retune 走的**必须**是同一条钳制（否则同一个
 * `max_concurrency` 经两条路会落成两个数），而那两条路分居 run service 与 retune 两个模块。
 */
export function clampRunConcurrency(requested: number | undefined, ceiling: number): number {
  if (requested === undefined || !Number.isFinite(requested)) return ceiling;
  return Math.max(1, Math.min(ceiling, Math.floor(requested)));
}
