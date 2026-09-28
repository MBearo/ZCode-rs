/**
 * engine.ts 顶到 oxlint max-lines 上限（400 行），把**用户面产物**（`artifact.*`）的
 * 发布 / 声明 / 准入 / 结算 / resume 恢复拆到本文件；公开面仍从 engine.ts 导出。
 *
 * ⚠ 术语：以下一律指**用户面产物**（脚本发布给用户看的产出），不是 RunSettlement.artifact。
 * 自由函数经 {@link EngineState} 接缝读写引擎状态；WorkflowEngine 上的同名方法只是薄委托。
 */

import { canonicalJson, inputHash } from "./hash.js";
import { ARTIFACT_CAPS } from "../facade/artifact-caps.js";
import {
  isArtifactPresetOp,
  type ArtifactContentOp,
  type ArtifactPresetOp,
} from "../facade/registry.js";
import { heldResolution } from "./replay-order.js";
import {
  primaryArtifactId,
  primaryConflict,
  primaryConflictMessage,
} from "./engine-artifacts-primary.js";
import { validateArtifactSpec } from "./artifact-spec.js";
import { hashMismatch } from "./scheduler.js";
import type { EngineState } from "./engine-state.js";
import type {
  ArtifactPublishRequest,
  ArtifactRef,
  ArtifactVersionRecord,
  InstanceRef,
} from "./types.js";
import { refToString, WorkflowError } from "./types.js";

/**
 * 发布一个**内容产物**（`artifact.file` / `artifact.markdown`）。效应族：经 driver 拷字节进
 * store，成功兑现 {@link ArtifactRef}，失败**可 catch 地拒绝**——脚本的门控习语是
 * `try { await artifact.file(...) } catch { …让子代理补写… }`。
 *
 * 顺序：
 * 1. `nextOrdinal` → journal 命中？命中即短路：比 inputHash，成功记录返回它的 ArtifactRef，
 *    **失败记录按记录重新拒绝**（resume 是崩溃恢复，不是复验：一次失败的发布在恢复后仍然
 *    是那次失败，重跑它会让脚本的 catch 分支走两次不同的路）。命中一律**不重发事件**（同 report）。
 * 2. 未命中：种类归属 → id 数上限 → 版本数上限，三条都是节点级拒绝（有 promise 可拒）。
 * 3. 版本号在 dispatch **之前**算好并传给 driver（store 的 toolCallId 里要带它，每版一份字节）。
 * 4. `putNode(running)` → driver → 成功 `putNode(completed)` + `artifact-published`；
 *    失败 `putNode(failed)` + `artifact-failed` → reject。
 *
 * 两条路都**不发任何节点生命周期事件**（node-queued / dispatched / settled）：产物站点在两张
 * 图里都不是节点，一条 node-settled 落在这样的站点上会被投影归约进 `nodes[]`，在 run 面板里
 * 变成一个无从解释的未知节点。
 */
