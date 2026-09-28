import {
  AutomationCreateLimitError,
  hasRelativeDelayMinutes,
  isAutomationCreateLimitError,
  type AutomationPort,
  type CronAutomation,
} from "@zcode/contracts";
import {
  parseModelPickerValue,
  zcodeAutomationCheckTaskBindingResultSchema,
  zcodeAutomationCreateResultSchema,
  zcodeAutomationDeleteResultSchema,
  zcodeAutomationListResultSchema,
  zcodeAutomationUpdateResultSchema,
  zcodeProtocolMethods,
  type ZCodeAutomationProtocol,
} from "@zcode/shared";
import {
  ProtocolRequestError,
  type ZCodeProtocolAgentServerContext,
  type ZCodeProtocolSessionRecord,
} from "./server-types.js";

const AUTOMATION_CREATE_FROM_AUTOMATION_RUN_ERROR =
  "Cannot create a scheduled task while running a scheduled task.";

const AUTOMATION_CREATE_IN_BOUND_SESSION_ERROR =
  "Cannot create a scheduled task inside a session that already belongs to a scheduled task. " +
  "Ask the user to start a new chat to create another scheduled task.";

const AUTOMATION_CREATE_BOUND_SESSION_CHECK_ERROR =
  "Cannot verify whether this session belongs to a scheduled task. Try again later.";

