// ============================================================
// Permission Service - Permission checking and decision making
// ============================================================

import {
  AMEND_WORKFLOW_TOOL_NAME,
  isAmendWorkflowOwnedPredecessor,
  PermissionCapabilityGroup,
  type PermissionCapabilityGroup as PermissionCapabilityGroupType,
  type PermissionRuleValue,
  type PermissionRuleset,
  type PermissionUpdate,
  type CollaborationMode,
  type ModelToolSideEffectScope,
  type RiskLevel,
  type ToolPermissionSpec,
} from "@zcode/contracts";
import { OFFICIAL_CUA_PERMISSION_RULE_TOOL_NAME } from "@zcode/shared";
import { resolvePlanModeTransitionPermission } from "./plan-mode-policy.js";
import { webFetchRuleSubjects, wildcardToRegExp } from "./rule-matching.js";
import { isPreapprovedWorkflowDraftWrite } from "./workflow-draft-path.js";
import { applyPermissionUpdates } from "../tool/executor/permission-rules.js";
import { isWebFetchPreapprovedUrl } from "../tool/webfetch-preapproved.js";
import type { ToolPermissionRulePolicy } from "../tool/types.js";

// -----------------------------------------------
// Types
// -----------------------------------------------

/** 草稿免确认的规则号。 */
const WORKFLOW_DRAFT_PREAPPROVED_RULE_ID = "tool.workflowDraft.preapproved";

export interface PermissionContext {
  toolName: string;
  input: unknown;
  riskLevel: RiskLevel;
  mode: CollaborationMode;
  planEnabled?: boolean;
  prePlanMode?: Exclude<CollaborationMode, "plan">;
  /**
   * 会话工作目录。判定相对路径的落点用（目前只有 workflow 草稿免确认这一条），
   * 可选：拿不到工作目录的调用方照常按其余规则判定，不会因此少一层确认。
   */
  workingDirectory?: string;
}

export interface PermissionToolCapability {
  allowedInPlanMode?: boolean;
  alwaysAsk?: boolean;
  readOnly?: boolean;
  destructive?: boolean;
  requiresUserInteraction?: boolean;
  sideEffectScope?: ModelToolSideEffectScope;
  riskLevel?: RiskLevel;
  needsApproval?: boolean;
  permissionCapabilityGroup?: PermissionCapabilityGroupType;
  permission?: ToolPermissionSpec;
}

export type PermissionBehavior = "allow" | "ask" | "deny";

export interface PermissionDecisionResult {
  decision: PermissionBehavior;
  allowed: boolean;
  reason?: string;
  modifiedInput?: unknown;
  escalated: boolean;
  mode: CollaborationMode;
  ruleId: string;
  riskLevel: RiskLevel;
  sideEffectScope?: ModelToolSideEffectScope;
  /**
   * 该 ask 来自工具的 alwaysAsk 声明，不是模式或规则推导出来的。下游（PreToolUse hook 的
   * allow 覆盖）靠这个结构化标记识别"不可抹掉的确认"，而不是去匹配 ruleId 字符串。
   */
  alwaysAsk?: boolean;
}

// -----------------------------------------------
// Permission Service
// -----------------------------------------------

export class PermissionService {
  /**
   * 会话级 allow 规则（「Always allow in this session」）。
   * 一个实例 = 一个 app = 一个会话，所以"随会话消亡"不需要任何额外机制：重启 / 冷恢复 / `/new`
   * 都会造一个空的新实例。只服务 alwaysAsk gate（见 checkAlwaysAsk），普通工具的模式语义不认它。
   */
  private sessionRules: PermissionRuleset = { version: 1 };

  constructor(private config: PermissionConfig = defaultPermissionConfig) {}

  grantSessionPermission(updates: PermissionUpdate[]): void {
    this.sessionRules = applyPermissionUpdates(this.sessionRules, updates);
  }