export function publishContentArtifact(
  state: EngineState,
  siteId: string,
  op: ArtifactContentOp,
  args: unknown[],
): Promise<ArtifactRef> {
  if (state.isRunSettled()) return Promise.reject(state.runError());
  const ordinal = state.nextOrdinal(siteId);
  const instance: InstanceRef = { siteId, ordinal };

  const id = args[0];
  const payload = args[1];
  // 实参形状的运行期护栏（编译期的字面量 id 诊断与 facade 类型是正门）：走到这里的非字符串
  // 只可能是有东西绕过了编译。用 DriverError 而不是某个 Artifact* 码——让那些码只表示它们
  // 各自的那件事（同 probeReportItem 对「item 不是 JSON」的取码论证）。
  //
  // 这两条**不落 journal 行、不发事件**，与下面所有失败路径不同。理由是它们是**确定性**的：
  // 结论只由 args 决定，没有 driver 往返，resume 重跑同一个调用必然得到同一个拒绝，所以
  // 不需要一行记录把它钉住（「settled failure 落库」那条要求针对的是 driver 执行过的、
  // 下次未必再失败的那种）。何况这里根本没有 id 可写进 `artifact_id` 或事件——编一个出来
  // 只会让读者以为真有那么一个产物。
  if (typeof id !== "string" || id === "") {
    return Promise.reject(
      new WorkflowError(
        "DriverError",
        `artifact.${op}: id must be a non-empty string (at ${refToString(instance)}).`,
      ),
    );
  }
  if (typeof payload !== "string") {
    return Promise.reject(
      new WorkflowError(
        "DriverError",
        op === "file"
          ? `artifact.file: path must be a string (at ${refToString(instance)}).`
          : `artifact.markdown: content must be a string (at ${refToString(instance)}).`,
      ),
    );
  }
  // `opts.primary` 由**引擎**读：driver 对不认识的
  // opts 键一律放行，而「全 run 至多一个 primary」是 run 级事实，只有引擎知道。非布尔值与上面
  // 两条同属确定性护栏——不落行、不发事件。
  const primaryOption = (args[2] as { primary?: unknown } | undefined)?.primary;
  if (primaryOption !== undefined && typeof primaryOption !== "boolean") {
    return Promise.reject(
      new WorkflowError(
        "DriverError",
        `artifact.${op}: opts.primary must be a boolean (at ${refToString(instance)}).`,
      ),
    );
  }
  const hash = inputHash({ args, id, op });

  const recorded = state.journal.getNode(state.runId, siteId, ordinal);
  if (recorded !== undefined) {
    if (recorded.inputHash !== hash) {
      const err = hashMismatch(instance, recorded.inputHash, hash);
      state.failRun(err);
      return Promise.reject(err);
    }
    // 与 ask / world 命中同规：释放点过 replay 次序闸。发布是效应、脚本要 await 它，所以它也是一个能决定
    // 后续续体次序的释放点；预置声明是同步 void，从不走这条路，也就从不占闸。
    if (recorded.status === "completed") {
      const record = recorded.result as ArtifactVersionRecord;
      return heldResolution(state.holdForReplay, instance, () => ({
        id: record.id,
        version: record.version,
      }));
    }
    if (recorded.status === "failed") {
      return heldResolution(state.holdForReplay, instance, () => {
        throw WorkflowError.fromJSON(recorded.error!);
      });
    }
    // status === "running"：崩溃于执行中，落到下面重新 live 执行（字节的拷贝是幂等的，
    // 而版本号从**已 completed 的行数**派生，所以重跑不会跳号）。
  }

  // 粘着：这个 id 已经是 primary，则这一版也是，不管本次有没有再写 `primary`。
  const idState = state.artifacts.get(id);
  const primary = primaryOption === true || idState?.primary === true;
  const admission = admitArtifact(state, id, op, primary);
  if (admission !== undefined) {
    return Promise.reject(settleArtifactFailure(state, instance, hash, { id, op }, admission));
  }

  const version = (idState?.versions ?? 0) + 1;
  const publish = state.driver.executeArtifactPublish;
  if (publish === undefined) {
    // 装配没有 driver 侧的发布能力（纯 replay / 没接 store 的 fake）。**大声的一条命名失败**，
    // 不是静默降级成「发布了一个空产物」——脚本看得见、节点以 failed 落库。
    return Promise.reject(
      settleArtifactFailure(
        state,
        instance,
        hash,
        { id, op },
        new WorkflowError(
          "ArtifactStoreUnavailable",
          `Cannot publish "${id}": this host has no artifact store (at ${refToString(instance)}).`,
        ),
      ),
    );
  }

  state.journal.putNode({
    runId: state.runId,
    siteId,
    ordinal,
    kind: "artifact",
    inputHash: hash,
    status: "running",
    artifactId: id,
  });

  const request: ArtifactPublishRequest = {
    runId: state.runId,
    siteId,
    ordinal,
    op,
    id,
    version,
    ...(op === "file" ? { path: payload } : { content: payload }),
    ...(args[2] === undefined ? {} : { opts: args[2] }),
  };
  return publish.call(state.driver, request).then(
    (record) => settleArtifactPublish(state, instance, hash, { id, op, version, primary }, record),
    (cause: unknown) => {
      const err =
        cause instanceof WorkflowError
          ? cause
          : new WorkflowError("DriverError", `Artifact publish failed: ${op} "${id}".`, {
              cause,
            });
      throw settleArtifactFailure(state, instance, hash, { id, op }, err);
    },
  );
}

