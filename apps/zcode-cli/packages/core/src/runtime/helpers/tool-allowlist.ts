import {
  AMEND_WORKFLOW_TOOL_NAME,
  CREATE_WORKFLOW_TOOL_NAME,
  RESOLVE_WORKFLOW_QUESTION_TOOL_NAME,
  RESPOND_TO_COORDINATOR_TOOL_NAME,
  RESUME_WORKFLOW_RUN_TOOL_NAME,
  SAVE_WORKFLOW_TOOL_NAME,
} from "@zcode/contracts";
import { EXPLORE_AGENT_ALLOWED_TOOLS } from "../../subagent/explore-tools.js";
import type { AgentRuntimeConfig } from "../types.js";
import { normalizeToolNameAlias } from "../../tool/tool-visibility.js";

const EXPLORE_AGENT_ALLOWED_TOOL_SET = new Set<string>(EXPLORE_AGENT_ALLOWED_TOOLS);

/**
 * workflow child 运行时额外不注册的工具。
 *
 * 根因：workflow child（`/workflow`、`/expert` 与 script workflow 的 agent 调用）被强制成
 * yolo，而它的交互事件只在 subagent 路径上镜像到父会话（`runtime/methods/subagent.ts` 是
 * `mirrorSubagentToolEvent` 的唯一接入点）。CreateWorkflow 声明了 alwaysAsk，所以一段能编译
 * 的脚本会在 child 里发出一个父界面看不到的确认请求，一路挂到权限超时。直接不注册该工具，
 * child 拿到的是干净的"工具不可用"错误，而不是隐形挂起。
 *
 * 长期解法是把 workflow child 的交互事件也镜像到父会话，随执行引擎落地时一并处理
 */
const WORKFLOW_CHILD_DISALLOWED_TOOLS = [
  CREATE_WORKFLOW_TOOL_NAME,
  // AmendWorkflow 与 CreateWorkflow 同一道 alwaysAsk 门、同一种嵌套编排，因同一个根因入列。
  AMEND_WORKFLOW_TOOL_NAME,
  // SaveWorkflow 因**同一个**根因入列：它也声明了 alwaysAsk，所以在 child 里同样会发出一个
  // 父界面看不到的确认请求并挂到超时。ListSavedWorkflows 不在列——那条禁令的理由是无窗可弹，
  // 只读查询不适用（与两个 run 内省工具同理）。
  SAVE_WORKFLOW_TOOL_NAME,
  // **结构性禁用**——child 内不得再编排。恢复 = 重新执行整块脚本（完结节点 replay、未完结
  // 重派发），与 CreateWorkflow 新启是同一能力档；纵使免确认后「无窗可弹」的技术问题消失，
  // 嵌套编排（child 再拉起或复活 run）仍不开放。
  RESUME_WORKFLOW_RUN_TOOL_NAME,
  // ResolveWorkflowQuestion 与 ResumeWorkflowRun 同属**结构性禁用**，但守的是另一条不变式：
  // 升级问答的语义是「actor 提问，创建这条工作流的那一方作答」。
  // 让一个 child 顺手作答，等于把「把判断权交回给能改掉那道门的人」悄悄退化成 actor 之间的
  // 互相说服——而一个同样被挡在门内的 actor 恰恰是最没有资格拍板的那个。
  //
  // 与 bootstrap 的 `ACTOR_DISALLOWED_TOOLS` **刻意重复**（CreateWorkflow 已有同样的双列
  // 先例）：那一份是 driver 侧 persona 工具面的减法；这一份按 taskType 覆盖全部 workflow
  // child，不依赖 driver 记得写。
  RESOLVE_WORKFLOW_QUESTION_TOOL_NAME,
] as const;

/**
 * 运行时最终的工具禁用名单：turn 级 `toolDisallowlist` 叠加按 taskType 推导出的结构性禁用。
 * 放在这里而不是各个 child runtime 的构造点，是因为构造点有两个
 * （`workflow-facade.ts` 与 `script-workflow-child-runtime.ts`），两份名单必然漂移。
 */
export function resolveRuntimeDisallowedTools(
  config: AgentRuntimeConfig,
): readonly string[] | undefined {
  if (config.taskType !== "workflow_child") return config.toolDisallowlist;

  const disallowed = new Set(config.toolDisallowlist ?? []);
  for (const toolName of WORKFLOW_CHILD_DISALLOWED_TOOLS) disallowed.add(toolName);
  return [...disallowed];
}

/**
 * 动态工作流开关在 registerBuiltInTools 上的取值。
 * **缺席即开启**：TUI 保留默认工具面；headless 根据 --enable-workflow 显式写 true/false，
 * protocol session 由受信 Host 控制，workflow_child 继承父配置。fail-closed 的缺省值在
 * headless 入口和协议服务端的 appRuntimePreferences，不在这一层。
 *
 * 之所以和 resolveRuntimeDisallowedTools 一样收在这里而不是写在调用点：注册面有**两个**入口
 * （helpers/runtime-tools.ts 的首次装配、methods/embedded-search-branch.ts 的分支刷新），
 * 两个入口必须使用同一规则，否则刷新工具列表时可能重新注册已关闭的工作流工具。
 */
export function resolveRuntimeDynamicWorkflowToolsIncluded(config: AgentRuntimeConfig): boolean {
  return config.dynamicWorkflowEnabled !== false;
}

export function resolveBuiltInToolAllowlist(
  config: AgentRuntimeConfig,
): readonly string[] | undefined {
  const normalizedAllowlist = normalizeBuiltInToolAllowlist(config.toolAllowlist);

  if (config.toolset !== "explore") {
    return appendChildControlTool(config, normalizedAllowlist);
  }

  if (!normalizedAllowlist) {
    return appendChildControlTool(config, EXPLORE_AGENT_ALLOWED_TOOLS);
  }

  return appendChildControlTool(
    config,
    normalizedAllowlist.filter((toolName) => EXPLORE_AGENT_ALLOWED_TOOL_SET.has(toolName)),
  );
}

function appendChildControlTool(
  config: AgentRuntimeConfig,
  allowlist: readonly string[] | undefined,
): readonly string[] | undefined {
  if (!allowlist) {
    return allowlist;
  }

  if (config.taskType === "subagent_child") {
    if (allowlist.includes(RESPOND_TO_COORDINATOR_TOOL_NAME)) {
      return allowlist;
    }
    // Explore 会在 runtime 注册前再次求工具交集，child 控制工具必须在最终结果补回。
    return [...allowlist, RESPOND_TO_COORDINATOR_TOOL_NAME];
  }

  // workflow child 没有 allowlist 收窄（persona 无工具档位，工具面只有减法，见
  // bootstrap 的 workflow-actor-tools.ts），submit_result / escalate 由 includeSubmitResult /
  // includeEscalate 两道门注册，不需要在这里补回。
  return allowlist;
}

function normalizeBuiltInToolAllowlist(
  allowlist: readonly string[] | undefined,
): readonly string[] | undefined {
  return allowlist?.map((toolName) => normalizeToolNameAlias(toolName));
}