  checkPermission(
    context: PermissionContext,
    toolCapability?: PermissionToolCapability,
    projectRules?: PermissionRuleset | null,
    rulePolicy?: ToolPermissionRulePolicy,
  ): PermissionDecisionResult {
    const capability = this.resolveCapability(context, toolCapability);
    const planModeTransition = resolvePlanModeTransitionPermission(context);

    if (planModeTransition) {
      return planModeTransition.behavior === "allow"
        ? this.allow(context, capability, planModeTransition.ruleId, planModeTransition.reason)
        : this.deny(context, capability, planModeTransition.ruleId, planModeTransition.reason);
    }

    if (capability.requiresUserInteraction) {
      if (this.config.disallowedTools.has(context.toolName)) {
        return this.deny(
          context,
          capability,
          "rule.disallowedTools",
          `Tool ${context.toolName} is explicitly disallowed`,
        );
      }

      return this.ask(
        context,
        capability,
        "tool.userInteraction",
        `Tool ${context.toolName} requires user interaction`,
      );
    }

    // 声明 alwaysAsk 的工具必须经过用户确认，不能被权限模式的放行分支绕过。
    if (capability.alwaysAsk) {
      return this.checkAlwaysAsk(context, capability, projectRules, rulePolicy);
    }

    const planEnabled = context.planEnabled ?? context.mode === "plan";
    if (context.mode === "yolo" && !planEnabled) {
      return this.allow(context, capability, "mode.yolo", "Yolo mode bypasses permission prompts");
    }

    if (context.mode === "auto") {
      return this.deny(
        context,
        capability,
        "mode.auto.unimplemented",
        "Auto mode is reserved but not implemented yet",
      );
    }

    if (this.config.disallowedTools.has(context.toolName)) {
      return this.deny(
        context,
        capability,
        "rule.disallowedTools",
        `Tool ${context.toolName} is explicitly disallowed`,
      );
    }

    if (this.matchesProjectRules(projectRules, "deny", context, capability, rulePolicy)) {
      return this.deny(
        context,
        capability,
        "rule.project.deny",
        `Tool ${context.toolName} is denied by project permission rules`,
      );
    }

    if (this.matchesProjectRules(projectRules, "ask", context, capability, rulePolicy)) {
      return this.ask(
        context,
        capability,
        "rule.project.ask",
        `Tool ${context.toolName} requires approval by project permission rules`,
      );
    }

    if (planEnabled) {
      return this.checkPlanMode(context, capability);
    }

    if (this.matchesProjectRules(projectRules, "allow", context, capability, rulePolicy)) {
      return this.allow(
        context,
        capability,
        "rule.project.allow",
        `Tool ${context.toolName} is allowed by project permission rules`,
      );
    }

    if (this.isPreapprovedWebFetchRequest(context)) {
      return this.allow(
        context,
        capability,
        "tool.webfetch.preapproved",
        "WebFetch URL is preapproved",
      );
    }

    // workflow 草稿免确认：
    // 与 WebFetch 预批同一位次——排在 plan 分支之后，因为 plan 模式必须继续拦下一切写入，
    // 草稿也是写入；也排在项目 deny / ask 之后，项目规则照样压得过它。判定本身见
    // workflow-draft-path.ts（含"为什么这样放行是安全的"）。
    if (
      isPreapprovedWorkflowDraftWrite({
        input: context.input,
        toolName: context.toolName,
        workingDirectory: context.workingDirectory,
      })
    ) {
      return this.allow(
        context,
        capability,
        WORKFLOW_DRAFT_PREAPPROVED_RULE_ID,
        "Workflow draft file is preapproved",
      );
    }

    if (this.config.allowedTools.has(context.toolName)) {
      return this.allow(
        context,
        capability,
        "rule.allowedTools",
        `Tool ${context.toolName} is explicitly allowed`,
      );
    }

    if (context.mode === "edit") {
      return this.checkEditMode(context, capability);
    }

    return this.checkBuildMode(context, capability);
  }

  private matchesProjectRules(
    ruleset: PermissionRuleset | null | undefined,
    behavior: PermissionBehavior,
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    rulePolicy?: ToolPermissionRulePolicy,
  ): boolean {
    const rules = ruleset?.[behavior];
    if (!Array.isArray(rules)) return false;
    const toolRules = rules.filter((rule) =>
      this.matchesRuleScope(rule, context.toolName, capability),
    );
    if (toolRules.length === 0) return false;
    if (rulePolicy) return rulePolicy.evaluateRules(behavior, toolRules);
    return toolRules.some((rule) => this.matchesRule(rule, context, capability));
  }

  private matchesRule(
    rule: PermissionRuleValue,
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
  ): boolean {
    if (!this.matchesRuleScope(rule, context.toolName, capability)) return false;
    if (!rule.ruleContent) return true;

    const subjects = this.ruleSubjects(context.input, context.toolName);
    if (subjects.length === 0) return false;

    return subjects.some((subject) => this.matchesRuleContent(subject, rule.ruleContent!));
  }

