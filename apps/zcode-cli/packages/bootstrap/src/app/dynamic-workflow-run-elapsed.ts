// dwf run 的活动时长，用于完成卡的「时间」格。
// 本 run 的每次启动，以及沿 `resumedFrom` 上溯的每个前驱，都按各自活动区间求和；
// 停止或进程退出后、下次 resume 前的空档不计入。
//
// 不能只用当前注册表条目的 `completedAt - startedAt`：resume 或修订会重置该时钟，
// 从而漏掉之前的运行时间。时长与 token 用量都应覆盖整条 lineage。
//
// 事件日志已记录每段区间的起点和最后活动时刻，因此只需读取求和，无须额外持久化。

import type { JournalStorePort } from "@zcode/dynamic-workflow";
import { supportsRunLifeSpans } from "./dynamic-workflow-run-journal.js";

/**
 * lineage 上溯的跳数上限。一次修订一跳，链条实际是个位数；上限只为「行里的 `resumedFrom`
 * 被外力写成一条长链」留一道闸——读一条 run 的时长不该扫过任意多行。
 */
const LINEAGE_HOP_LIMIT = 64;

/**
 * 本 run 及其 lineage 的活动时长（毫秒），或 `undefined`（无任何一世的证据）。
 *
 * `undefined` 与 `0` 是两件事：前者是「journal 说不出话」（读面不在场、run 的事件早于本记账、
 * 行已被清理），调用方据此退回自己观察到的那一世；后者是「确有一世，但它的时长不足 1 毫秒」。
 *
 * 防环不是防御性编程的摆设：`resumedFrom` 是建 run 那一刻写死的元数据，理论上不成环，但这个
 * 循环的终止条件依赖的是**库里的数据**而不是本进程的逻辑——一条被外力写成自指的行会把一次
 * 读快照变成死循环，而快照读在后台追踪器的轮询路径上。
 */
export function runLineageActiveMs(journal: JournalStorePort, runId: string): number | undefined {
  if (!supportsRunLifeSpans(journal)) return undefined;
  let total = 0;
  let sawLife = false;
  let cursor: string | undefined = runId;
  const visited = new Set<string>();
  for (let hop = 0; cursor !== undefined && hop < LINEAGE_HOP_LIMIT; hop += 1) {
    if (visited.has(cursor)) break;
    visited.add(cursor);
    for (const life of journal.listRunLifeSpans(cursor)) {
      sawLife = true;
      // 钳到非负：两个时刻同源于 `dwf_event.time_created`，但那是**墙钟**——一次系统对时可以
      // 让「最后一条」早于「第一条」。负数会从总和里减掉别的世的真实时长，那比丢掉这一世更糟。
      total += Math.max(0, life.lastActivityAt - life.startedAt);
    }
    // 只上溯 lineage，不下溯 supersededBy：修订是「同一件工作的下一版」，而被替代的前驱的活
    // 是这一版的底座；反方向的后继与本 run 的时长无关（它有自己的卡）。
    cursor = journal.getRun(cursor)?.resumedFrom;
  }
  return sawLife ? total : undefined;
}