export function createProtocolAutomationPort(
  context: ZCodeProtocolAgentServerContext,
  // 归属会话解析器：automation-port 是按 session 构造的，直接绑定「自己所服务的 session
  // record」。避免依赖 context.sessions 查找——V4/desktop 会话未必登记在该 legacy map 里，
  // 之前会导致 activeSession 命中失败、权限/思考等级丢失（模型有 createContext 兜底才幸存）。
  resolveOwnSession?: () => ZCodeProtocolSessionRecord | undefined,
): AutomationPort {
  return {
    async create(input, createContext) {
      // 优先用归属 session（本工具运行所在会话）；退回按 createContext.sessionId 查 legacy map。
      const activeSession =
        resolveOwnSession?.() ??
        (createContext?.sessionId ? context.sessions.get(createContext.sessionId) : undefined);
      if (activeSession?.activeAutomationId?.trim()) {
        // 当前 turn 已经由 automationId 派发；继续 CronCreate 会形成递归定时任务链。
        // 这个判断发生在 automation-port 内，命中时不会调用 protocol automation/create。
        throw new Error(AUTOMATION_CREATE_FROM_AUTOMATION_RUN_ERROR);
      }
      // 桌面交互输入直连 CLI（绕过 host adapter 的 toolDenylist 注入），普通用户在
      // 一个已归属定时任务的会话里继续输入时，CronCreate 仍被注册且没有 per-turn 过滤，会绕过
      // 前面所有守卫再次创建定时任务。这里在创建入口按 targetTaskId 做与入口路径无关的兜底：
      // 只要当前会话已经是某个 automation 的绑定会话（= 定时任务会话），就拒绝再次创建；普通会话
      // 首次创建时还没有绑定，正常放行。归属判断必须走专用 EXISTS 协议，不能读取完整列表；否则
      // 任意历史任务的展示字段损坏都会让当前会话误报“无法验证”。
      const ownSessionId = createContext?.sessionId ?? activeSession?.app.sessionId;
      if (ownSessionId) {
        let bound: boolean;
        try {
          try {
            const result = await context.requestClient(
              zcodeProtocolMethods.automationCheckTaskBinding,
              { targetTaskId: ownSessionId },
              zcodeAutomationCheckTaskBindingResultSchema,
            );
            bound = result.bound;
          } catch (error) {
            if (!(error instanceof ProtocolRequestError && error.code === -32601)) {
              throw error;
            }
            // 协议版本仍为 1，新 CLI 连接升级前仍存活或远端的旧 Host 时，专用归属
            // 方法会返回 -32601。只有“方法不存在”能证明是能力差异，此时回退旧版列表筛选；
            // 数据库、传输和协议错误仍交给外层 fail-closed，不能误判为未绑定。
            const legacyResult = await context.requestClient(
              zcodeProtocolMethods.automationList,
              {},
              zcodeAutomationListResultSchema,
            );
            bound = legacyResult.automations.some(
              (automation) => automation.targetTaskId === ownSessionId,
            );
          }
        } catch (error) {
          context.logger?.warn("Failed to check bound automations before CronCreate", {
            errorMessage: error instanceof Error ? error.message : String(error),
            event: "automation.create.bound_session_check.failed",
            sessionId: ownSessionId,
          });
          // 会话归属查询是阻止递归 CronCreate 的授权边界；未知不能等同于未绑定，
          // 否则 host / 数据库短暂故障会重新开放创建能力。查询失败必须 fail-closed。
          throw new Error(AUTOMATION_CREATE_BOUND_SESSION_CHECK_ERROR);
        }
        if (bound) {
          throw new Error(AUTOMATION_CREATE_IN_BOUND_SESSION_ERROR);
        }
      }
      // CronCreate 的工具上下文只携带了部分配置，导致权限或思考等级在跨层时丢失。
      // 协议边界按 sessionId 读取活跃 runtime，确保保存的是用户触发工具当下看到的配置。
      const runtimeModelSelection =
        activeSession?.app.runtime.getSessionModelSelection() ??
        (() => {
          try {
            return createContext?.model ? parseModelPickerValue(createContext.model) : undefined;
          } catch {
            return undefined;
          }
        })();
      const runtimeMode = activeSession?.app.getMode();
      const hasIntervalCarrier = input.intervalUnit !== undefined && input.interval !== undefined;
      let result;
      try {
        result = await context.requestClient(
          zcodeProtocolMethods.automationCreate,
          {
            // 相对时间不由模型换算绝对时刻；兼容协议仍要求 cronExpr，服务层会用真实时钟覆盖占位值。
            // `!== null` 判定会把省略 delayMinutes 的普通 cron 调用误走相对分支，
            // 强制 recurring=false；相对任务的唯一口径是 hasRelativeDelayMinutes（显式数字）。
            cronExpr: input.cron ?? "* * * * *",
            ...(hasRelativeDelayMinutes(input) ? { relativeDelayMinutes: input.delayMinutes } : {}),
            prompt: input.prompt,
            title: input.title,
            recurring: hasRelativeDelayMinutes(input)
              ? false
              : hasIntervalCarrier
                ? true
                : (input.recurring ?? true),
            // workspace 仍由 protocol server 从当前 session 注入。
            ...(runtimeModelSelection ? { modelSelection: runtimeModelSelection } : {}),
            ...(runtimeMode ? { mode: runtimeMode === "auto" ? "build" : runtimeMode } : {}),
            ...(createContext?.sessionId ? { targetTaskId: createContext.sessionId } : {}),
            ...(activeSession?.activeBotDeliveryTarget
              ? { botDeliveryTarget: activeSession.activeBotDeliveryTarget }
              : {}),
            ...(hasIntervalCarrier
              ? {
                  // 新建 carrier 不透传有限上限；create 协议没有 maxRuns=null 清除语义。
                  intervalUnit: input.intervalUnit,
                  interval: input.interval,
                }
              : input.maxRuns !== undefined
                ? { maxRuns: input.maxRuns }
                : {}),
          },
          zcodeAutomationCreateResultSchema,
        );
      } catch (error) {
        if (!isAutomationCreateLimitError(error)) throw error;
        // 协议错误过去以普通 Error 进入工具循环，模型会把错误里的删除建议当成
        // 可执行恢复步骤，继而反复 CronList/CronDelete/CronCreate。映射成稳定领域错误，
        // 让 core 能在不依赖 shared 实现的情况下关闭当前 turn 的工具恢复边界。
        throw new AutomationCreateLimitError(
          error instanceof Error ? error.message : String(error),
          error,
        );
      }
      const automation = toCronAutomation(result.automation);
      const title = automation.title.trim();
      if (activeSession && title.length > 0) {
        try {
          // 会话内 CronCreate 会在首轮回复中创建 automation，但首条消息触发的
          // session_title sidecar 可能迟到并根据助手解释生成标题，导致标题变成“我无法...”。
          // CronCreate 成功后把当前会话标题固定为 automation 标题，阻止 generated 标题覆盖。
          await activeSession.app.setCustomSessionTitle({
            title,
            traceContext: activeSession.traceContext,
          });
        } catch (error) {
          context.logger?.warn("Failed to freeze session title after CronCreate", {
            errorMessage: error instanceof Error ? error.message : String(error),
            event: "automation.session_title_freeze.failed",
            sessionId: createContext?.sessionId,
          });
        }
      }
      return automation;
    },
    async update(input) {
      const hasIntervalCarrier = input.intervalUnit !== undefined && input.interval !== undefined;
      const result = await context.requestClient(
        zcodeProtocolMethods.automationUpdate,
        {
          automationId: input.id,
          ...(input.title !== undefined ? { title: input.title } : {}),
          ...(input.cron !== undefined ? { cronExpr: input.cron } : {}),
          ...(input.prompt !== undefined ? { prompt: input.prompt } : {}),
          ...(hasIntervalCarrier
            ? {
                // 历史一次性任务只透传 carrier 会保留 recurring=false，首次派发后
                // 被 repository 标记 completed。carrier 必须在协议边界原子切换为无限循环。
                recurring: true,
                maxRuns: null,
                intervalUnit: input.intervalUnit,
                interval: input.interval,
              }
            : {
                ...(input.recurring !== undefined ? { recurring: input.recurring } : {}),
                ...(input.maxRuns !== undefined ? { maxRuns: input.maxRuns } : {}),
              }),
        },
        zcodeAutomationUpdateResultSchema,
      );
      return toCronAutomation(result.automation);
    },
    async list() {
      const result = await context.requestClient(
        zcodeProtocolMethods.automationList,
        {},
        zcodeAutomationListResultSchema,
      );
      return result.automations.map(toCronAutomation);
    },
    async delete(input) {
      const result = await context.requestClient(
        zcodeProtocolMethods.automationDelete,
        { automationId: input.id },
        zcodeAutomationDeleteResultSchema,
      );
      return result.deleted;
    },
  };
}

