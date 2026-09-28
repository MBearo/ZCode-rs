/**
 * dwf-journal.ts 顶到 oxlint max-lines 上限（400 行），把宿主侧 **run 内省读面**
 * 一族（`DwfRunIntrospectionQueries` 及其 SQL）拆到本文件；公开面仍从 dwf-journal.ts 导出，
 * `SqliteDwfJournalStore` 只做委托。
 *
 * 与 dwf-journal-artifacts.ts 同一种分法：这些查询只要一个 db 句柄，与 run/actor/node/event
 * 的写入-读取无共享状态，而各自带着一大段「取数源为何是这张表、排序为何是这个」的论证。
 * 引擎的 `JournalStorePort` 写面不在这里——这里没有任何写入者。
 */

import type { DatabaseSync } from "node:sqlite";
import type { NodeRecord, NodeRecordStatus, RunStatus, StoredEvent } from "@zcode/dynamic-workflow";
import type { DwfArtifactItem, DwfArtifactItemsQuery } from "./dwf-journal-artifacts.js";
import {
  decodeEvent,
  decodeNode,
  decodeRunDetailRow,
  decodeRunListItem,
  decodeRunSessionListItem,
  type DwfEventRow,
  type DwfNodeRow,
  type DwfWorldNodeRow,
  type DwfRunDetailRow,
  type DwfRunListItem,
  type DwfRunMetadataRow,
  type DwfRunRow,
  type DwfRunSessionListItem,
  type DwfRunSessionRow,
  encodeRunStatusPredicate,
} from "./dwf-journal-codecs.js";

/** {@link DwfRunIntrospectionQueries.listRuns} 的查询袋。 */
export interface DwfListRunsQuery {
  /**
   * 项目键。字面等值匹配 `dwf_run.cwd`（写入侧原样落，读侧原样查）。
   *
   * **可选**：缺省即不加 cwd 谓词，跨所有项目枚举。全局工作流的运行历史横跨它被发起过的每个
   * 项目（`workflows/runs` 的 `scope: "global"` 变体）；项目档变体仍传 cwd，行为逐字不变。
   */
  cwd?: string;
  /**
   * 返回行数上限，**必填**。钳制策略属于调用方（工具面钳到 [1, 50]）；存储层不替它猜一个
   * 默认值——一条无界的枚举查询是这里唯一不该有的形状。
   *
   * 但**别在这里加自己的天花板**（如 `Math.min(50, limit)`）。调用方合法地传「钳制上限 + 1」：
   * run service 多取一条来判定 `truncated`（多取的那条不进页）。一个 50 的硬顶会把探测行悄悄
   * 吃掉，于是 `truncated` 在**恰好** limit = 50 时永久缺席——正是用户最需要知道"还有更多"的
   * 那个页大小，而且没有任何测试会在别的 limit 上发现它。
   */
  limit: number;
  /** 可选状态子集。缺省即不过滤；空数组即「不匹配任何状态」（回空页）。 */
  statuses?: readonly RunStatus[];
  /**
   * 可选的 run 名字（`dwf_run.name` 字面等值）。GUI 中枢按「工作流名 = run 名」归属运行历史，过滤下推到 SQL 而不是取一页再筛——否则一个高频
   * 工作流会把别的工作流挤出页外，卡片上的「上次运行」就是错的。
   */
  name?: string;
}

/** dwf_node 的三态计数（`NodeRecordStatus` 的全部取值，三个键恒在场）。 */
export interface DwfNodeStatusCounts {
  completed: number;
  failed: number;
  running: number;
}

/**
 * 一个 run 的**一世**（一次 `run-started` 到它最后一条事件）的起止，epoch 毫秒。
 *
 * 一世就是「引擎活着的一段」：崩溃或停止的那一世没有 `run-settled`，所以收尾只能由「这一世
 * 记下的最后一条事件」界定——那正是它最后一次动的时刻。两个时刻同源于 `dwf_event.time_created`
 * （事件日志里一切「多久以前」的唯一时钟），因此差值不会跨时钟。
 */
export interface DwfRunLifeSpan {
  startedAt: number;
  lastActivityAt: number;
}

