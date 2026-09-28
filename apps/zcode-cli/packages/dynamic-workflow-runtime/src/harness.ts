/**
 * 父进程 harness（Boundary A 的宿主侧 + 沙箱进程编排）。
 *
 * 职责：把一份 workflow 脚本（或已 lowered 的函数体）在受控子进程里跑起来，用 NDJSON 桥接
 * 子进程的 `__host.*` 调用到一个 {@link WorkflowEngine} 实例，最终返回引擎的 {@link RunSettlement}。
 * 本包**只**依赖 `@zcode/dynamic-workflow` 与 node 内建——证明整条管线 app-free 可跑，
 * 绝不 import `@zcode/core`/`@zcode/contracts`/`@zcode/bootstrap`/`@zcode/adapters`。
 *
 * 时序（happy path）：
 *
 *   parent                         child(vm)
 *     │  write <cwd>/.zcode/workflow-runs/<runId>.mjs（payload: lowered+args 内嵌）
 *     │  spawn(node <entry>)
 *     │──────────────────────────▶│  build __host in context
 *     │◀── create-actor(local#1) ──│  createActor 同步返回 local#1
 *     │  map local#1 → ActorId     │
 *     │◀── request ask(local#1) ───│  await __host.ask(...)
 *     │  engine.ask → driver.startAsk … settle
 *     │── response(value) ────────▶│  resolve
 *     │◀────── complete(ok,value) ──│  脚本 return
 *     │  engine.complete(value) → settled=completed
 *
 * 失败/取消：run 的裁决归引擎所有。终结失败（脚本抛错 error-complete、子进程崩溃/非零退出、
 * 墙钟超时、子进程行 JSON 解析失败）都调 `engine.fail(error)`（结算 failed + journal failure_json）；
 * abort 信号是唯一的"真取消"，调 `engine.stop(initiator)`（结算 stopped；`signal.reason` 为
 * `"model"` 即主代理 TaskStop，`"interrupted"` 即宿主 App 关闭时停下自己拥有的 run，否则算用户）。引擎自身的 run 级失败
 * （reportCap/inputHash/unknownActor）同样经 engine.settled 冒出。harness 侧的 first-wins
 * finalize 只管子进程清理（清 timer、关 stdin、kill child），不自造结算。
 */

import { spawn, type ChildProcess } from "node:child_process";
import { createInterface, type Interface } from "node:readline";
import {
  lowerWorkflowScript,
  WorkflowEngine,
  WorkflowError,
  type ActorId,
  type AskSpec,
  type Caps,
  type ImportedRunCache,
  type RunSettlement,
  type ValidateFn,
  type WorkflowDriver,
  type WorkflowReportSink,
} from "@zcode/dynamic-workflow";
import { type ChildMessage, type ChildPayload, type ResponseMessage } from "./protocol.js";
import { renderChildEntry } from "./child-source.js";
import { writeChildEntryFile, type HarnessWarning } from "./child-entry-file.js";

/** driver 工厂：harness 先建 sink（引擎的向上回报面），交给工厂造 driver，再以该 driver 建引擎。 */
export type DriverFactory = (sink: WorkflowReportSink) => WorkflowDriver;

/**
 * run 的**活体控制面**：harness 在引擎构造好之后把这一世的引擎交给它，于是持有句柄的那一侧
 * （run service）能够到活着的引擎。今日只有一条命令——就地改本 run 的并发上界。
 *
 * 收窄成 `Pick<…, "setMaxConcurrency">` 而不是整个引擎：控制面是一条**命令**通道，不是让
 * 宿主绕过 harness 去驱动 run 生命周期的后门（结算仍然只经 complete/stop/fail 那三条路）。
 *
 * 与 `signal` 同规：harness 只做接线，不解释、不校验、不兜底；命令的存活判定与 no-op 语义
 * 全在引擎里（`setMaxConcurrency` 返回 false 即这次什么也没发生）。
 */
export interface RunControlBinding {
  bind(engine: Pick<WorkflowEngine, "setMaxConcurrency">): void;
}

