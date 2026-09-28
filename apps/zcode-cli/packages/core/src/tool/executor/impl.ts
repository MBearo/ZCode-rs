import type { TraceContext, TurnId } from "@zcode/contracts";
import { createDenyPermissionBroker } from "../../permission/broker.js";
import type { ToolSchedule } from "../scheduler.js";
import type { ExecutableToolCall, ToolBatchEvent, ToolExecutionResult } from "../types.js";
import { BackgroundTaskTracker } from "./background-tasks.js";
import { executeToolBatch, executeToolSchedule } from "./batch-runner.js";
import { executeToolCall } from "./call-runner.js";
import type {
  ToolBatchExecuteOptions,
  ToolExecuteOptions,
  ToolExecutor,
  ToolExecutorDeps,
  ToolExecutorOptions,
} from "./types.js";

export class ToolExecutorImpl implements ToolExecutor {
  private readonly deps: ToolExecutorDeps;
  private readonly backgroundTasks: BackgroundTaskTracker;

  constructor(options: ToolExecutorOptions) {
    this.deps = {
      agentTelemetry: options.agentTelemetry,
      agentTelemetryActorKind: options.agentTelemetryActorKind,
      registry: options.registry,
      permissionService: options.permissionService,
      permissionBroker: options.permissionBroker ?? createDenyPermissionBroker(),
      emitEvent: options.emitEvent,
      enqueueBackgroundTaskNotification: options.enqueueBackgroundTaskNotification,
      shouldEnqueueBackgroundTaskNotification: options.shouldEnqueueBackgroundTaskNotification,
      sessionId: options.sessionId,
      turnId: options.turnId,
      defaultTimeoutMs: options.defaultTimeoutMs ?? 300000,
      permissionTimeoutMs: options.permissionTimeoutMs,
      logger: options.logger,
      backgroundTaskControlPort: options.backgroundTaskControlPort,
      executionPort: options.executionPort,
      browserControlPort: options.browserControlPort,
      browserDocumentationRoot: options.browserDocumentationRoot,
      fileSystemPort: options.fileSystemPort,
      httpClientPort: options.httpClientPort,
      imageProcessorPort: options.imageProcessorPort,
      pdfDocumentPort: options.pdfDocumentPort,
      model: options.model,
      embeddedSearchBackend: options.embeddedSearchBackend,
      nativeSearchEnhancementsEnabled: options.nativeSearchEnhancementsEnabled,
      skillPort: options.skillPort,
      subagentPort: options.subagentPort,
      coordinatorResponsePort: options.coordinatorResponsePort,
      workflowSubmitPort: options.workflowSubmitPort,
      workflowEscalatePort: options.workflowEscalatePort,
      artifactStore: options.artifactStore,
      automationPort: options.automationPort,
      offPeakPort: options.offPeakPort,
      sessionStore: options.sessionStore,
      sessionModePort: options.sessionModePort,
      workflowPort: options.workflowPort,
      dynamicWorkflowRunPort: options.dynamicWorkflowRunPort,
      dynamicWorkflowSnippetPort: options.dynamicWorkflowSnippetPort,
      modelCatalogPort: options.modelCatalogPort,
      runtimeTaskRegistry: options.runtimeTaskRegistry,
      readFileState: options.readFileState ?? new Map(),
      hasLoadedSkill: options.hasLoadedSkill,
      subagentBackgroundBashMaxMs: options.subagentBackgroundBashMaxMs,
      bashShellSelection: options.bashShellSelection,
      getBashShellSelection: options.getBashShellSelection,
      getWorkingDirectory: options.getWorkingDirectory ?? (() => options.workingDirectory ?? "."),
      setWorkingDirectory: options.setWorkingDirectory,
      getWorkspaceRoot:
        options.getWorkspaceRoot ??
        (() =>
          options.workspaceRoot ??
          options.getWorkingDirectory?.() ??
          options.workingDirectory ??
          "."),
      workspaceIdentity: options.workspaceIdentity,
      remoteSessionId: options.remoteSessionId,
      clientMode: options.clientMode,
      deliveryKind: options.deliveryKind,
      getMemoryRoot: options.getMemoryRoot,
      runtimeScope: options.runtimeScope ?? "main",
      traceContext: options.traceContext,
      getMode: options.getMode ?? (() => options.mode ?? "build"),
      maxConcurrency: options.maxConcurrency ?? 10,
      hookRunner: options.hookRunner,
    };
    this.backgroundTasks = new BackgroundTaskTracker(this.deps);
  }

  execute(
    toolCall: ExecutableToolCall,
    options?: ToolExecuteOptions,
  ): Promise<ToolExecutionResult> {
    return executeToolCall(this.deps, this.backgroundTasks, toolCall, options);
  }

  trackExternalBackgroundTask(
    toolCall: ExecutableToolCall,
    output: Record<string, unknown>,
    traceContext: TraceContext,
    turnId?: TurnId,
  ): Promise<void> {
    // 与 submit 路径同一个 tracker 实例、同一条 trackBackgroundTask：resume 重臂的行为
    // 按构造等于工具启动路径的行为，两条路径不可能漂移（见 ToolExecutor 接口注释）。
    return this.backgroundTasks.trackBackgroundTask(toolCall, output, traceContext, turnId);
  }

  executeBatch(
    toolCalls: ExecutableToolCall[],
    options?: ToolBatchExecuteOptions,
  ): Promise<ToolExecutionResult[]> {
    return executeToolBatch(
      this.deps,
      (toolCall, executeOptions) => {
        return this.execute(toolCall, executeOptions);
      },
      toolCalls,
      options,
    );
  }

  async *executeSchedule(
    toolCalls: ExecutableToolCall[],
    schedule: ToolSchedule,
    options?: ToolBatchExecuteOptions,
  ): AsyncGenerator<ToolBatchEvent, ToolExecutionResult[], void> {
    return yield* executeToolSchedule(
      this.deps,
      (batchToolCalls, batchOptions) => this.executeBatch(batchToolCalls, batchOptions),
      toolCalls,
      schedule,
      options,
    );
  }
}

export function createToolExecutor(options: ToolExecutorOptions): ToolExecutor {
  return new ToolExecutorImpl(options);
}