/**
 * 宿主侧的 run 内省查询面（`ListWorkflowRuns` / `GetWorkflowRun` 两个只读工具的取数底座）。
 *
 * 刻意**不加宽**引擎的 `JournalStorePort`，与 {@link SqliteDwfJournalStore.listNonTerminalRuns}
 * 逐字同一条论证：引擎只按 runId 读写自己那一行，从不枚举 run、也不做聚合计数——把这些加进
 * 领域端口，等于要求每个 journal 实现（包括引擎自带的内存实现）为一件引擎不做的事负责。
 *
 * 消费方按能力探测（`typeof journal.listRuns === "function"`）决定工具可用性，所以这个接口是
 * 宿主与 adapter 之间**唯一**的签名来源：签名在两处各写一份就会漂移，而漂移的后果是工具静默
 * 降级成「本会话没有这个能力」。
 */
export interface DwfRunIntrospectionQueries {
  countNodesByStatus(runId: string): DwfNodeStatusCounts;
  getRunRow(runId: string): DwfRunDetailRow | undefined;
  listArtifactItems(
    runId: string,
    artifactId: string,
    query: DwfArtifactItemsQuery,
  ): DwfArtifactItem[];
  listArtifactRows(runId: string): NodeRecord[];
  listRecentLogEvents(runId: string, limit: number): StoredEvent[];
  listRuns(query: DwfListRunsQuery): DwfRunListItem[];
  /**
   * 本 run 每一世的活动区间，按时序（完成卡的「时间」格）。一条 run 可以
   * 有多世（每次 resume 一世），而每一世的墙钟只有事件日志知道。
   */
  listRunLifeSpans(runId: string): DwfRunLifeSpan[];
  /**
   * 本 run 的 world-read / world-run 行，按落库先后（`order by id`），带 journal 时间戳。**不取
   * `result_json`**：这条读面是清单（op / args / 状态 / 时间），正文另有按 (siteId, ordinal)
   * 的读面——一页 256 个节点把每个 256 KB 的 stdout 一起解出来，等于把整条 journal 读进内存。
   */
  listWorldNodes(runId: string): DwfWorldNodeRow[];
}

/**
 * 按项目（cwd）枚举 run，最近更新的在前。与 {@link SqliteDwfJournalStore.listNonTerminalRuns} 同一条论证：引擎
 * 从不枚举 run，这是宿主的读面需求（`ListWorkflowRuns` 工具），所以不进领域端口。
 *
 * cwd / statuses / 排序 / limit **全部下推 SQL**：`dwf_run_cwd_idx`（0021）正是这个形状；
 * 在 JS 里取全量再筛就把索引和 limit 一起浪费掉了。cwd 是**字面**等值匹配——写入侧原样落
 * `context.workingDirectory`，读侧原样查，任何单侧的路径规范化都只会造出不匹配。
 *
 * 行是窄投影（{@link DwfRunListItem}，不带 failure / result）：列表面不展示产物，而产物
 * 可以很大。
 */
export function listRuns(db: DatabaseSync, query: DwfListRunsQuery): DwfRunListItem[] {
  // 空状态集合的语义是「不匹配任何状态」而不是「不过滤」：把它当成后者，等于让一个显式
  // 传下来的过滤器静默失效。同理 limit ≤ 0 是空页（listEvents 的 `limit -1` 全量惯用法
  // 在这条查询上不适用——枚举面永远是有界的）。
  //
  // 地板在这里，天花板**不在**：调用方合法地传「工具面上限 + 1」当截断探测行，加一个
  // `Math.min(50, …)` 会让 truncated 在恰好 limit = 50 时永久缺席。见 {@link DwfListRunsQuery.limit}。
  if (query.statuses !== undefined && query.statuses.length === 0) return [];
  if (query.limit <= 0) return [];

  // cwd 缺省即不加谓词：全局工作流的历史跨所有它跑过的项目（全局变体不按 cwd 过滤）。
  // 给了 cwd 就字面等值匹配，走 dwf_run_cwd_idx，行为逐字不变。
  const cwdFilter = query.cwd === undefined ? "" : " and cwd = ?";
  // 逻辑状态 → 物理谓词（stopped / errored 共享物理 failed，靠 failure_json 的 code 在 SQL
  // 里分清；见 dwf-journal-codecs.ts 的 encodeRunStatusPredicate）。
  const statusPredicate =
    query.statuses === undefined ? undefined : encodeRunStatusPredicate(query.statuses);
  const statusFilter = statusPredicate === undefined ? "" : ` and ${statusPredicate.sql}`;
  const nameFilter = query.name === undefined ? "" : " and name = ?";
  const rows = db
    .prepare(
      `
      select
        id, parent_session_id, cwd, name, script_text, script_hash, tool_call_id,
        args_json, resumed_from, caps_max_concurrency,
        spent_tokens, status, failure_json, time_created, time_updated
      from dwf_run
      where 1 = 1${cwdFilter}${statusFilter}${nameFilter}
      order by time_updated desc
      limit ?
      `,
    )
    .all(
      ...(query.cwd === undefined ? [] : [query.cwd]),
      ...(statusPredicate?.params ?? []),
      ...(query.name === undefined ? [] : [query.name]),
      query.limit,
    ) as unknown as DwfRunMetadataRow[];
  return rows.map(decodeRunListItem);
}