  private matchesRuleToolName(ruleToolName: string, contextToolName: string): boolean {
    if (ruleToolName === contextToolName) return true;
    return contextToolName === "Write" && ruleToolName === "Edit";
  }

  private matchesRuleScope(
    rule: PermissionRuleValue,
    contextToolName: string,
    capability: ResolvedPermissionCapability,
  ): boolean {
    if (rule.toolName === OFFICIAL_CUA_PERMISSION_RULE_TOOL_NAME) {
      // 保留 key 只有在当前 tool entry 另行携带宿主验证后的 official_cua
      // capability 时才匹配。同名第三方 MCP、authority 漂移以及旧普通 tool
      // 都不能把可解析的 wire/storage 字符串升级成可信能力。
      return capability.permissionCapabilityGroup === PermissionCapabilityGroup.OfficialCua;
    }
    return this.matchesRuleToolName(rule.toolName, contextToolName);
  }

  private ruleSubjects(input: unknown, toolName: string): string[] {
    if (typeof input === "string") return [input];
    if (!input || typeof input !== "object") return [];

    const record = input as Record<string, unknown>;
    if (toolName === "WebFetch" && typeof record.url === "string") {
      return webFetchRuleSubjects(record.url);
    }

    for (const key of ["command", "url", "file_path", "path", "pattern", "patch_text"]) {
      const value = record[key];
      if (typeof value === "string") return [value];
    }

    return [];
  }

  private isPreapprovedWebFetchRequest(context: PermissionContext): boolean {
    if (context.toolName !== "WebFetch") return false;
    if (!context.input || typeof context.input !== "object") return false;
    const url = (context.input as Record<string, unknown>).url;
    return typeof url === "string" && isWebFetchPreapprovedUrl(url);
  }

  private matchesRuleContent(subject: string, ruleContent: string): boolean {
    if (ruleContent.endsWith(":*")) {
      const prefix = ruleContent.slice(0, -2);
      return (
        subject === prefix || subject.startsWith(`${prefix} `) || subject.startsWith(`${prefix}\t`)
      );
    }

    if (ruleContent.includes("*")) {
      return wildcardToRegExp(ruleContent).test(subject);
    }

    return subject === ruleContent;
  }

  /**
   * 工具自报 alwaysAsk 时的判定：ask 压过所有"放行"分支（yolo 直通、plan 的 readOnly 直通），
   * 但**压不过"阻断"**——所以这里先自己走一遍硬阻断判定。
   *
   * 为什么不直接返回 ask：disallowedTools 是用户配置的硬禁用，项目 deny 规则符合工具自报的
   * denyPriority: "beforeAsk"，auto 模式是"该模式未实现"的保护。少了这一步，一个被硬禁用的
   * 工具会退化成"弹个窗、用户一点就能跑"。
   *
   * 这些判定在 checkPermission 里按原有顺序还会各自出现一次；此处刻意只覆盖 alwaysAsk 工具，
   * 不改动其他工具的既有优先级（尤其 yolo 目前先于 disallowedTools 放行这一点）。
   */
  private checkAlwaysAsk(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    projectRules?: PermissionRuleset | null,
    rulePolicy?: ToolPermissionRulePolicy,
  ): PermissionDecisionResult {
    if (context.mode === "auto") {
      return this.deny(
        context,
        capability,
        "mode.auto.unimplemented",
        "Auto mode is reserved but not implemented yet",
      );
    }
    if (this.config.disallowedTools.has(context.toolName)) {
      return this.deny(
        context,
        capability,
        "rule.disallowedTools",
        `Tool ${context.toolName} is explicitly disallowed`,
      );
    }
    if (this.matchesProjectRules(projectRules, "deny", context, capability, rulePolicy)) {
      return this.deny(
        context,
        capability,
        "rule.project.deny",
        `Tool ${context.toolName} is denied by project permission rules`,
      );
    }
    // 会话免确认：阻断分支之后、ask 之前。命中即放行，不发 permission 事件、不弹窗；
    // 与 gate 本身一样不看模式（yolo / plan / build 一致）。
    if (this.matchesProjectRules(this.sessionRules, "allow", context, capability, rulePolicy)) {
      return this.allow(
        context,
        capability,
        "rule.session.allow",
        `Tool ${context.toolName} was allowed for this session`,
      );
    }
    // 修订免确认：AmendWorkflow 的前驱是
    // **本会话发起**的 run、且不是用户亲手停下的，即放行。与会话规则同位——阻断分支之后、ask 之前，
    // 不看模式。事实来自 resolveInput 回填的 `predecessor`（journal 的 parent_session_id / stopReason），
    // 不是内存表：重启、冷恢复后依然成立，也没有可播种、可撤销的东西。别的会话的 run、用户停过的
    // run 照常 ask：钥匙是 run 的归属，不是字段的在场。
    if (this.isOwnedWorkflowAmend(context)) {
      return this.allow(
        context,
        capability,
        "rule.session.workflowOwner",
        `Tool ${context.toolName} amends a run this session started`,
      );
    }
    return this.ask(
      context,
      capability,
      "tool.alwaysAsk",
      `Tool ${context.toolName} always requires explicit approval`,
    );
  }