/**
 * {@link runWorkflowScript} 的入参。执行体来自 `scriptText` 或 `lowered`（后者优先）；
 * 两者同时给出正是「编译一次」的形态——调用方自己编译得到 `lowered`，同时把作者写的
 * `scriptText` 交下来落库。
 *
 * **harness 绝不校验这一对是否自洽**：它无法校验（判断 `lowered` 是否真由 `scriptText`
 * 降级而来，等于把编译再跑一遍，而那正是「编译一次」要省掉的那次）。递进来一份对不上的
 * 组合，落库的 `script_text` 就会与实际执行的代码不符，resume 的比对基准随之失真——
 * 这份自洽性归调用方所有。
 */
export interface RunWorkflowOptions {
  /**
   * 作者写的 workflow 脚本源码，两个用途：
   * 1. `lowered` 缺省时由 harness lower 它得到执行体；
   * 2. **始终**作为 run 元数据落 `dwf_run.script_text`（resume 的比对基准是作者原文，
   *    不是 lowered 函数体）。
   */
  scriptText?: string;
  /**
   * 已 lowered 的 async 函数体（自由标识符仅 `__host`）。绕过编译器，直接喂沙箱——
   * 安全测试用它投喂手写 lowered 体（vm 契约本身才是被测对象），编译一次的路径用它
   * 交出自己那一次编译的产物。给出时优先于 `scriptText` 作为执行体。
   */
  lowered?: string;
  runId?: string;
  /** driver 工厂（见 {@link DriverFactory}）。driver 自带 journal 与 emit。 */
  makeDriver: DriverFactory;
  caps: Caps;
  /** 每 ask 站点的静态规格。**必须覆盖脚本里的每个 ask 站点**——引擎把缺席当接线错误硬失败。 */
  askSpecs: ReadonlyMap<string, AskSpec>;
  /** 注入的 schema 校验器（引擎不 import schema 实现）。 */
  validate: ValidateFn;
  /** 外部取消信号：中止在飞 ask 并 kill 子进程，run 结算 cancelled。 */
  signal?: AbortSignal;
  /**
   * 活体控制面的绑定口（见 {@link RunControlBinding}）。与 `signal` 同一条缝递进来：
   * 那个是「停下这个 run」的通道，这个是「改这个 run 的一项设置」的通道。缺席即本次启动
   * 没有控制面（如 snippet 执行）。
   */
  control?: RunControlBinding;
  /** 墙钟超时（ms）：到点 kill 子进程，run 结算 failed。缺省不限。 */
  timeoutMs?: number;
  /** 子进程堆上限（MB），映射为 `--max-old-space-size`。缺省 256。 */
  maxOldSpaceSizeMb?: number;
  /**
   * 子进程 spawn 策略的替代形态：给出时 spawn `process.execPath [...argsPrefix, <entry path>]`，
   * **不带任何 Node CLI 旗标**。
   *
   * 存在理由：SEA 单文件二进制不解释 Node CLI 旗标，缺省路径的 `--max-old-space-size` 会作为
   * 普通 token 落进 CLI 的严格 parseArgs 而必然报错退出（每个 workflow run 在 SEA 下都失败）。SEA 下
   * 由 bootstrap 传入隐藏子命令名作为 argsPrefix，子进程自 re-exec 本二进制并在 parseArgs 之前
   * `import()` 入口文件、调它的 `start`。
   *
   * 代价：堆上限只能由入口文件自己 `v8.setFlagsFromString` best-effort。
   */
  childSpawn?: { argsPrefix: readonly string[] };
  /**
   * 非致命状况的上报口（今日只有一种：入口文件写不进项目 `.zcode/`，回落到了 OS 临时目录）。
   * harness 是 app-free 的，没有 logger；bootstrap 把它接到自己的 warn 日志。
   */
  onWarning?: (warning: HarnessWarning) => void;
  /** 子进程工作目录，缺省 process.cwd()；同时作为 run 元数据落 dwf_run.cwd。 */
  cwd?: string;
  /**
   * run 元数据，**原样**转交 `EngineConfig`（createRun 落 dwf_run）。全可选，harness
   * 不加工也不推断：`scriptHash` **由调用方计算**。harness 若哈希"它看到的文本"，在
   * lowered 路径上落库的就是 lowered 函数体的哈希——resume 校验会拿到一个静默错误的
   * 比对对象。脚本原文走上面的 `scriptText`。
   */
  scriptHash?: string;
  parentSessionId?: string;
  /** run 的展示名（宿主枚举面的标签来源）。与 `scriptText` 同路，harness 只转交。 */
  name?: string;
  /**
   * 本次 run 的实参（saved workflow 的声明式参数，已由调用方校验并回填默认值）。
   *
   * **两个去处**，这是它与其余元数据的不同之处：既随 `EngineConfig` 落 `dwf_run.args_json`
   * （resume 从那里读回重放），也进 spawn payload 注入沙箱成为冻结的 `args` 全局。harness
   * 不校验、不加工——声明与校验都在调用方（工具侧）。缺席即 `{}`。
   */
  args?: Record<string, unknown>;
  /** 发起 run 的 CreateWorkflow 工具调用 id（重启后 join/通知锚点）；verbatim 转交 EngineConfig。 */
  toolCallId?: string;
  /**
   * 修订续跑（amend-resume）的一对入参，与其余元数据同规：**verbatim 转交 EngineConfig**，
   * harness 不读、不加工、不推断。`resumedFrom` 是落 `dwf_run.resumed_from` 的 lineage 指针，
   * `importedCache` 是注入引擎的纯数据缓存表。
   *
   * 与 `scriptText`/`lowered` 那一对同样的分工：**两者的一致性归调用方所有**。harness 无从判断
   * 这张表是否真由那个前驱 run 构建而来（那要求它自己去读 journal，而本包连存储都不认识）。
   * 递进来一张对不上的表，得到的就是一个把别人的答案当成自己缓存的 run。构建与门都在 run
   * service（bootstrap 的 dynamic-workflow-import.ts）。
   */
  resumedFrom?: string;
  /**
   * 发起 run 那一轮的锚点；verbatim 转交 EngineConfig。
   * `subagentModel` 与锚点、阶段表同车：本 run 子代理的规范 picker 串
   * （`providerId/modelId[$reasoningLevel]`），harness 同样不读、不解析、不加工——它零 SQL 地
   * 活在 `run-launched` 事件里，解析与优先级都在宿主侧（bootstrap 的 workflow-actor-model.ts）。
   * `phaseAlongside` 与 `phaseNames` 按位置对齐（下标指向同一张表），同车同规。
   */
  launch?: {
    inputId: string;
    phaseNames?: string[];
    subagentModel?: string;
    phaseAlongside?: number[][];
  };
  importedCache?: ImportedRunCache;
  /**
   * 建 run 时的用量起点（前驱 run 的 `spentTokens`）；与其余元数据同规：**verbatim 转交
   * EngineConfig**，harness 不读、不加工。读前驱的行发生在 run service。
   */
  inheritedTokens?: number;
}