/**
 * 单个 run 的完整行 + journal 时间戳。`getRun` 回的 `RunRecord` 不带时间（引擎不关心），
 * 而详情面要报 createdAt / updatedAt，且**必须直读 journal**——内存快照在条目蒸发后给的是
 * 假的起始时间。
 */
export function getRunRow(db: DatabaseSync, runId: string): DwfRunDetailRow | undefined {
  const row = db.prepare("select * from dwf_run where id = ?").get(runId) as DwfRunRow | undefined;
  return row ? decodeRunDetailRow(row) : undefined;
}

/**
 * 本 run 的节点按 status 聚合计数。计数在 SQL 里做：详情面只要三个数字，把 dwf_node 全行
 * 读出来再数是同一个答案的昂贵版本（而节点数没有上界之外的保证）。
 *
 * 三个键恒在场（缺节点即 0）：下游要拿它们直接相加算 nodesObserved，缺键会把「还没有节点」
 * 变成 NaN。词汇表就是 `NodeRecordStatus` 的三值——`queued` 只存在于事件相位、不落库。
 */
export function countNodesByStatus(db: DatabaseSync, runId: string): DwfNodeStatusCounts {
  const rows = db
    .prepare("select status, count(*) as total from dwf_node where run_id = ? group by status")
    .all(runId) as unknown as { status: NodeRecordStatus; total: number }[];
  const counts: DwfNodeStatusCounts = { running: 0, completed: 0, failed: 0 };
  for (const row of rows) counts[row.status] = Number(row.total);
  return counts;
}

/**
 * 本 run 最后 N 条 `log` 事件，按时序（sequence 升序）返回。
 *
 * 取法是 `order by sequence desc limit ?` 再在 JS 里反转：一条长 run 的 dwf_event 是它最大的
 * 一张表，为了尾巴几条把整条 journal 读进内存正是分页存在的理由要排除的做法。类型过滤同样
 * 下推——`type` 列就是为此存的冗余（payload_json 里也有一份）。
 */
export function listRecentLogEvents(db: DatabaseSync, runId: string, limit: number): StoredEvent[] {
  if (limit <= 0) return [];
  const rows = db
    .prepare(
      "select * from dwf_event where run_id = ? and type = 'log' order by sequence desc limit ?",
    )
    .all(runId, limit) as unknown as DwfEventRow[];
  return rows.reverse().map(decodeEvent);
}

/**
 * 本 run 每一世的活动区间（`run-started` 的时刻 → 那一世最后一条事件的时刻），按时序。
 *
 * 完成卡的时长是 lineage 的**活动**时长之和，而一条 run 的每一世都要各算一段——世与世之间的空档
 * （进程已死、还没 resume）什么都没在跑，不能计入。
 *
 * 一条 SQL 做完，且**不解一个 payload**：`lead()` 把每一世的起点与下一世的起点配成区间，
 * 相关子查询在区间内取 sequence 最大的那条的时刻。两个谓词都落在 `unique(run_id, sequence)`
 * 上；`type` 列就是为这类过滤存的冗余（payload_json 里也有一份）。一条 18k 事件的 run 因此
 * 只读几行，而不是把 2.8 MB 的 payload 解进内存——后者正是这条读面刻意不用 `listEvents` 的理由。
 *
 * 子查询恒有解（区间至少含 `run-started` 自己），所以正常行不会回 null；仍防御性收窄——
 * 这是个跨存储边界的读面，而一个 null 会静默变成 NaN 毫秒。
 */