  /** AmendWorkflow 且回填的 `predecessor` 说「本会话的 run、非用户停下」。 */
  private isOwnedWorkflowAmend(context: PermissionContext): boolean {
    if (context.toolName !== AMEND_WORKFLOW_TOOL_NAME) return false;
    if (!context.input || typeof context.input !== "object") return false;
    // 谓词本体住在契约里：就地调并发落回修订时读的必须是同一条规则，不能各写一遍。
    return isAmendWorkflowOwnedPredecessor((context.input as Record<string, unknown>).predecessor);
  }

  private checkPlanMode(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
  ): PermissionDecisionResult {
    if (capability.readOnly && !capability.destructive) {
      return this.allow(
        context,
        capability,
        "mode.plan.readOnly",
        "Plan mode allows read-only tool execution",
      );
    }

    if (this.isMcpToolCapability(capability) && !capability.destructive) {
      return this.allow(
        context,
        capability,
        "mode.plan.mcp",
        "Plan mode allows non-destructive MCP tool execution",
      );
    }

    if (
      capability.allowedInPlanMode &&
      capability.sideEffectScope === "session" &&
      !capability.destructive &&
      !capability.needsApproval
    ) {
      return this.allow(
        context,
        capability,
        "mode.plan.explicitSessionCapability",
        "Plan mode allows this explicit non-destructive session control action",
      );
    }

    return this.deny(
      context,
      capability,
      "mode.plan.nonReadOnly",
      "Plan mode only allows read-only, non-destructive tools",
    );
  }

  private isMcpToolCapability(capability: ResolvedPermissionCapability): boolean {
    return capability.permissionName === "mcp";
  }

  private checkBuildMode(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
  ): PermissionDecisionResult {
    if (capability.readOnly && !capability.destructive && !capability.needsApproval) {
      return this.allow(
        context,
        capability,
        "mode.build.readOnly",
        "Build mode allows read-only tools",
      );
    }

    if (capability.riskLevel === "critical") {
      return this.ask(
        context,
        capability,
        "mode.build.criticalRisk",
        "Critical risk tools require explicit approval",
      );
    }

    if (capability.riskLevel === "high" && !this.config.autoApproveHighRisk) {
      return this.ask(
        context,
        capability,
        "mode.build.highRisk",
        "High risk tools require explicit approval",
      );
    }

    if (
      capability.sideEffectScope === "session" &&
      capability.riskLevel === "low" &&
      !capability.destructive &&
      !capability.needsApproval
    ) {
      return this.allow(
        context,
        capability,
        "mode.build.sessionState",
        "Build mode allows low-risk session-local state updates",
      );
    }

    if (
      capability.needsApproval ||
      capability.destructive ||
      capability.sideEffectScope !== "none"
    ) {
      return this.ask(
        context,
        capability,
        "mode.build.sideEffect",
        "Tool has side effects and requires approval",
      );
    }

    return this.allow(
      context,
      capability,
      "mode.build.lowRisk",
      "Build mode allows low-risk tool execution",
    );
  }

  private checkEditMode(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
  ): PermissionDecisionResult {
    if (capability.permissionName === "edit" && capability.sideEffectScope === "workspace") {
      return this.allow(
        context,
        capability,
        "mode.edit.fileEdit",
        "Edit mode allows file edit tools",
      );
    }

    return this.checkBuildMode(context, capability);
  }

  requiresApproval(context: PermissionContext, toolCapability?: PermissionToolCapability): boolean {
    const decision = this.checkPermission(context, toolCapability);
    return decision.decision === "ask";
  }