/**
 * 声明一个**预置产物**（`artifact.chart` / `table` / `metrics` / `board`）。声明族：同步、
 * 无返回值、**不经 driver**——一个声明没有可等的东西。
 *
 * 顺序：`nextOrdinal` → 命中即返回 → spec 形状校验
 * → 同 id 既有声明？相同 canonical ⇒ 幂等 no-op（不落新行、不发事件）；不同 ⇒ failRun
 * → 上限 → `putNode(completed)` + `artifact-published`。
 *
 * 三种失败（spec 非法 / 同 id 异 spec / 超上限）全是 failRun 而不是拒绝：void 返回没有拒绝
 * 通道，与 `report` 的两个上限同一条论证。
 */
export function declarePresetArtifact(
  state: EngineState,
  siteId: string,
  op: ArtifactPresetOp,
  args: unknown[],
): void {
  if (state.isRunSettled()) return; // 与 report / log 同
  const ordinal = state.nextOrdinal(siteId);
  const instance: InstanceRef = { siteId, ordinal };

  const id = args[0];
  if (typeof id !== "string" || id === "") {
    state.failRun(
      new WorkflowError(
        "DriverError",
        `artifact.${op}: id must be a non-empty string (at ${refToString(instance)}).`,
      ),
    );
    return;
  }
  const spec = args[1];
  const hash = inputHash({ args, id, op });

  const recorded = state.journal.getNode(state.runId, siteId, ordinal);
  if (recorded !== undefined) {
    if (recorded.inputHash !== hash) {
      state.failRun(hashMismatch(instance, recorded.inputHash, hash));
      return;
    }
    return; // replay 去重：静默跳过（同 report）
  }

  const problem = validateArtifactSpec(op, spec);
  if (problem !== undefined) {
    state.failRun(
      new WorkflowError(
        "ArtifactSpecInvalid",
        `Artifact "${id}" has an invalid spec (at ${refToString(instance)}): ${problem}`,
      ),
    );
    return;
  }

  const canonical = canonicalJson(spec);
  const idState = state.artifacts.get(id);
  if (idState !== undefined) {
    if (idState.kind !== op) {
      state.failRun(
        new WorkflowError(
          "ArtifactKindMismatch",
          `Artifact id "${id}" is already a ${idState.kind} and cannot be declared as a ${op} ` +
            `(at ${refToString(instance)}). An id belongs to one artifact kind for the whole run; ` +
            `use a different id.`,
        ),
      );
      return;
    }
    if (idState.spec === canonical) return; // 幂等 no-op：不落新行、不发事件
    state.failRun(
      new WorkflowError(
        "ArtifactRedeclared",
        `Artifact "${id}" was already declared with a different spec (at ${refToString(instance)}). ` +
          `The spec for one id must stay identical (an identical re-declaration is a no-op); ` +
          `declare it once at the top of the script.`,
      ),
    );
    return;
  }
  // `primary` 进 canonical spec，所以同 id 改旗子是 ArtifactRedeclared（上面已经拒了）；这里只剩
  // **新** id 想当 primary 而别的 id 已经是——预置族没有拒绝通道，failRun。
  const wantsPrimary = (spec as { primary?: unknown }).primary === true;
  const holder = wantsPrimary ? primaryArtifactId(state) : undefined;
  if (holder !== undefined && holder !== id) {
    state.failRun(
      new WorkflowError(
        "ArtifactPrimaryConflict",
        primaryConflictMessage(id, holder, `declare "${id}" without primary`, instance),
      ),
    );
    return;
  }
  if (state.artifacts.size >= ARTIFACT_CAPS.maxArtifactsPerRun) {
    state.failRun(
      new WorkflowError(
        "ArtifactCapExceeded",
        `Cannot declare "${id}": this run already has ${ARTIFACT_CAPS.maxArtifactsPerRun} artifact ` +
          `ids, the maximum. Reuse an existing id or publish fewer artifacts.`,
      ),
    );
    return;
  }

  // 声明是**一次写**（同 report）：准入与结算之间没有 driver 调用，没有什么能在中间失败。
  const record = presetRecord(id, op, spec);
  state.artifacts.set(id, {
    kind: op,
    spec: canonical,
    versions: 1,
    ...(wantsPrimary ? { primary: true as const } : {}),
  });
  state.journal.putNode({
    runId: state.runId,
    siteId,
    ordinal,
    kind: "artifact",
    inputHash: hash,
    status: "completed",
    result: record,
    artifactId: id,
  });
  state.record({ type: "artifact-published", instance, artifact: record });
}