export function listRunLifeSpans(db: DatabaseSync, runId: string): DwfRunLifeSpan[] {
  const rows = db
    .prepare(
      `
      with lives as (
        select sequence, time_created,
               lead(sequence) over (order by sequence) as next_sequence
        from dwf_event
        where run_id = ? and type = 'run-started'
      )
      select
        l.time_created as started_at,
        (
          select e.time_created
          from dwf_event e
          where e.run_id = ?
            and e.sequence >= l.sequence
            and (l.next_sequence is null or e.sequence < l.next_sequence)
          order by e.sequence desc
          limit 1
        ) as last_activity_at
      from lives l
      order by l.sequence
      `,
    )
    .all(runId, runId) as unknown as { started_at: number; last_activity_at: number | null }[];
  return rows.map((row) => ({
    startedAt: Number(row.started_at),
    lastActivityAt:
      typeof row.last_activity_at === "number"
        ? Number(row.last_activity_at)
        : Number(row.started_at),
  }));
}

/**
 * 某父会话名下的 run（最近更新在前，最多 limit 条）。服务宿主侧的枚举面
 * （`DynamicWorkflowRunPort.listRunsForSession` → UI 重启后的发现查询）：`workflowRuns` 投影跨进程不存活，工具卡 join
 * 与 Resume 按钮的可用性只能从这张表还原。
 *
 * 与 {@link SqliteDwfJournalStore.listNonTerminalRuns} 同族：宿主窄查询，刻意不进引擎的 JournalStorePort。
 * 无索引：dwf_run 的行数是「每会话的 workflow 次数」量级（个位到两位数），全扫可接受；
 * 若将来量级变了，索引形状应是 (parent_session_id, time_updated)。
 *
 * 行是窄投影（{@link DwfRunSessionListItem}）：**不 select `result_json`**——那一列是脚本的
 * 顶层返回值，真正无界，而列表面从不展示产物。`failure_json` 反而必须取：会话枚举面要报
 * failureCode，且 `resumable` 的谓词就是「failed 且 code 为 Interrupted」，省掉它会让每个
 * 被打断的 run 都被静默算成不可恢复。时间戳随行返回（`RunRecord` 刻意不带时间）。
 */
export function listRunsByParentSession(
  db: DatabaseSync,
  parentSessionId: string,
  limit: number,
): DwfRunSessionListItem[] {
  const rows = db
    .prepare(
      `
      select
        id, parent_session_id, cwd, name, script_text, script_hash, tool_call_id,
        args_json, resumed_from, caps_max_concurrency,
        spent_tokens, status, failure_json, time_created, time_updated
      from dwf_run
      where parent_session_id = ?
      order by time_updated desc, id desc
      limit ?
      `,
    )
    .all(parentSessionId, Math.max(0, limit)) as unknown as DwfRunSessionRow[];
  return rows.map(decodeRunSessionListItem);
}

export function listWorldNodes(db: DatabaseSync, runId: string): DwfWorldNodeRow[] {
  const rows = db
    .prepare(
      `
      select
        run_id, site_id, ordinal, kind, actor_site_id, actor_ordinal, actor_seq, input_hash,
        status, error_json, stats_json, message_boundary, artifact_id, input_json,
        length(cast(result_json as blob)) as result_bytes,
        case when json_type(result_json) = 'array' then json_array_length(result_json) end
          as result_count,
        case when json_type(result_json) = 'object' then json_extract(result_json, '$.exitCode') end
          as exit_code,
        case when json_type(result_json) = 'object'
          then length(cast(json_extract(result_json, '$.stdout') as blob)) end as stdout_bytes,
        case when json_type(result_json) = 'object'
          then length(cast(json_extract(result_json, '$.stderr') as blob)) end as stderr_bytes,
        time_created, time_updated
      from dwf_node
      where run_id = ? and kind in ('world-read', 'world-run')
      order by id
      `,
    )
    .all(runId) as unknown as (Omit<DwfNodeRow, "id" | "result_json"> & {
    result_bytes: number | null;
    result_count: number | null;
    exit_code: number | null;
    stdout_bytes: number | null;
    stderr_bytes: number | null;
  })[];
  return rows.map((row) => {
    const record = decodeNode({ ...row, id: 0, result_json: null });
    return {
      ...record,
      timeCreated: row.time_created,
      timeUpdated: row.time_updated,
      ...(row.result_bytes === null ? {} : { resultBytes: row.result_bytes }),
      ...(row.result_count === null ? {} : { resultCount: row.result_count }),
      ...(typeof row.exit_code === "number" ? { exitCode: row.exit_code } : {}),
      ...(row.stdout_bytes === null ? {} : { stdoutBytes: row.stdout_bytes }),
      ...(row.stderr_bytes === null ? {} : { stderrBytes: row.stderr_bytes }),
    };
  });
}