const DEFAULT_MAX_OLD_SPACE_MB = 256;
const STDERR_LIMIT = 64 * 1024;

/**
 * 跑一份 workflow 脚本至结算。返回引擎的 {@link RunSettlement}。
 * 失败一等公民，但分两类：脚本抛错是脚本之错，
 * 归一成 `{status:"errored", error}`；子进程无法启动 / 崩溃 / 墙钟超时 / 协议损坏是宿主侧故障，
 * 重跑很可能就好，归一成 `{status:"stopped", reason:"interrupted", error}`（可 resume）。
 * 两者都绝不静默吞掉。
 */
export async function runWorkflowScript(options: RunWorkflowOptions): Promise<RunSettlement> {
  const lowered = resolveLowered(options);
  const runId = options.runId ?? "run";

  // sink 晚绑定：引擎在 driver 之后构造，但 driver 需要 sink 回报——用转发代理打破环依赖。
  let engine!: WorkflowEngine;
  const sink: WorkflowReportSink = {
    askSubmitAttempted: (instance, payload) => engine.askSubmitAttempted(instance, payload),
    askTurnEnded: (instance, finalText) => engine.askTurnEnded(instance, finalText),
    askProgress: (instance, progress) => engine.askProgress(instance, progress),
    askStats: (instance, stats) => engine.askStats(instance, stats),
    askFailed: (instance, error) => engine.askFailed(instance, error),
    // 确定性模型侧错误 → 整个 run 停下，同样只是转发。
    stopRun: (error) => engine.stopRun(error),
    // 自适应并发的三个纯观察与 run 级停滞观察，同样只是转发。
    askWaiting: (instance, info) => engine.askWaiting(instance, info),
    askExecuting: (instance) => engine.askExecuting(instance),
    // 唯一会让引擎做决策的观察（关导入缓存），同样只是转发。
    askMutating: (instance) => engine.askMutating(instance),
    concurrencyChanged: (change) => engine.concurrencyChanged(change),
    runStalled: (info) => engine.runStalled(info),
  };
  const driver = options.makeDriver(sink);
  engine = new WorkflowEngine({
    runId,
    driver,
    caps: options.caps,
    askSpecs: options.askSpecs,
    validate: options.validate,
    // 元数据 verbatim 转交，缺省保持缺席（不落成 undefined 键）。cwd 与子进程实际使用的
    // 是同一个值，所以这里也记 process.cwd() 的兜底——落库的 cwd 就是 run 真正跑的目录。
    ...(options.scriptText === undefined ? {} : { scriptText: options.scriptText }),
    ...(options.name === undefined ? {} : { name: options.name }),
    ...(options.args === undefined ? {} : { args: options.args }),
    ...(options.scriptHash === undefined ? {} : { scriptHash: options.scriptHash }),
    ...(options.parentSessionId === undefined ? {} : { parentSessionId: options.parentSessionId }),
    ...(options.toolCallId === undefined ? {} : { toolCallId: options.toolCallId }),
    ...(options.resumedFrom === undefined ? {} : { resumedFrom: options.resumedFrom }),
    ...(options.launch === undefined ? {} : { launch: options.launch }),
    ...(options.importedCache === undefined ? {} : { importedCache: options.importedCache }),
    ...(options.inheritedTokens === undefined ? {} : { inheritedTokens: options.inheritedTokens }),
    cwd: options.cwd ?? process.cwd(),
  });
  // 控制面一构造好就绑：run 从第一条事件起就可被改设置，而**不必**等子进程起来——下面那条
  // spawn 失败路径也经引擎结算，句柄在那之后照样安全（命令自己的 settled 判定负责收口）。
  options.control?.bind(engine);

  const maxOldSpaceSizeMb = options.maxOldSpaceSizeMb ?? DEFAULT_MAX_OLD_SPACE_MB;
  const cwd = options.cwd ?? process.cwd();
  const payload: ChildPayload = {
    lowered,
    // 实参只在启动时过界一次（run 生命周期内是常量）。
    ...(options.args === undefined ? {} : { args: options.args }),
    // 堆上限随 payload 进入口文件，只为 argsPrefix 路径服务：那条路上没有 Node 旗标可传，
    // 入口文件只能自己 best-effort 设。缺省路径由真旗标生效，入口文件看到旗标就跳过。
    maxOldSpaceSizeMb,
  };

  // payload 不过命令行：Windows 的命令行上限 32,767 字符，整份 lowered 脚本放上去一过约 18 KB
  // 就 `spawn ENAMETOOLONG`。写成入口文件，
  // argv 只剩一条路径，长度与脚本大小无关。
  let child: ChildProcess;
  try {
    const entry = writeChildEntryFile({
      cwd,
      runId,
      source: renderChildEntry(payload, { runId }),
      ...(options.onWarning === undefined ? {} : { onWarning: options.onWarning }),
    });
    child = spawn(
      process.execPath,
      options.childSpawn === undefined
        ? [`--max-old-space-size=${maxOldSpaceSizeMb}`, entry.path]
        : [...options.childSpawn.argsPrefix, entry.path],
      {
        cwd,
        stdio: ["pipe", "pipe", "pipe"],
        // 桌面端 agent 由 Electron Helper 运行（process.execPath 指向 Helper），而 CLI
        // 启动时会把 ELECTRON_RUN_AS_NODE 从自身 env sanitize 掉。不显式带上它，子进程会按完整
        // Electron/Chromium 应用启动并卡在 GPU 初始化——永远沉默也不退出，run 卡死在 run-started。
        // 纯 Node 的 execPath 下该变量无效，无副作用（同 official-plugin-runtime.ts 的处理）。
        // 两条 spawn 策略都要带：桌面打包态同样可能走 argsPrefix 路径。
        env: { ...process.env, ELECTRON_RUN_AS_NODE: "1" },
      },
    );
  } catch (cause) {
    // Node 只把 EACCES/EAGAIN/EMFILE/ENFILE/ENOENT 转成 error
    // 事件，其余 spawn 失败（ENAMETOOLONG、E2BIG…）**同步抛**。此时引擎已构造、journal 行已以
    // running 落库；让异常直接冒出去会绕过引擎结算——注册表记成终态，journal 行却永远
    // running，随后的 resume_from 被「has not settled yet」拒绝。入口文件写不下（两个目录都
    // 失败）同理。一律经引擎结算 stopped(interrupted)：宿主侧故障，可 resume。
    const message = cause instanceof Error ? cause.message : String(cause);
    engine.stop(
      "interrupted",
      new WorkflowError("Interrupted", `Could not start the workflow sandbox process: ${message}`, {
        cause,
      }),
    );
    return engine.settled;
  }

  return bridge({ child, engine, runId, signal: options.signal, timeoutMs: options.timeoutMs });
}

