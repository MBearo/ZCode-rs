/* eslint-disable max-lines -- Main 的完整薄转发契约集中维护请求与端口关联。 */
import { randomUUID } from "node:crypto";
import { BrowserWindow, MessageChannelMain } from "electron";
import type { MessagePortMain, UtilityProcess as ElectronUtilityProcess } from "electron";
import {
  buildRemoteWorkspaceIdentity,
  buildRemoteEnvironmentKey,
  buildSshRemoteHostKey,
  HostMessageTypes,
  HostResponseTypes,
  hostResponseMessageSchema,
  InternalChannels,
  PlatformChannels,
  resolveWorkspaceKey,
  type RemoteTarget,
  type ProviderProvisioningTrigger,
  type WindowHostRemoteWorkspaceDescriptor,
} from "@zcode/shared";
import type {
  RemoteConnectionStats,
  RemoteDisconnectReason,
  RemoteGaugeTransition,
} from "./desktopRemoteUsageArmsTelemetry.js";
import type { RemoteAssetDirs } from "./desktopRuntimeEnv.js";
import { ProviderProvisioningEnvironmentCoordinator } from "./providerProvisioningEnvironmentCoordinator.js";

interface RemoteWorkspaceSessionContext {
  workspacePath: string;
  workspaceIdentity?: string;
}

interface PendingConnect {
  requestId: string;
  webContentsId: number;
  win: BrowserWindow;
  remoteUsageTelemetryEligible: boolean;
  resolve: (sessionId: string) => void;
  reject: (error: Error) => void;
}

interface RemoteAttachmentRoute {
  webContentsId: number;
  // Main 只保留端口转发所需的 request/session 关联和脱敏 descriptor；连接/任务事实归 Host。
  descriptor: WindowHostRemoteWorkspaceDescriptor;
  rendererAttachmentId?: string;
  pendingRendererAttachment?: {
    attachmentId: string;
    previousAttachmentId?: string;
    reason: string;
    timeout: NodeJS.Timeout;
    resolve: () => void;
    reject: (error: Error) => void;
  };
  attachmentState: "attachable" | "closed";
  connectedAtMonotonicMs: number;
  connectFinalized: boolean;
  remoteUsageTelemetryEligible: boolean;
  providerProvisioningDispose?: () => void;
}

interface PendingProviderProvisioningExecution {
  readonly child: ElectronUtilityProcess;
  readonly trigger: ProviderProvisioningTrigger;
  readonly startedAtMonotonicMs: number;
  readonly resolve: () => void;
  readonly reject: (error: Error) => void;
}

function normalizeServerRemoteUrlForComparison(url: string): string {
  try {
    const parsed = new URL(url.trim());
    if (parsed.protocol === "ws:") parsed.protocol = "http:";
    if (parsed.protocol === "wss:") parsed.protocol = "https:";
    parsed.hash = "";
    parsed.search = "";
    const normalizedPath = parsed.pathname.replace(/\/+$/g, "");
    parsed.pathname = normalizedPath.endsWith("/ws")
      ? normalizedPath.slice(0, -"/ws".length) || "/"
      : normalizedPath || "/";
    return parsed.toString().replace(/\/$/g, "");
  } catch {
    return url.trim().replace(/\/+$/g, "");
  }
}

function isSameRemoteTarget(left: RemoteTarget, right: RemoteTarget): boolean {
  if (left.kind !== right.kind) return false;
  switch (left.kind) {
    case "ssh":
      return (
        right.kind === "ssh" &&
        left.host.trim().toLowerCase() === right.host.trim().toLowerCase() &&
        (left.port ?? 22) === (right.port ?? 22) &&
        left.username.trim() === right.username.trim() &&
        (left.privateKeyPath ?? "") === (right.privateKeyPath ?? "")
      );
    case "wsl":
      return (
        right.kind === "wsl" &&
        (left.distro?.trim() || "default") === (right.distro?.trim() || "default") &&
        (left.user?.trim() ?? "") === (right.user?.trim() ?? "")
      );
    case "docker":
      return right.kind === "docker" && left.container === right.container;
    case "server":
      return (
        right.kind === "server" &&
        normalizeServerRemoteUrlForComparison(left.url) ===
          normalizeServerRemoteUrlForComparison(right.url)
      );
  }
}

function buildRemoteTargetTelemetryKey(target: RemoteTarget): string {
  switch (target.kind) {
    case "ssh":
      return `ssh:${buildSshRemoteHostKey(target)}`;
    case "wsl":
      return `wsl:${target.distro?.trim() || "default"}\0${target.user?.trim() ?? ""}`;
    case "docker":
      return `docker:${target.container}`;
    case "server":
      return `server:${normalizeServerRemoteUrlForComparison(target.url)}`;
  }
}

