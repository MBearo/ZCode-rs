import type {
  DesktopCommandId,
  DesktopZoomState,
  DesktopWindowChromeState,
  DesktopTitleBarTheme,
  CuaAccessibilitySettingsResult,
  OpenCuaPermissionOnboardingOptions,
  PrepareCuaHelperPermissionDragResult,
  AppSettings,
  BrowserViewOperationPayload,
  BrowserGuestAttachResult,
  BrowserViewScreenshotSurfacePreparePayload,
  BrowserViewScreenshotSurfaceReadyPayload,
  BrowserViewScreenshotSurfaceReleasePayload,
  BrowserViewViewportChangedPayload,
  BrowserViewCloseTabNotification,
  BrowserViewCloseTabRequest,
  BrowserViewResidencyReportPayload,
  BrowserViewResidencyTransitionPayload,
  BrowserViewRestoredTabShell,
  BrowserViewRestoreTabsRequest,
  BrowserViewportSize,
  ChromeBrowserDataImportResult,
  DockerContainerInfo,
  EmbeddedBrowserOpenUrlRequest,
  EditorInfo,
  ApplicationIconInfo,
  ApplicationIconRequest,
  Locale,
  OAuthStateRegistration,
  PostUpdateReleaseNotesPayload,
  RemoteConnectionRuntimeLog,
  RemoteSessionClosedEvent,
  BotRemoteWorkspaceReconnectedEvent,
  RemoteTarget,
  SSHConfigAliasOption,
  RendererTelemetryEventPayload,
  RendererActionTraceBatchV1,
  RendererActionTraceConfigV1,
  RendererHeapSample,
  TelemetryRendererContext,
  TaskNotificationPayload,
  WindowScreenshotResult,
  EmbeddedBrowserDataClearResult,
  WSLDistro,
  UpdateCheckResultPayload,
  UpdateStatePayload,
  OpenInEditorOptions,
} from "@zcode/shared";

/**
 * window.zcode 类型定义 —— 仅包含需要 main 进程参与的平台操作
 *
 * 凭据管理已迁移到 ICredentialService（通过 RPC），不再经过此接口。
 */