/** 解析 lowered 体：优先 `lowered`，否则 lower `scriptText`（脏脚本抛错，不启动子进程）。 */
function resolveLowered(options: RunWorkflowOptions): string {
  if (options.lowered !== undefined) return options.lowered;
  if (options.scriptText === undefined) {
    throw new Error("runWorkflowScript: pass either scriptText or lowered");
  }
  const result = lowerWorkflowScript(options.scriptText);
  if (!result.ok || result.lowered === undefined) {
    const detail = result.diagnostics.map((d) => `${d.line}:${d.column} ${d.message}`).join("; ");
    throw new Error(
      `runWorkflowScript: the script failed compilation/analysis and is not run degraded: ${detail}`,
    );
  }
  return result.lowered.code;
}

interface BridgeDeps {
  child: ChildProcess;
  engine: WorkflowEngine;
  /** 只用于 abort 归一里的错误文本（"interrupted" 那一支要指名是哪个 run 被关掉的）。 */
  runId: string;
  signal?: AbortSignal;
  timeoutMs?: number;
}

/**
 * NDJSON 桥接 + 生命周期收敛。single-writer finalize：无论从引擎结算、子进程退出、超时还是
 * abort 触达，第一个到达者胜出，随后 kill 子进程并兑现结果。
 */
function bridge(deps: BridgeDeps): Promise<RunSettlement> {
  const { child, engine, runId, signal, timeoutMs } = deps;

  // local#N（child-local 句柄）→ engine ActorId 的映射；stdio FIFO + 同步 create-actor 处理保证
  // 任何引用某句柄的 ask 到达前，该映射已就绪。
  const actorMap = new Map<string, ActorId>();
  let stderr = "";
  let finalized = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let reader: Interface | undefined;

  return new Promise<RunSettlement>((resolve) => {
    const finalize = (settlement: RunSettlement): void => {
      if (finalized) return;
      finalized = true;
      if (timer !== undefined) clearTimeout(timer);
      if (signal !== undefined) signal.removeEventListener("abort", onAbort);
      reader?.close();
      if (child.exitCode === null && child.signalCode === null) child.kill();
      resolve(settlement);
    };

    // 终结失败（脚本抛错 / 子进程崩溃 / 超时 / 协议损坏）都是 run 失败：交给引擎的公有 fail()，
    // 由它 first-wins 结算、driver 侧取消在飞 ask、journal 记 failed + failure_json——run 的裁决
    // 归引擎所有，journal 与调用方看到的结果不分叉。子进程清理由 finalize（经 engine.settled）负责。
    const failRun = (error: WorkflowError): void => {
      if (finalized) return;
      engine.fail(error);
    };
    // 宿主侧故障（沙箱崩溃 / 超时 / 协议损坏）：stopped(interrupted)，可 resume。
    const interruptRun = (message: string, cause?: unknown): void => {
      if (finalized) return;
      engine.stop(
        "interrupted",
        new WorkflowError("Interrupted", message, cause === undefined ? undefined : { cause }),
      );
    };

    // abort 是唯一的"真取消"：走 engine.stop(initiator)，结算 stopped。initiator 经
    // `AbortController.abort(reason)` 过来：字面 "model"、amend 路径的
    // `{ superseded: newRunId }`（后继 id 随原因同一笔落库）、字面 "interrupted"（宿主 App
    // 关闭，见下），其余一律 "user"。
    function onAbort(): void {
      if (finalized) return;
      const reason: unknown = signal?.reason;
      const supersededBy = readSupersededBy(reason);
      if (supersededBy !== undefined) {
        engine.stop("superseded", undefined, supersededBy);
        return;
      }
      // 宿主主动停下自己拥有的 run（run service 的 close()）。它不是用户取消：带 Interrupted 失败编码落库，与超时 /
      // 沙箱崩溃同一族——stopped(interrupted) 可 resume，而 stopped(user) 在 UI 上读作
      // 「用户按了停止」。错误文本指名 run 与成因，下一次激活时详情页据它解释这一行。
      if (reason === "interrupted") {
        engine.stop(
          "interrupted",
          new WorkflowError(
            "Interrupted",
            `dynamic workflow run ${runId} was interrupted: the owning session closed before the run settled`,
          ),
        );
        return;
      }
      engine.stop(reason === "model" ? "model" : "user");
    }

    // 一切结算（complete / fail / stop / 引擎内部 cap 失败）都经 engine.settled 冒出到 finalize。
    void engine.settled.then(finalize);

    if (signal !== undefined) {
      if (signal.aborted) {
        onAbort();
        return;
      }
      signal.addEventListener("abort", onAbort, { once: true });
    }

    if (timeoutMs !== undefined) {
      timer = setTimeout(() => {
        interruptRun(
          `The workflow sandbox process exceeded its wall-clock timeout of ${timeoutMs}ms`,
        );
      }, timeoutMs);
      timer.unref?.();
    }

    child.stderr?.on("data", (chunk: Buffer) => {
      stderr = trimTail(stderr + chunk.toString("utf8"), STDERR_LIMIT);
    });
    // 子进程退出后仍可能有 response 待写：给 stdin 挂 error 监听，避免 EPIPE 冒成未捕获异常。
    child.stdin?.on("error", () => undefined);

    reader = createInterface({ input: child.stdout! });
    reader.on("line", (line) => {
      if (line.trim().length === 0) return;
      let message: ChildMessage;
      try {
        message = JSON.parse(line) as ChildMessage;
      } catch (cause) {
        // 子进程行 JSON 损坏：暴露而非吞掉。
        interruptRun(
          `The workflow sandbox process emitted a line that is not valid NDJSON: ${line}`,
          cause,
        );
        return;
      }
      handleChildMessage(message, { engine, actorMap, child, failRun });
    });

    child.on("error", (error) => {
      interruptRun(`Could not start the workflow sandbox process: ${error.message}`, error);
    });

    child.on("close", (code, sig) => {
      if (finalized) return;
      // 子进程退出但引擎未结算：崩溃/被杀而无 complete。以 stderr 归因。
      const reason =
        stderr.trim().length > 0
          ? stderr.trim()
          : `The workflow sandbox process exited (code=${code}, signal=${sig}) before completing`;
      interruptRun(reason);
    });
  });
}