function closeMessagePort(port: MessagePortMain | undefined): void {
  if (!port) return;
  try {
    port.close();
  } catch {
    // MessagePort 关闭失败不影响 Host 内 attachment 的 close/dispose 幂等收口。
  }
}

export function createRemoteWorkspaceSessionManager(options: {
  logger: {
    info: (...args: unknown[]) => void;
    warn: (...args: unknown[]) => void;
    error: (...args: unknown[]) => void;
  };
  windowHostProcessMap: Map<number, ElectronUtilityProcess>;
  resolveRemoteAssetDirs: () => RemoteAssetDirs;
  resolveWslTarget?: (
    target: Extract<RemoteTarget, { kind: "wsl" }>,
  ) => Promise<Extract<RemoteTarget, { kind: "wsl" }>>;
  createMessageChannel?: () => { port1: MessagePortMain; port2: MessagePortMain };
  rendererAttachmentReadyTimeoutMs?: number;
  reportRemoteConnectionStateChanged?: (params: {
    rendererId: number;
    remoteKind: RemoteTarget["kind"];
    transition: Exclude<RemoteGaugeTransition, "none">;
  }) => void;
  reportRemoteDisconnect?: (params: {
    rendererId: number;
    remoteKind: RemoteTarget["kind"];
    disconnectReason: RemoteDisconnectReason;
    durationMs: number;
  }) => void;
  monotonicNowMs?: () => number;
  providerProvisioningCoordinator?: ProviderProvisioningEnvironmentCoordinator;
}) {
  const pendingByRequestKey = new Map<string, PendingConnect>();
  const routesBySessionId = new Map<string, RemoteAttachmentRoute>();
  const listenedHosts = new WeakSet<ElectronUtilityProcess>();
  const reconnectsByWorkspaceKey = new Map<string, Promise<string>>();
  const pendingProviderProvisioningExecutions = new Map<
    string,
    PendingProviderProvisioningExecution
  >();
  const providerProvisioningCoordinator =
    options.providerProvisioningCoordinator ?? new ProviderProvisioningEnvironmentCoordinator();
  let appShutdownStarted = false;
  const monotonicNowMs = options.monotonicNowMs ?? (() => performance.now());

  function requestKey(webContentsId: number, requestId: string): string {
    return `${webContentsId}\0${requestId}`;
  }

  function getWindowHost(win: BrowserWindow): ElectronUtilityProcess {
    const child = options.windowHostProcessMap.get(win.webContents.id);
    if (!child || child.pid == null) {
      throw new Error(`未找到窗口 Local Host，windowId=${win.webContents.id}`);
    }
    ensureHostListener(child, win.webContents.id);
    return child;
  }

  function createMessageChannel() {
    return options.createMessageChannel?.() ?? new MessageChannelMain();
  }

  function emitConnectionLog(
    win: BrowserWindow,
    payload: {
      requestId?: string;
      sessionId?: string;
      level: "info" | "warn" | "error";
      message: string;
    },
  ): void {
    if (win.isDestroyed() || win.webContents.isDestroyed()) return;
    win.webContents.send(PlatformChannels.RemoteConnectionLog, {
      label: `window-host-${win.webContents.id}`,
      requestId: payload.requestId,
      sessionId: payload.sessionId,
      level: payload.level,
      source: "window-host-controller",
      message: payload.message,
      // 改造曾把完整 ISO 时间直接交给连接日志 UI，长时间戳会挤压 flex 日志列并换行。
      // 这里恢复既有的紧凑时钟展示契约，Host 的 requestId 路由和原始日志内容保持不变。
      timestamp: new Date().toLocaleTimeString(undefined, {
        hour12: false,
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }),
    });
  }

  function detachServicePort(
    child: ElectronUtilityProcess,
    attachmentId: string,
    reason: string,
  ): void {
    try {
      child.postMessage({
        type: HostMessageTypes.DetachServicePort,
        attachmentId,
      });
    } catch (error) {
      // 候选 port 的回收属于幂等清理，不能因 Host 已退出导致 ready promise 再次悬空。
      options.logger.warn("[window-host-remote] detach renderer attachment failed", {
        attachmentId,
        reason,
        error,
      });
    }
  }

  async function attachRendererPort(
    win: BrowserWindow,
    route: RemoteAttachmentRoute,
    reason: string,
  ): Promise<void> {
    if (win.isDestroyed() || win.webContents.isDestroyed()) {
      throw new Error("窗口已关闭，无法 attachment 远程 workspace");
    }
    const child = getWindowHost(win);
    const descriptor = route.descriptor;
    if (!descriptor.workspacePath || !descriptor.workspaceIdentity) {
      throw new Error(
        `远程 descriptor 缺少 workspace scope，sessionId=${descriptor.remoteSessionId}`,
      );
    }
    const { port1, port2 } = createMessageChannel();
    const attachmentId = randomUUID();
    const previousAttachmentId = route.rendererAttachmentId;
    const superseded = route.pendingRendererAttachment;
    if (superseded) {
      clearTimeout(superseded.timeout);
      route.pendingRendererAttachment = undefined;
      detachServicePort(child, superseded.attachmentId, "superseded");
      superseded.reject(
        new Error(`renderer attachment 已被后续换代替代，sessionId=${descriptor.remoteSessionId}`),
      );
    }

    return new Promise<void>((resolve, reject) => {
      const timeout = setTimeout(() => {
        const pending = route.pendingRendererAttachment;
        if (!pending || pending.attachmentId !== attachmentId) return;
        route.pendingRendererAttachment = undefined;
        detachServicePort(child, attachmentId, "ready-timeout");
        const error = new Error(
          `renderer attachment ready 超时，sessionId=${descriptor.remoteSessionId}`,
        );
        options.logger.warn("[window-host-remote] renderer attachment ready timeout", {
          sessionId: descriptor.remoteSessionId,
          attachmentId,
          reason,
        });
        reject(error);
      }, options.rendererAttachmentReadyTimeoutMs ?? 15_000);
      timeout.unref?.();
      route.pendingRendererAttachment = {
        attachmentId,
        previousAttachmentId,
        reason,
        timeout,
        resolve,
        reject,
      };

      try {
        child.postMessage(
          {
            type: HostMessageTypes.AttachServicePort,
            requestId: randomUUID(),
            attachmentId,
            clientMode: "desktop-continuous",
            scope: {
              kind: "remote",
              remoteSessionId: descriptor.remoteSessionId,
              workspacePath: descriptor.workspacePath,
              workspaceIdentity: descriptor.workspaceIdentity,
            },
          },
          [port2],
        );
        win.webContents.postMessage(
          InternalChannels.ScopedServicePort,
          {
            attachmentId,
            sessionId: descriptor.remoteSessionId,
            target: descriptor.target,
          },
          [port1],
        );
      } catch (error) {
        clearTimeout(timeout);
        route.pendingRendererAttachment = undefined;
        detachServicePort(child, attachmentId, "delivery-failed");
        closeMessagePort(port1);
        closeMessagePort(port2);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  function confirmRendererAttachmentReady(
    webContentsId: number,
    payload: { sessionId: string; attachmentId: string },
  ): void {
    const route = routesBySessionId.get(payload.sessionId);
    const pending = route?.pendingRendererAttachment;
    if (
      !route ||
      route.webContentsId !== webContentsId ||
      !pending ||
      pending.attachmentId !== payload.attachmentId
    ) {
      options.logger.warn("[window-host-remote] ignore stale renderer attachment ready", {
        webContentsId,
        sessionId: payload.sessionId,
        attachmentId: payload.attachmentId,
      });
      return;
    }

    clearTimeout(pending.timeout);
    route.pendingRendererAttachment = undefined;
    const child = options.windowHostProcessMap.get(route.webContentsId);
    if (!child || child.pid == null) {
      pending.reject(new Error(`窗口 Local Host 已退出，sessionId=${payload.sessionId}`));
      return;
    }
    route.rendererAttachmentId = pending.attachmentId;
    if (pending.previousAttachmentId) {
      // Host generation bind 会 fail-closed 立即失效 A；Main 仍需等 renderer 注册 B
      // 后再提升 route 并做幂等清理。reload 复挂没有 generation 换代，也因此不会提前拆掉可用旧 port。
      detachServicePort(child, pending.previousAttachmentId, "candidate-promoted");
    }
    options.logger.info(
      `[window-host-remote] renderer attachment ready, sessionId=${payload.sessionId}, reason=${pending.reason}`,
    );
    if (!route.connectFinalized) {
      route.connectFinalized = true;
      if (route.remoteUsageTelemetryEligible) {
        try {
          options.reportRemoteConnectionStateChanged?.({
            rendererId: route.webContentsId,
            remoteKind: route.descriptor.target.kind,
            transition: "connected",
          });
        } catch (error) {
          options.logger.warn("[remote-usage-arms] connected reporter failed", { error });
        }
      }
    }
    pending.resolve();
  }

  function handleConnected(
    child: ElectronUtilityProcess,
    webContentsId: number,
    requestId: string,
    descriptor: WindowHostRemoteWorkspaceDescriptor,
  ): void {
    const key = requestKey(webContentsId, requestId);
    const pending = pendingByRequestKey.get(key);
    if (!pending) {
      child.postMessage({
        type: HostMessageTypes.DisposeRemoteWorkspaceSession,
        requestId: randomUUID(),
        remoteSessionId: descriptor.remoteSessionId,
      });
      return;
    }
    pendingByRequestKey.delete(key);
    const route: RemoteAttachmentRoute = {
      webContentsId,
      descriptor,
      attachmentState: "attachable",
      connectedAtMonotonicMs: monotonicNowMs(),
      connectFinalized: false,
      remoteUsageTelemetryEligible: pending.remoteUsageTelemetryEligible,
    };
    routesBySessionId.set(descriptor.remoteSessionId, route);
    const environmentKey = buildRemoteEnvironmentKey(descriptor.target);
    const registration = providerProvisioningCoordinator.register(
      environmentKey,
      descriptor.remoteSessionId,
      (trigger) =>
        executeProviderProvisioning(child, descriptor.remoteSessionId, environmentKey, trigger),
    );
    route.providerProvisioningDispose = registration.dispose;
    void registration.initialSync
      .then(() => attachRendererPort(pending.win, route, "connect"))
      .then(() => {
        emitConnectionLog(pending.win, {
          requestId,
          sessionId: descriptor.remoteSessionId,
          level: "info",
          message: `远程 ${descriptor.target.kind} workspace 已连接`,
        });
        pending.resolve(descriptor.remoteSessionId);
      })
      .catch((error: unknown) => {
        route.providerProvisioningDispose?.();
        routesBySessionId.delete(descriptor.remoteSessionId);
        child.postMessage({
          type: HostMessageTypes.DisposeRemoteWorkspaceSession,
          requestId: randomUUID(),
          remoteSessionId: descriptor.remoteSessionId,
        });
        pending.reject(error instanceof Error ? error : new Error(String(error)));
      });
  }

  function executeProviderProvisioning(
    child: ElectronUtilityProcess,
    remoteSessionId: string,
    environmentKey: string,
    trigger: ProviderProvisioningTrigger,
  ): Promise<void> {
    const requestId = randomUUID();
    return new Promise<void>((resolve, reject) => {
      pendingProviderProvisioningExecutions.set(requestId, {
        child,
        trigger,
        startedAtMonotonicMs: monotonicNowMs(),
        resolve,
        reject,
      });
      try {
        child.postMessage({
          type: HostMessageTypes.ProviderProvisioningExecute,
          requestId,
          environmentKey,
          remoteSessionId,
          trigger,
        });
      } catch (error) {
        pendingProviderProvisioningExecutions.delete(requestId);
        reject(error instanceof Error ? error : new Error(String(error)));
      }
    });
  }

  function detachRouteAttachments(
    route: RemoteAttachmentRoute,
    reason: string,
    error: Error,
  ): void {
    const child = options.windowHostProcessMap.get(route.webContentsId);
    if (route.pendingRendererAttachment) {
      const pending = route.pendingRendererAttachment;
      clearTimeout(pending.timeout);
      route.pendingRendererAttachment = undefined;
      if (child) detachServicePort(child, pending.attachmentId, reason);
      pending.reject(error);
    }
    if (route.rendererAttachmentId) {
      if (child) detachServicePort(child, route.rendererAttachmentId, reason);
      route.rendererAttachmentId = undefined;
    }
  }

  function retireActiveRoute(
    route: RemoteAttachmentRoute,
    reason: RemoteDisconnectReason,
    mutate: () => void,
  ): void {
    const wasActive =
      route.remoteUsageTelemetryEligible &&
      route.attachmentState === "attachable" &&
      route.connectFinalized;
    mutate();
    if (!wasActive) return;

    const common = {
      rendererId: route.webContentsId,
      remoteKind: route.descriptor.target.kind,
    };
    try {
      options.reportRemoteConnectionStateChanged?.({ ...common, transition: reason });
    } catch (error) {
      // telemetry 是连接生命周期的旁路，不能因 reporter 异常回滚 route 退出状态。
      options.logger.warn("[remote-usage-arms] disconnect gauge reporter failed", { error });
    }
    try {
      options.reportRemoteDisconnect?.({
        ...common,
        disconnectReason: reason,
        durationMs: Math.max(0, Math.round(monotonicNowMs() - route.connectedAtMonotonicMs)),
      });
    } catch (error) {
      options.logger.warn("[remote-usage-arms] disconnect reporter failed", { error });
    }
  }

  function handleClosed(
    webContentsId: number,
    event: {
      remoteSessionId: string;
      reason: "connection-closed" | "disposed" | "connect-cancelled";
      exitCode?: number | null;
      signal?: string | null;
      error?: string;
    },
  ): void {
    const route = routesBySessionId.get(event.remoteSessionId);
    if (!route || route.webContentsId !== webContentsId) return;
    if (event.reason !== "connection-closed") {
      route.providerProvisioningDispose?.();
      retireActiveRoute(route, "disposed", () => {
        routesBySessionId.delete(event.remoteSessionId);
      });
      detachRouteAttachments(
        route,
        "session-released",
        new Error(`远程 workspace 已释放，sessionId=${event.remoteSessionId}`),
      );
      return;
    }
    retireActiveRoute(route, "connection-closed", () => {
      route.attachmentState = "closed";
    });
    route.providerProvisioningDispose?.();
    // 连接断开只把 route 标成 closed，已暴露的 desktop attachment 仍留在窗口 Host；
    // sessionId 换代后 Main 又会删除 route，导致旧 ChannelServer 永久失去回收入口。
    detachRouteAttachments(
      route,
      "session-connection-closed",
      new Error(`远程 workspace 已关闭，sessionId=${event.remoteSessionId}`),
    );
    const win = BrowserWindow.getAllWindows().find(
      (candidate) => candidate.webContents.id === webContentsId,
    );
    if (win && !win.isDestroyed() && !win.webContents.isDestroyed()) {
      win.webContents.send(PlatformChannels.RemoteSessionClosed, {
        sessionId: event.remoteSessionId,
        reason: "host-exit" as const,
        exitCode: event.exitCode ?? null,
        signal: event.signal ?? null,
      });
      emitConnectionLog(win, {
        sessionId: event.remoteSessionId,
        level: "warn",
        message: event.error || "远程 workspace 连接已断开",
      });
    }
  }

  function ensureHostListener(child: ElectronUtilityProcess, webContentsId: number): void {
    if (listenedHosts.has(child)) return;
    listenedHosts.add(child);
    child.on("message", (message: unknown) => {
      const parsed = hostResponseMessageSchema.safeParse(message);
      if (!parsed.success) return;
      if (parsed.data.type === HostResponseTypes.RemoteWorkspaceConnectionLog) {
        const pending = pendingByRequestKey.get(requestKey(webContentsId, parsed.data.requestId));
        if (!pending) return;
        // 多个远程连接共享 window Host，不能再从进程 label/stdout 猜日志归属；
        // Host 已带上连接 requestId，Main 只向对应发起窗口做薄转发。
        emitConnectionLog(pending.win, {
          requestId: parsed.data.requestId,
          level: parsed.data.level,
          message: parsed.data.message,
        });
        return;
      }
      if (parsed.data.type === HostResponseTypes.ProviderProvisioningSourceChanged) {
        void providerProvisioningCoordinator.requestAll(parsed.data.trigger);
        return;
      }
      if (parsed.data.type === HostResponseTypes.ProviderProvisioningExecutionResult) {
        const pending = pendingProviderProvisioningExecutions.get(parsed.data.requestId);
        if (!pending || pending.child !== child) return;
        pendingProviderProvisioningExecutions.delete(parsed.data.requestId);
        const logContext = {
          environmentKey: parsed.data.environmentKey,
          trigger: pending.trigger,
          status: parsed.data.status,
          durationMs: Math.max(0, monotonicNowMs() - pending.startedAtMonotonicMs),
        };
        if (parsed.data.status !== "applied" && parsed.data.status !== "already-applied") {
          if (pending.trigger === "environment-online") {
            pending.reject(new Error(`Provider Provisioning 首次同步失败 (${parsed.data.status})`));
            return;
          }
          // Target 错误可能来自任意远端实现并携带请求材料；过渡期只记录可定位的状态事实，
          // 不转抄不可证明已脱敏的自由文本，避免 Provisioning 日志成为凭据泄露入口。
          options.logger.warn("[provider-provisioning] Environment sync did not apply", logContext);
        } else {
          options.logger.info("[provider-provisioning] Environment sync completed", logContext);
        }
        pending.resolve();
        return;
      }
      if (parsed.data.type === HostResponseTypes.RemoteWorkspaceConnected) {
        handleConnected(child, webContentsId, parsed.data.requestId, parsed.data.descriptor);
        return;
      }
      if (parsed.data.type === HostResponseTypes.RemoteWorkspaceConnectFailed) {
        const key = requestKey(webContentsId, parsed.data.requestId);
        const pending = pendingByRequestKey.get(key);
        if (!pending) return;
        pendingByRequestKey.delete(key);
        emitConnectionLog(pending.win, {
          requestId: parsed.data.requestId,
          level: "error",
          message: parsed.data.error,
        });
        pending.reject(new Error(parsed.data.error));
        return;
      }
      if (parsed.data.type === HostResponseTypes.RemoteWorkspaceClosed) {
        handleClosed(webContentsId, parsed.data);
      }
    });
    child.once("exit", () => {
      const error = new Error("窗口 Local Host 已退出");
      for (const [requestId, pendingExecution] of pendingProviderProvisioningExecutions) {
        if (pendingExecution.child !== child) continue;
        pendingProviderProvisioningExecutions.delete(requestId);
        pendingExecution.reject(error);
      }
      for (const [key, pending] of Array.from(pendingByRequestKey)) {
        if (pending.webContentsId === webContentsId) {
          pendingByRequestKey.delete(key);
          pending.reject(error);
        }
      }
      for (const [sessionId, route] of Array.from(routesBySessionId)) {
        if (route.webContentsId !== webContentsId) continue;
        route.providerProvisioningDispose?.();
        retireActiveRoute(route, "host-exit", () => {
          routesBySessionId.delete(sessionId);
        });
        if (route.pendingRendererAttachment) {
          clearTimeout(route.pendingRendererAttachment.timeout);
          route.pendingRendererAttachment.reject(error);
        }
      }
    });
  }

  async function createRemoteWorkspaceSession(
    win: BrowserWindow,
    target: RemoteTarget,
    requestId?: string,
    context?: RemoteWorkspaceSessionContext,
    lifecycle?: { remoteUsageTelemetryEligible?: boolean },
  ): Promise<string> {
    if (appShutdownStarted) {
      throw new Error("应用正在退出，无法创建远程工作区连接");
    }
    const child = getWindowHost(win);
    const resolvedTarget =
      target.kind === "wsl" && options.resolveWslTarget
        ? await options.resolveWslTarget(target)
        : target;
    if (appShutdownStarted) {
      // WSL identity 解析跨 await，期间退出屏障可能已清空 Main 请求关联并开始回收 Host。
      // 恢复后必须重新验证生命周期，禁止在 shutdown barrier 之后注册迟到请求。
      throw new Error("应用正在退出，无法创建远程工作区连接");
    }
    if (win.isDestroyed() || win.webContents.isDestroyed()) {
      throw new Error("窗口已关闭，无法创建远程工作区连接");
    }
    const resolvedRequestId = requestId ?? randomUUID();
    const key = requestKey(win.webContents.id, resolvedRequestId);
    if (pendingByRequestKey.has(key)) {
      throw new Error(`远程连接 requestId 重复，requestId=${resolvedRequestId}`);
    }
    emitConnectionLog(win, {
      requestId: resolvedRequestId,
      level: "info",
      message: `正在通过窗口 Host 连接 ${resolvedTarget.kind} workspace`,
    });
    return new Promise<string>((resolve, reject) => {
      pendingByRequestKey.set(key, {
        requestId: resolvedRequestId,
        webContentsId: win.webContents.id,
        win,
        remoteUsageTelemetryEligible: lifecycle?.remoteUsageTelemetryEligible ?? false,
        resolve,
        reject,
      });
      child.postMessage({
        type: HostMessageTypes.ConnectRemoteWorkspace,
        requestId: resolvedRequestId,
        target: resolvedTarget,
        remoteAssets: options.resolveRemoteAssetDirs(),
        ...(context?.workspacePath ? { workspacePath: context.workspacePath } : {}),
        ...(context?.workspaceIdentity ? { workspaceIdentity: context.workspaceIdentity } : {}),
      });
    });
  }

  async function bindRemoteWorkspaceSessionContext(
    sessionId: string,
    context: RemoteWorkspaceSessionContext,
    expectedWebContentsId?: number,
  ): Promise<void> {
    const route = routesBySessionId.get(sessionId);
    if (!route) {
      if (expectedWebContentsId != null) {
        throw new Error(`未找到远程 workspace session，sessionId=${sessionId}`);
      }
      return;
    }
    if (expectedWebContentsId != null && route.webContentsId !== expectedWebContentsId) {
      throw new Error(`远程 workspace session 不属于当前窗口，sessionId=${sessionId}`);
    }
    const workspaceIdentity =
      context.workspaceIdentity?.trim() ||
      buildRemoteWorkspaceIdentity(context.workspacePath, route.descriptor.target);
    const child = options.windowHostProcessMap.get(route.webContentsId);
    const win = BrowserWindow.getAllWindows().find(
      (candidate) => candidate.webContents.id === route.webContentsId,
    );
    if (!child || !win) {
      throw new Error(`未找到远程 workspace 所属窗口 Host，sessionId=${sessionId}`);
    }
    child.postMessage({
      type: HostMessageTypes.BindRemoteWorkspaceContext,
      requestId: randomUUID(),
      remoteSessionId: sessionId,
      workspacePath: context.workspacePath,
      workspaceIdentity,
    });
    route.descriptor = {
      ...route.descriptor,
      workspacePath: context.workspacePath,
      workspaceIdentity,
      generation: route.descriptor.generation + 1,
    };
    // 同一 parentPort 上 Bind 先于 Attach 处理；IPC 只在 renderer 注册新 port 后返回。
    await attachRendererPort(win, route, "workspace-context-bound");
  }

  function reattachRemoteWorkspaceSessionsForWindow(win: BrowserWindow, reason: string): void {
    for (const route of routesBySessionId.values()) {
      if (route.webContentsId === win.webContents.id && route.attachmentState === "attachable") {
        void attachRendererPort(win, route, reason).catch((error: unknown) => {
          options.logger.warn("[window-host-remote] renderer reattach failed", {
            sessionId: route.descriptor.remoteSessionId,
            reason,
            error,
          });
        });
      }
    }
  }

  function getRemoteConnectionStats(): RemoteConnectionStats {
    const activeRoutes = Array.from(routesBySessionId.values()).filter(
      (route) =>
        route.remoteUsageTelemetryEligible &&
        route.attachmentState === "attachable" &&
        route.connectFinalized,
    );
    return {
      activeSessionCount: activeRoutes.length,
      activeTargetCount: new Set(
        activeRoutes.map((route) => buildRemoteTargetTelemetryKey(route.descriptor.target)),
      ).size,
    };
  }

  function disposeRemoteWorkspaceSession(sessionId: string, _reason?: string): void {
    const route = routesBySessionId.get(sessionId);
    if (!route) return;
    retireActiveRoute(route, "disposed", () => {
      routesBySessionId.delete(sessionId);
    });
    route.providerProvisioningDispose?.();
    const child = options.windowHostProcessMap.get(route.webContentsId);
    if (route.pendingRendererAttachment) {
      const pending = route.pendingRendererAttachment;
      clearTimeout(pending.timeout);
      route.pendingRendererAttachment = undefined;
      if (child) detachServicePort(child, pending.attachmentId, "session-disposed");
      pending.reject(new Error(`远程 workspace session 已释放，sessionId=${sessionId}`));
    }
    if (!child) return;
    if (route.rendererAttachmentId) {
      child.postMessage({
        type: HostMessageTypes.DetachServicePort,
        attachmentId: route.rendererAttachmentId,
      });
    }
    child.postMessage({
      type: HostMessageTypes.DisposeRemoteWorkspaceSession,
      requestId: randomUUID(),
      remoteSessionId: sessionId,
    });
  }

  function disposeRemoteWorkspaceSessionsForWindow(webContentsId: number): void {
    for (const [sessionId, route] of Array.from(routesBySessionId)) {
      if (route.webContentsId !== webContentsId) continue;
      retireActiveRoute(route, "window-closed", () => {
        routesBySessionId.delete(sessionId);
      });
      route.providerProvisioningDispose?.();
      if (route.pendingRendererAttachment) {
        clearTimeout(route.pendingRendererAttachment.timeout);
        route.pendingRendererAttachment.reject(new Error("窗口已关闭，attachment 已取消"));
      }
    }
    for (const [key, pending] of Array.from(pendingByRequestKey)) {
      if (pending.webContentsId !== webContentsId) continue;
      pendingByRequestKey.delete(key);
      pending.reject(new Error("窗口已关闭，远程连接已取消"));
    }
  }

  function cancelPendingRemoteWorkspaceSessionsForWindow(
    webContentsId: number,
    _reason: string,
    requestId?: string,
  ): void {
    const child = options.windowHostProcessMap.get(webContentsId);
    if (!child) return;
    for (const pending of pendingByRequestKey.values()) {
      if (
        pending.webContentsId === webContentsId &&
        (!requestId || pending.requestId === requestId)
      ) {
        child.postMessage({
          type: HostMessageTypes.CancelRemoteWorkspaceConnect,
          requestId: pending.requestId,
        });
      }
    }
  }

  function hasRemoteWorkspaceSessionForTarget(
    win: BrowserWindow,
    target: RemoteTarget,
    context?: RemoteWorkspaceSessionContext,
  ): boolean {
    const workspaceKey = context ? resolveWorkspaceKey(context) : undefined;
    return Array.from(routesBySessionId.values()).some(
      (route) =>
        route.webContentsId === win.webContents.id &&
        route.attachmentState === "attachable" &&
        isSameRemoteTarget(route.descriptor.target, target) &&
        (!workspaceKey ||
          resolveWorkspaceKey({
            workspacePath: route.descriptor.workspacePath ?? "",
            workspaceIdentity: route.descriptor.workspaceIdentity,
          }) === workspaceKey),
    );
  }

  function attachRemoteWorkspaceSessionHost(params: {
    windowId: number;
    remoteSessionId: string;
    workspacePath: string;
    workspaceIdentity: string;
    workspaceKey: string;
    clientMode: "web-remote-replayable";
  }): {
    process: ElectronUtilityProcess;
    port: MessagePortMain;
    remoteKind: RemoteTarget["kind"];
  } {
    const route = routesBySessionId.get(params.remoteSessionId);
    if (!route) {
      throw Object.assign(
        new Error(`未找到远程 workspace session，sessionId=${params.remoteSessionId}`),
        {
          code: "REMOTE_SESSION_MISSING" as const,
        },
      );
    }
    if (route.attachmentState !== "attachable") {
      throw Object.assign(new Error("远程 workspace source 当前离线"), {
        code: "REMOTE_SESSION_OFFLINE" as const,
      });
    }
    const win = BrowserWindow.fromId(params.windowId);
    if (!win || win.webContents.id !== route.webContentsId) {
      throw Object.assign(new Error("远程 workspace session 不属于当前窗口"), {
        code: "REMOTE_SESSION_WINDOW_MISMATCH" as const,
      });
    }
    const descriptor = route.descriptor;
    if (
      descriptor.workspacePath !== params.workspacePath ||
      descriptor.workspaceIdentity !== params.workspaceIdentity ||
      params.workspaceKey !== params.workspaceIdentity
    ) {
      throw Object.assign(new Error("远程 workspaceKey 与 logical session 不匹配。"), {
        code: "REMOTE_WORKSPACE_IDENTITY_MISMATCH" as const,
      });
    }
    const process = getWindowHost(win);
    const { port1, port2 } = createMessageChannel();
    process.postMessage(
      {
        type: HostMessageTypes.AttachServicePort,
        requestId: randomUUID(),
        attachmentId: randomUUID(),
        clientMode: params.clientMode,
        scope: {
          kind: "remote",
          remoteSessionId: params.remoteSessionId,
          workspacePath: params.workspacePath,
          workspaceIdentity: params.workspaceIdentity,
        },
      },
      [port2],
    );
    return { process, port: port1, remoteKind: descriptor.target.kind };
  }

  function reconnectBotRemoteWorkspaceSession(
    win: BrowserWindow,
    params: {
      target: RemoteTarget;
      workspacePath: string;
      workspaceIdentity: string;
      requestId?: string;
    },
  ): Promise<string> {
    const existing = Array.from(routesBySessionId.entries()).find(([, route]) => {
      return (
        route.webContentsId === win.webContents.id &&
        route.attachmentState === "attachable" &&
        route.descriptor.workspacePath === params.workspacePath &&
        route.descriptor.workspaceIdentity === params.workspaceIdentity &&
        isSameRemoteTarget(route.descriptor.target, params.target)
      );
    });
    if (existing) return Promise.resolve(existing[0]);
    const key = `${win.webContents.id}\0${params.workspaceIdentity}`;
    const pending = reconnectsByWorkspaceKey.get(key);
    if (pending) return pending;
    const reconnect = createRemoteWorkspaceSession(win, params.target, params.requestId, params);
    reconnectsByWorkspaceKey.set(key, reconnect);
    void reconnect.finally(() => {
      if (reconnectsByWorkspaceKey.get(key) === reconnect) reconnectsByWorkspaceKey.delete(key);
    });
    return reconnect;
  }

  async function createBotRemoteWorkspaceRuntimePort(
    win: BrowserWindow,
    params: { target: RemoteTarget; workspacePath: string; workspaceIdentity: string },
    _requestId?: string,
  ): Promise<MessagePortMain> {
    const routeEntry = Array.from(routesBySessionId.entries()).find(([, route]) => {
      return (
        route.webContentsId === win.webContents.id &&
        route.attachmentState === "attachable" &&
        route.descriptor.workspacePath === params.workspacePath &&
        route.descriptor.workspaceIdentity === params.workspaceIdentity &&
        isSameRemoteTarget(route.descriptor.target, params.target)
      );
    });
    if (!routeEntry) {
      throw new Error("未找到可供 Bot attachment 的远程 logical session");
    }
    return attachRemoteWorkspaceSessionHost({
      windowId: win.id,
      remoteSessionId: routeEntry[0],
      workspacePath: params.workspacePath,
      workspaceIdentity: params.workspaceIdentity,
      workspaceKey: params.workspaceIdentity,
      clientMode: "web-remote-replayable",
    }).port;
  }

  return {
    createRemoteWorkspaceSession,
    attachRemoteWorkspaceSessionHost,
    reconnectBotRemoteWorkspaceSession,
    bindRemoteWorkspaceSessionContext,
    confirmRendererAttachmentReady,
    reattachRemoteWorkspaceSessionsForWindow,
    hasRemoteWorkspaceSessionForTarget,
    createBotRemoteWorkspaceRuntimePort,
    getRemoteConnectionStats,
    disposeRemoteWorkspaceSession,
    disposeRemoteWorkspaceSessionsForWindow,
    disposeAllAndWaitForAppShutdown: async (_reason: string) => {
      appShutdownStarted = true;
      const error = new Error("应用正在退出，远程连接已取消");
      for (const pending of pendingByRequestKey.values()) pending.reject(error);
      pendingByRequestKey.clear();
      for (const [sessionId, route] of Array.from(routesBySessionId)) {
        retireActiveRoute(route, "app-shutdown", () => {
          routesBySessionId.delete(sessionId);
        });
        route.providerProvisioningDispose?.();
        if (!route.pendingRendererAttachment) continue;
        clearTimeout(route.pendingRendererAttachment.timeout);
        route.pendingRendererAttachment.reject(error);
      }
      routesBySessionId.clear();
    },
    cancelPendingRemoteWorkspaceSessionsForWindow,
    handleWorkspaceRunningTaskCountChanged: () => {
      // Running-task 事实现在由窗口 Host registry/ControllerProjection 持有；Main 不再维护 WSL pool。
    },
  };
}