/**
 * 内容产物的准入：种类归属 → id 数上限 → 版本数上限。返回拒绝理由，`undefined` 即放行。
 *
 * 三条全是**节点级**拒绝（内容成员有 promise 可拒），与预置族里同名的三条形成对照——
 * 那一族只能 failRun。同一个事实、两个通道，差别在返回类型而不是严重程度。
 */
function admitArtifact(
  state: EngineState,
  id: string,
  op: ArtifactContentOp,
  primary: boolean,
): WorkflowError | undefined {
  const idState = state.artifacts.get(id);
  if (idState !== undefined && idState.kind !== op) {
    return new WorkflowError(
      "ArtifactKindMismatch",
      `Artifact id "${id}" is already a ${idState.kind} and cannot be published as a ${op}. An id ` +
        `belongs to one artifact kind for the whole run (publishing the same id again creates a ` +
        `new version, and a version cannot change what it is); use a different id.`,
    );
  }
  // primary 冲突排在上限之前：一个想当交付物的 id 被拒，理由该是「已经有交付物了」，不是「满了」。
  const holder = primaryConflict(state, id, primary);
  if (holder !== undefined) {
    return new WorkflowError(
      "ArtifactPrimaryConflict",
      primaryConflictMessage(id, holder, `publish "${id}" without primary`, undefined),
    );
  }
  if (idState === undefined && state.artifacts.size >= ARTIFACT_CAPS.maxArtifactsPerRun) {
    return new WorkflowError(
      "ArtifactCapExceeded",
      `Cannot publish "${id}": this run already has ${ARTIFACT_CAPS.maxArtifactsPerRun} artifact ` +
        `ids, the maximum. Publish a new version of an existing id instead.`,
    );
  }
  if (idState !== undefined && idState.versions >= ARTIFACT_CAPS.maxVersionsPerArtifact) {
    return new WorkflowError(
      "ArtifactVersionCapExceeded",
      `Artifact "${id}" already has ${idState.versions} versions, the maximum of ` +
        `${ARTIFACT_CAPS.maxVersionsPerArtifact}. Publish the final content in fewer versions.`,
    );
  }
  return undefined;
}

/**
 * 内容产物发布成功：落 completed 行 + 记账 + 发 `artifact-published`，返回脚本要的 ref。
 *
 * 身份（id / 种类 / 版本号）一律取**引擎自己算的那份**（`issued`），而不是 driver 回来的
 * 记录里的同名字段：版本号是引擎按 journal 派生并下发的，让它绕 driver 走一圈再读回来，
 * 只会多出一个可以不一致的地方。driver 的记录负责它真正拥有的东西——uri / bytes /
 * contentType / sourcePath——那些整条原样落 journal。
 */
function settleArtifactPublish(
  state: EngineState,
  instance: InstanceRef,
  hash: string,
  issued: { id: string; op: ArtifactContentOp; version: number; primary: boolean },
  record: ArtifactVersionRecord,
): ArtifactRef {
  if (state.isRunSettled()) throw state.runError();
  // 准入与结算之间隔着一次 driver 往返：两个并发的 primary 发布都能过准入。结算时再查一次，
  // 输的那个走失败路径——failed 行不认领旗子，所以 journal 里永远不会有两个 completed 的 primary。
  const holder = primaryConflict(state, issued.id, issued.primary);
  if (holder !== undefined) {
    throw settleArtifactFailure(
      state,
      instance,
      hash,
      issued,
      new WorkflowError(
        "ArtifactPrimaryConflict",
        primaryConflictMessage(
          issued.id,
          holder,
          `publish "${issued.id}" without primary`,
          instance,
        ),
      ),
    );
  }
  // 旗子由引擎盖章：driver 的记录只负责它真正拥有的东西（uri / bytes / contentType / sourcePath）。
  const stored: ArtifactVersionRecord = issued.primary ? { ...record, primary: true } : record;
  state.journal.putNode({
    runId: state.runId,
    siteId: instance.siteId,
    ordinal: instance.ordinal,
    kind: "artifact",
    inputHash: hash,
    status: "completed",
    result: stored,
    artifactId: issued.id,
  });
  state.artifacts.set(issued.id, {
    kind: issued.op,
    versions: issued.version,
    ...(issued.primary ? { primary: true as const } : {}),
  });
  state.record({ type: "artifact-published", instance, artifact: stored });
  return { id: issued.id, version: issued.version };
}