interface MessageDeps {
  engine: WorkflowEngine;
  actorMap: Map<string, ActorId>;
  child: ChildProcess;
  failRun: (error: WorkflowError) => void;
}

/** 分发一条 child→parent 消息。ask/world-read 异步桥接到引擎并回 response（搭载最新预算）。 */
function handleChildMessage(message: ChildMessage, deps: MessageDeps): void {
  const { engine, actorMap, child, failRun } = deps;

  switch (message.kind) {
    case "create-actor": {
      // 同步处理：在任何引用该句柄的 ask 之前把映射建好（stdio FIFO 前提）。createActor 是纯同步的，
      // 若 run 已结算会同步抛错——此时没有 response 通道，捕获后归为 run 失败（多为无害的收尾竞态）。
      try {
        const actorId = engine.createActor(
          message.siteId,
          message.name,
          message.persona as string | undefined,
        );
        actorMap.set(message.localId, actorId);
      } catch (cause) {
        failRun(
          cause instanceof WorkflowError
            ? cause
            : new WorkflowError("DriverError", `createActor failed at site ${message.siteId}`, {
                cause,
              }),
        );
      }
      return;
    }
    case "event":
      // 同步分派，与 log 一致：事件通道的 FIFO 顺序对 report 与 declare-artifact 都是承重的
      // （父进程 journal 的就是到达的东西，而一条打了标签的 report 必须晚于它的声明落库），
      // 异步化会让到达顺序与 journal 顺序脱钩。
      if (message.type === "report") {
        engine.report(message.siteId, message.item, message.artifactId);
      } else if (message.type === "declare-artifact") {
        engine.declareArtifact(message.siteId, message.op, message.args);
      } else if (message.type === "phase-entered") {
        engine.enterPhase(message.name);
      } else {
        engine.log(message.message);
      }
      return;
    case "complete":
      if (message.ok) {
        engine.complete(message.value);
      } else {
        // 脚本抛错：run 失败（错误明细来自沙箱）。
        const err = message.error;
        failRun(
          new WorkflowError("DriverError", err?.message ?? "The workflow script threw an error", {
            cause: err?.stack ?? err?.name,
          }),
        );
      }
      return;
    case "request":
      handleRequest(message, { engine, actorMap, child, failRun });
      return;
    default: {
      const _exhaustive: never = message;
      void _exhaustive;
    }
  }
}