function normalizeCronAutomationMode(
  mode: ZCodeAutomationProtocol["mode"],
): CronAutomation["mode"] {
  switch (mode) {
    case undefined:
      return undefined;
    case "plan":
    case "edit":
    case "yolo":
    case "build":
      return mode;
    case "auto":
    case "autoEdit":
      return "build";
    default: {
      const exhaustiveMode: never = mode;
      return exhaustiveMode;
    }
  }
}

function toCronAutomation(input: ZCodeAutomationProtocol): CronAutomation {
  return {
    automationId: input.automationId,
    title: input.title,
    cronExpr: input.cronExpr,
    prompt: input.prompt,
    enabled: input.enabled,
    lifecycleStatus: input.lifecycleStatus,
    nextRunAt: input.nextRunAt,
    lastRunAt: input.lastRunAt,
    runCount: input.runCount,
    recurring: input.recurring,
    maxRuns: input.maxRuns,
    modelSelection: input.modelSelection,
    // 显式按既有 session 权限语义降级，协议以后新增 mode 时由穷尽检查强制同步处理。
    mode: normalizeCronAutomationMode(input.mode),
    // 透传权威 scheduleRule；会话卡片必须读到本字段才能展示 cron 无法表达的真实间隔
    // （如每50小时、每40天），否则只能从兼容 cronExpr 推断出「每小时的第00分」等错误展示。
    scheduleRule: input.scheduleRule,
  };
}