declare global {
  interface Window {
    zcode: {
      connectRemote(
        options: RemoteTarget,
        requestId?: string,
        context?: {
          workspacePath: string;
          workspaceIdentity?: string;
          connectTrigger?: import("@zcode/shared").RemoteWorkspaceConnectTrigger;
        },
      ): Promise<{ success: boolean; error?: string; sessionId?: string }>;
      /** 取消当前窗口尚未建立完成的远程连接 */
      cancelPendingRemoteConnection?(requestId?: string): Promise<void>;
      /** 绑定远程 logical session 的 canonical workspace context */
      bindRemoteWorkspaceSessionContext?(context: {
        remoteSessionId: string;
        workspacePath: string;
        workspaceIdentity?: string;
      }): Promise<BrowserGuestAttachResult>;
      /** 释放当前窗口里的远程 session */
      disposeRemoteSession(sessionId: string): Promise<void>;
      /** 检查本机 Docker daemon 是否可用 */
      isDockerAvailable(): Promise<boolean>;
      /** 列出本机可用的 WSL 发行版 */
      listWSLDistros(): Promise<WSLDistro[]>;
      /** 列出当前可连接的 Docker 容器 */
      listDockerContainers(): Promise<DockerContainerInfo[]>;
      /** 列出当前机器 SSH config 里可用于快速填表的 alias */
      listSSHConfigAliases(): Promise<SSHConfigAliasOption[]>;
      /** renderer 日志通过 IPC 传到 main 进程统一存储 */
      log(level: "info" | "warn" | "error", args: unknown[]): void;
      /** 打开系统目录选择框，返回选中路径或 null */
      selectDirectory(): Promise<string | null>;
      /** 打开系统文件选择框，返回选中文件路径或 null */
      selectFile(): Promise<string | null>;
      /** 打开系统多文件选择框，返回选中文件路径；取消时返回空数组 */
      selectFiles?(): Promise<string[]>;
      /** 通过系统原生另存为对话框保存文件 */
      saveFile?(
        payload: import("@zcode/shared").SaveFileRequest,
      ): Promise<import("@zcode/shared").SaveFileResult>;
      /** 将当前页面的 print 媒体版面导出为 PDF（Chromium 打印引擎，矢量文本） */
      printPageToPdf?(): Promise<import("@zcode/shared").PrintPageToPdfResult>;
      /** 从系统拖拽/文件输入得到的 Web File 解析真实本地路径 */
      getPathForFile?(file: File): string | null;
      /** 订阅当前窗口内远程连接过程日志，返回 disposer */
      onRemoteConnectionLog(handler: (entry: RemoteConnectionRuntimeLog) => void): () => void;
      /** 订阅远程 workspace session 关闭事件，返回 disposer */
      onRemoteSessionClosed(handler: (event: RemoteSessionClosedEvent) => void): () => void;
      /** 订阅 Bot 触发的远程 workspace 重连成功事件，返回 disposer */
      onBotRemoteWorkspaceReconnected(
        handler: (event: BotRemoteWorkspaceReconnectedEvent) => void,
      ): () => void;
      /** 检查目录是否已在其他窗口打开 */
      activateOrSetWorkspace?(path: string): Promise<{ activated: boolean }>;
      /** 同步当前窗口所有 tab 的 workspace 路径到 main 进程 */
      syncWindowTabs(paths: string[]): void;
      /** 同步当前窗口的未读 task 数到 main 进程 */
      syncWindowUnreadCount(count: number): void;
      syncActiveTaskSession(sessionId: string | null): void;
      /** 同步需要 main 进程即时感知的应用设置 */
      syncAppSettings?(patch: Partial<AppSettings>): void;
      /** 注册 main 进程要求聚焦指定 workspace tab 的回调，返回 disposer */
      onFocusTab(handler: (path: string) => void): () => void;
      /** 注册 main 进程触发新建 tab 的回调，返回 disposer */
      onNewTab(handler: () => void): () => void;
      /** 注册内置浏览器 webview 请求打开新页面的回调，返回 disposer */
      onOpenBrowserUrl?(handler: (request: EmbeddedBrowserOpenUrlRequest) => void): () => void;
      onBrowserViewReady?(
        handler: (payload: {
          workspaceKey: string;
          remoteSessionId?: string;
          sessionId: string;
          tabId: string;
          browserId: string;
          browserGeneration: number;
        }) => void,
      ): () => void;
      onBrowserViewOperation?(handler: (payload: BrowserViewOperationPayload) => void): () => void;
      onBrowserViewVisibility?(
        handler: (payload: {
          visible: boolean;
          workspaceKey: string;
          remoteSessionId: string | undefined;
          sessionId: string;
          tabId?: string;
          browserId: string;
          browserGeneration: number;
        }) => void,
      ): () => void;
      onBrowserViewViewportChanged?(
        handler: (payload: BrowserViewViewportChangedPayload) => void,
      ): () => void;
      onBrowserViewScreenshotSurfacePrepare?(
        handler: (payload: BrowserViewScreenshotSurfacePreparePayload) => void,
      ): () => void;
      onBrowserViewScreenshotSurfaceRelease?(
        handler: (payload: BrowserViewScreenshotSurfaceReleasePayload) => void,
      ): () => void;
      browserViewScreenshotSurfaceReady?(payload: BrowserViewScreenshotSurfaceReadyPayload): void;
      onBrowserViewCloseTab?(
        handler: (payload: BrowserViewCloseTabNotification) => void,
      ): () => void;
      onBrowserViewSuspend?(
        handler: (payload: BrowserViewResidencyTransitionPayload) => void,
      ): () => void;
      onBrowserViewRestore?(
        handler: (payload: BrowserViewResidencyTransitionPayload) => void,
      ): () => void;
      /** 注册 main 进程触发新建任务的回调，返回 disposer */
      onNewTask(handler: () => void): () => void;
      /** 注册 main 进程触发打开工作区的回调，返回 disposer */
      onOpenWorkspace?(handler: () => void): () => void;
      /** 注册 main 进程通过 deep link 直接打开本地工作区目录的回调，返回 disposer */
      onOpenWorkspacePath?(handler: (path: string) => void): () => void;
      /** 注册窗口全屏状态变化回调，返回 disposer */
      onWindowFullscreenChanged(handler: (isFullscreen: boolean) => void): () => void;
      /** 读取窗口最大化状态与系统原生圆角能力 */
      getDesktopWindowChromeState?(): Promise<DesktopWindowChromeState>;
      /** 订阅窗口最大化状态与系统原生圆角能力变化 */
      onDesktopWindowChromeStateChanged?(
        handler: (state: DesktopWindowChromeState) => void,
      ): () => void;
      /** 读取当前桌面窗口页面缩放档位 */
      getDesktopZoomLevel?(): Promise<DesktopZoomState>;
      /** 订阅当前桌面窗口页面缩放档位变化 */
      onDesktopZoomLevelChanged?(handler: (state: DesktopZoomState) => void): () => void;
      /** 注册用户点击系统通知后跳转到对应任务的回调，返回 disposer */
      onTaskNotificationClick(handler: (taskId: string) => void): () => void;
      /** 打开外部 URL */
      openExternal(url: string): void;
      /** 查询当前语言下是否存在可用的用户社群入口 */
      canOpenCommunity(locale: Locale): Promise<boolean>;
      /** 在系统文件管理器中打开指定路径 */
      openInFileManager(path: string): Promise<{ success: boolean; error?: string }>;
      /** 使用系统默认应用打开本地文件 */
      openExternalFile(path: string): Promise<{ success: boolean; error?: string }>;
      /** 打开 ZCode Computer Use 完整权限引导 */
      openCuaPermissionOnboarding?(
        options?: OpenCuaPermissionOnboardingOptions,
      ): Promise<CuaAccessibilitySettingsResult>;
      /** 只取消当前 renderer 以 operationId 发起的 onboarding participant。 */
      cancelCuaPermissionOnboarding?(operationId: string): void;
      /** 预热并缓存已验证的 Helper 路径，使 dragstart 能同步 startDrag */
      prepareCuaHelperPermissionDrag?(): Promise<PrepareCuaHelperPermissionDragResult>;
      /** 从权限浮窗拖拽 Helper.app 到 macOS 权限列表 */
      startCuaHelperPermissionDrag?(): void;
      /** 上报 OAuth state 用于 deep link 路由 */
      registerOAuthState(payload: OAuthStateRegistration): void;
      /** 注册 OAuth deep link 回调，返回 disposer */
      onOAuthCallback(cb: (url: string) => void): () => void;
      /** 注册支付 deep link 回调，返回 disposer */
      onPaymentCallback(cb: (url: string) => void): () => void;
      /** 通知 main process renderer 已就绪 */
      notifyRendererReady(): void;
      /** 同步当前 renderer 的 telemetry 上下文到 main process */
      syncTelemetryContext(context: TelemetryRendererContext): void;
      /** 通过 main process 统一上报业务 telemetry 事件 */
      reportTelemetryEvent(payload: RendererTelemetryEventPayload): Promise<void>;
      /** 读取 Desktop Renderer 用户操作 Trace 灰度配置。 */
      getRendererActionTraceConfig?(): Promise<RendererActionTraceConfigV1>;
      /** 订阅 Renderer 用户操作 Trace 灰度配置变化。 */
      onRendererActionTraceConfigChanged?(
        callback: (config: RendererActionTraceConfigV1) => void,
      ): () => void;
      /** 发送已结束的 ui_action batch；Main 不返回业务结果。 */
      reportRendererActionTraceBatch?(batch: RendererActionTraceBatchV1): void;
      /** 主窗口 renderer 的 60 秒 heap 读数；单向 send，Main 不回执。 */
      reportRendererHeapSample?(sample: RendererHeapSample): void;
      /** 触发任务状态对应的系统通知 */
      showTaskNotification(payload: TaskNotificationPayload): void;
      /** 导出日志：打包 ~/.zcode/v2 及外部 agent 日志为 zip 并在 Finder 中显示 */
      exportLogs(): Promise<{
        success: boolean;
        path?: string;
        error?: string;
      }>;
      /** 截取当前窗口，用于错误反馈携带现场画面 */
      captureWindowScreenshot?(): Promise<WindowScreenshotResult | null>;
      browserViewAttachGuest?(payload: {
        key: string;
        webContentsId: number;
        active?: boolean;
        workspaceKey?: string;
        remoteSessionId?: string;
        sessionId?: string;
        residencyGeneration?: number;
      }): Promise<void>;
      /** 重建 `<webview>` 前让 main 精确断开旧 guest 的 CDP。 */
      browserViewDetachGuest?(payload: { key: string; webContentsId: number }): Promise<boolean>;
      browserViewCloseTab?(payload: BrowserViewCloseTabRequest): Promise<void>;
      browserViewReportResidency?(payload: BrowserViewResidencyReportPayload): Promise<void>;
      browserViewSuspendReady?(payload: { tabId: string; generation: number }): Promise<void>;
      browserViewEnsureResident?(payload: BrowserViewCloseTabRequest): Promise<void>;
      browserViewRestoreTabs?(
        payload: BrowserViewRestoreTabsRequest,
      ): Promise<BrowserViewRestoredTabShell[]>;
      /** renderer 自由尺寸回写 main。 */
      browserViewUpdateViewport?(payload: {
        tabId: string;
        viewport: BrowserViewportSize | null;
      }): Promise<void>;
      /** 从自动发现的 Chrome Profile 一次性导入内置浏览器数据。 */
      importChromeBrowserData?(
        options?: import("@zcode/shared").ChromeBrowserDataImportOptions,
      ): Promise<ChromeBrowserDataImportResult>;
      /** 清理内置浏览器缓存或全部站点数据。 */
      clearEmbeddedBrowserData?(mode: "cache" | "all"): Promise<EmbeddedBrowserDataClearResult>;
      /** 注册新版本已下载完毕的回调，返回 disposer */
      onUpdateReady(callback: (version: string) => void): () => void;
      /** 注册"手动检查更新"结果的回调，返回 disposer */
      onUpdateCheckResult(callback: (payload: UpdateCheckResultPayload) => void): () => void;
      /** 注册自动更新持续状态变化，返回 disposer */
      onUpdateStateChanged?(callback: (payload: UpdateStatePayload) => void): () => void;
      /** 主动读取当前自动更新状态 */
      getUpdateState?(): Promise<UpdateStatePayload>;
      /** 开始下载当前已发现的更新 */
      downloadUpdate?(): Promise<void>;
      /** 取消当前正在下载的更新 */
      cancelUpdateDownload?(): Promise<void>;
      /** 打开或聚焦独立更新窗口 */
      openUpdateStatusWindow?(): Promise<void>;
      /** 读取自动更新偏好 */
      getAutoUpdatePreferences?(): Promise<{
        autoDownloadAndInstallUpdates: boolean;
      }>;
      /** 写入“自动下载并安装更新”偏好 */
      setAutoDownloadAndInstallUpdates?(enabled: boolean): Promise<void>;
      /** 跳过当前已发现的更新版本 */
      skipUpdateVersion?(version: string): Promise<void>;
      /** 注册应用语言变化，返回 disposer */
      onApplicationLocaleChanged?(callback: (locale: Locale) => void): () => void;
      /** 订阅 main 进程修改 settings 后的通知 */
      onSettingsChanged?(callback: () => void): () => void;
      /** 查询桌面端正在运行的会话数量 */
      getDesktopSessionActivity?(): Promise<{
        runningAgentSessionCount: number;
      }>;
      /** 注册更新安装后的版本说明，返回 disposer */
      onPostUpdateReleaseNotes(
        callback: (payload: PostUpdateReleaseNotesPayload) => void,
      ): () => void;
      /** 标记当前版本说明已读 */
      acknowledgePostUpdateReleaseNotes(version: string): Promise<void>;
      /** 用户确认重启安装更新 */
      quitAndInstallUpdate(): Promise<void>;
      /** 获取已安装的编辑器/终端列表（含图标） */
      getInstalledEditors(): Promise<EditorInfo[]>;
      /** 按兼容 bundle id 或结构化 locator 获取系统应用图标 */
      getApplicationIcon?(
        request: string | ApplicationIconRequest,
      ): Promise<ApplicationIconInfo | null>;
      /** 用指定编辑器打开路径 */
      openInEditor(
        editorId: string,
        path: string,
        options?: OpenInEditorOptions,
      ): Promise<{ success: boolean; error?: string }>;
      /** 执行桌面窗口级命令 */
      executeDesktopCommand(command: DesktopCommandId): Promise<void>;
      /** 同步应用菜单语言 */
      setApplicationLocale(locale: Locale): Promise<void>;
      /** 读取宿主系统语言 */
      getSystemLocale?(): Promise<Locale>;
      /** 同步标题栏亮暗色 */
      setTitleBarTheme(theme: DesktopTitleBarTheme): Promise<void>;
    };
  }
}