/** 桥接一次需应答的 host 调用（ask / world-read）到引擎，settle 后回 response。 */
function handleRequest(
  message: Extract<ChildMessage, { kind: "request" }>,
  deps: MessageDeps,
): void {
  const { engine, actorMap, child } = deps;

  const respond = (ok: boolean, value: unknown, error?: WorkflowError): void => {
    const response: ResponseMessage = {
      kind: "response",
      id: message.id,
      ok,
      ...(ok ? { value } : { error: toWireError(error) }),
    };
    // 子进程可能已退出（取消/失败收尾）：仅在可写时写，EPIPE 等 I/O 竞态吞在此边界（run 已在结算）。
    const stdin = child.stdin;
    if (stdin === null || !stdin.writable) return;
    stdin.write(`${JSON.stringify(response)}\n`, () => undefined);
  };

  let promise: Promise<unknown>;
  if (message.type === "ask") {
    const actorId = actorMap.get(message.actor ?? "");
    if (actorId === undefined) {
      // 映射缺失（理应不会发生：FIFO 保证）——归一成 UnknownActor 结构化拒绝，不静默。
      respond(
        false,
        undefined,
        new WorkflowError("UnknownActor", `Unknown subagent handle: ${message.actor}`),
      );
      return;
    }
    promise = engine.ask(message.siteId, actorId, message.instructions ?? "");
  } else if (message.type === "publish-artifact") {
    const op = message.artifactOp;
    if (op === undefined) {
      // 缺 op 是接线错误（lowering 恒填它）。**不编一个默认值**：一个被当成 file 处理的
      // markdown 发布，错误会出现在离故障点很远的地方。归一成结构化拒绝，脚本看得见。
      respond(
        false,
        undefined,
        new WorkflowError(
          "DriverError",
          `publish-artifact request is missing artifactOp (site ${message.siteId})`,
        ),
      );
      return;
    }
    promise = engine.publishArtifact(message.siteId, op, message.args ?? []);
  } else {
    // op/args 原样转交引擎：本层不看 op、不校验元数（那是 driver 的职责）。缺失 args 归一为空数组，
    // 让 driver 的实参校验大声拒绝，而不是在这里悄悄编一个默认值。
    promise = engine.worldRead(message.siteId, message.op ?? "read", message.args ?? []);
  }

  promise.then(
    (value) => respond(true, value),
    (cause: unknown) => {
      const error =
        cause instanceof WorkflowError
          ? cause
          : new WorkflowError(
              "DriverError",
              cause instanceof Error ? cause.message : String(cause),
              { cause },
            );
      respond(false, undefined, error);
    },
  );
}

/** WorkflowError → 线形态（保留 code/violations/finalText，供沙箱脚本 try/catch 结构化处理）。 */
function toWireError(error: WorkflowError | undefined): ResponseMessage["error"] {
  if (error === undefined) return { name: "Error", message: "unknown error" };
  const wire: NonNullable<ResponseMessage["error"]> = {
    name: error.name,
    message: error.message,
    code: error.code,
  };
  if (error.violations !== undefined) wire.violations = error.violations;
  if (error.finalText !== undefined) wire.finalText = error.finalText;
  return wire;
}

/** 保留字符串尾部 limit 字节内的内容（stderr 截断）。 */
function trimTail(value: string, limit: number): string {
  if (Buffer.byteLength(value, "utf8") <= limit) return value;
  return value.slice(-limit);
}

/** abort reason 是 `{ superseded: <runId> }` 时取出后继 id；其余形状返回 undefined。 */
function readSupersededBy(reason: unknown): string | undefined {
  if (typeof reason !== "object" || reason === null) return undefined;
  const value = (reason as { superseded?: unknown }).superseded;
  return typeof value === "string" && value.length > 0 ? value : undefined;
}