/**
 * 内容产物发布失败：落 failed 行 + 发 `artifact-failed`，返回要抛出的错误。
 *
 * 落库是重放健全性的既有要求（settled failure 必须在 journal 里），否则 resume 会把一次
 * 已经被脚本 catch 过的失败重跑一遍——那正是「resume 是崩溃恢复不是复验」要防的。
 *
 * node-settled 的读者
 * 会把它归约进 `nodes[]` 并按 site id 去图里找对应节点，而产物站点不进任何图——那会在 run
 * 面板上变成一个无从解释的未知节点。`artifact-failed` 带着渲染一张失败卡需要的东西
 * （id / 种类 / 结构化错误），不冒充一步工作。成功那侧同理只发 `artifact-published`。
 */
function settleArtifactFailure(
  state: EngineState,
  instance: InstanceRef,
  hash: string,
  issued: { id: string; op: ArtifactContentOp },
  error: WorkflowError,
): WorkflowError {
  const json = error.toJSON();
  state.journal.putNode({
    runId: state.runId,
    siteId: instance.siteId,
    ordinal: instance.ordinal,
    kind: "artifact",
    inputHash: hash,
    status: "failed",
    error: json,
    artifactId: issued.id,
  });
  state.record({
    type: "artifact-failed",
    instance,
    id: issued.id,
    op: issued.op,
    error: json,
  });
  return error;
}

/**
 * resume 恢复：把 journal 里一行**已完成**的产物记录并进内存状态。
 *
 * **只有 completed 行才认领 id**：失败的发布不占 id、不占种类、不占版本。这不只是图简洁——
 * 失败行的 `result` 是空的，种类无从恢复，若让 live 那一侧认领而 resume 那一侧认领不了，
 * 同一个脚本在崩溃前后就会得到两套上限账。种类冲突另有编译期诊断把守，运行期这条
 * （`ArtifactKindMismatch`）管的是真正发布成功过的那些 id。
 */
export function rememberArtifactRow(
  state: EngineState,
  id: string | undefined,
  result: unknown,
): void {
  if (id === undefined || id === "") return;
  const record = (result ?? undefined) as ArtifactVersionRecord | undefined;
  if (record?.kind === undefined) return;
  const existing = state.artifacts.get(id);
  const spec = record.spec === undefined ? existing?.spec : canonicalJson(record.spec);
  const primary = record.primary === true || existing?.primary === true;
  state.artifacts.set(id, {
    kind: record.kind,
    ...(spec === undefined ? {} : { spec }),
    versions: (existing?.versions ?? 0) + 1,
    ...(primary ? { primary: true as const } : {}),
  });
}

/** 该 id 是否已被声明为**预置**产物（`report` 标签的合法目标）。 */
export function isDeclaredPreset(state: EngineState, id: string): boolean {
  const idState = state.artifacts.get(id);
  return idState !== undefined && isArtifactPresetOp(idState.kind);
}

/** 已声明的预置 id（字典序）——只用于把 `ArtifactUndeclared` 的消息写得能照做。 */
export function declaredPresetIds(state: EngineState): string[] {
  return [...state.artifacts.entries()]
    .filter(([, idState]) => isArtifactPresetOp(idState.kind))
    .map(([id]) => id)
    .sort();
}

/**
 * 预置声明的落库记录。`title` / `description` 从 spec 里**提上来**：卡片的读者（run 侧板、
 * 通知、中枢）只拿到这条记录，不该为了显示一个标题去解 spec；spec 仍原样保留，渲染器要
 * 完整形状。版本恒为 1——声明没有版本，它只有一次。
 */
function presetRecord(id: string, op: ArtifactPresetOp, spec: unknown): ArtifactVersionRecord {
  const options = (spec ?? {}) as { title?: unknown; description?: unknown; primary?: unknown };
  return {
    id,
    kind: op,
    version: 1,
    ...(typeof options.title === "string" ? { title: options.title } : {}),
    ...(typeof options.description === "string" ? { description: options.description } : {}),
    spec,
    ...(options.primary === true ? { primary: true as const } : {}),
  };
}