  getRiskLevel(toolName: string, toolCapability?: PermissionToolCapability): RiskLevel {
    if (toolCapability?.riskLevel) {
      return toolCapability.riskLevel;
    }

    if (this.isReadOnlyTool(toolName)) {
      return "low";
    }

    if (this.isWriteTool(toolName)) {
      return "medium";
    }

    if (this.isDestructiveTool(toolName)) {
      return "high";
    }

    return "medium";
  }

  private isReadOnlyTool(name: string): boolean {
    return new Set([
      "Read",
      "Glob",
      "Grep",
      "WebSearch",
      "WebFetch",
      "TodoRead",
      "TodoWrite",
      "AskUserQuestion",
      "Agent",
      "Task",
      "Skill",
    ]).has(name);
  }

  private isWriteTool(name: string): boolean {
    return new Set(["Write", "Edit", "ApplyPatch", "Bash"]).has(name);
  }

  private isDestructiveTool(name: string): boolean {
    return new Set(["Bash"]).has(name);
  }

  private resolveCapability(
    context: PermissionContext,
    toolCapability?: PermissionToolCapability,
  ): ResolvedPermissionCapability {
    return {
      allowedInPlanMode: toolCapability?.allowedInPlanMode ?? false,
      alwaysAsk: toolCapability?.permission?.alwaysAsk ?? toolCapability?.alwaysAsk ?? false,
      readOnly: toolCapability?.readOnly ?? this.isReadOnlyTool(context.toolName),
      destructive: toolCapability?.destructive ?? this.isDestructiveTool(context.toolName),
      requiresUserInteraction:
        toolCapability?.requiresUserInteraction ??
        (toolCapability?.permission?.sideEffectScope ?? toolCapability?.sideEffectScope) ===
          "userInteraction",
      sideEffectScope:
        toolCapability?.permission?.sideEffectScope ??
        toolCapability?.sideEffectScope ??
        (this.isReadOnlyTool(context.toolName) ? "none" : "workspace"),
      riskLevel:
        toolCapability?.permission?.riskLevel ??
        this.getRiskLevel(context.toolName, toolCapability),
      needsApproval:
        toolCapability?.permission?.needsApproval ??
        toolCapability?.needsApproval ??
        !this.isReadOnlyTool(context.toolName),
      permissionCapabilityGroup: toolCapability?.permissionCapabilityGroup,
      permissionName: toolCapability?.permission?.permission,
    };
  }

  private allow(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    ruleId: string,
    reason?: string,
  ): PermissionDecisionResult {
    return this.result("allow", context, capability, ruleId, reason);
  }

  private ask(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    ruleId: string,
    reason: string,
  ): PermissionDecisionResult {
    return this.result("ask", context, capability, ruleId, reason);
  }

  private deny(
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    ruleId: string,
    reason: string,
  ): PermissionDecisionResult {
    return this.result("deny", context, capability, ruleId, reason);
  }

  private result(
    decision: PermissionBehavior,
    context: PermissionContext,
    capability: ResolvedPermissionCapability,
    ruleId: string,
    reason?: string,
  ): PermissionDecisionResult {
    return {
      decision,
      allowed: decision === "allow",
      escalated: decision === "ask",
      mode: context.mode,
      reason,
      riskLevel: capability.riskLevel,
      ruleId,
      sideEffectScope: capability.sideEffectScope,
      ...(capability.alwaysAsk ? { alwaysAsk: true } : {}),
    };
  }
}

interface ResolvedPermissionCapability {
  allowedInPlanMode: boolean;
  alwaysAsk: boolean;
  readOnly: boolean;
  destructive: boolean;
  requiresUserInteraction: boolean;
  sideEffectScope: ModelToolSideEffectScope;
  riskLevel: RiskLevel;
  needsApproval: boolean;
  permissionCapabilityGroup?: PermissionCapabilityGroupType;
  permissionName?: string;
}

// -----------------------------------------------
// Configuration
// -----------------------------------------------

export interface PermissionConfig {
  allowedTools: Set<string>;
  disallowedTools: Set<string>;
  autoApproveHighRisk: boolean;
  allowMediumRiskInAutoMode: boolean;
}

export const defaultPermissionConfig: PermissionConfig = {
  allowedTools: new Set(),
  disallowedTools: new Set(),
  autoApproveHighRisk: false,
  allowMediumRiskInAutoMode: false,
};
