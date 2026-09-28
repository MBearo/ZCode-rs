// sessions-index fan-out 的高频事件节流。
// 纯调度：窗口状态与定时器在这里，publish 由调用方注入，网关只保留调用点。
//
// Workflow 进度事件会更新 record.updatedAt；若每条都立即发布摘要，Host 和 renderer 会按
// 引擎事件频率重算任务列表。因此合并窗口内的进度更新，控制任务索引的发布频率。

/** 工作流进度事件的 fan-out 窗口：窗内的进度合并为窗末一次发布（侧栏运行行 ≤4Hz）。 */
export const WORKFLOW_PROGRESS_INDEX_FANOUT_MS = 250;

/** 定时器句柄对调度逻辑不透明：默认使用 setTimeout，也允许调用方提供实现。 */
export type FanoutTimerHandle = unknown;

export interface SessionsIndexFanoutThrottleOptions {
  /** 发布某会话当前摘要到 sessions-index（网关的 publishCurrentSummaryToIndex）。 */
  publish: (sessionId: string) => void;
  windowMs?: number;
  setTimer?: (callback: () => void, delayMs: number) => FanoutTimerHandle;
  clearTimer?: (handle: FanoutTimerHandle) => void;
}

interface WindowState {
  /** 窗内是否还有未发布的进度；被任意即时发布满足后回落为 false。 */
  pending: boolean;
  handle: FanoutTimerHandle;
}

function defaultSetTimer(callback: () => void, delayMs: number): FanoutTimerHandle {
  const timer = setTimeout(callback, delayMs);
  // CLI 进程退出不被节流窗口挂住（与 scheduleFlush 同一姿态）。
  timer.unref?.();
  return timer;
}

function defaultClearTimer(handle: FanoutTimerHandle): void {
  clearTimeout(handle as ReturnType<typeof setTimeout>);
}

/**
 * leading + trailing 窗口节流，按 session 独立计窗：
 * 静默后的第一条立即发布并开窗；窗内的后续请求只置 pending，在窗末合并成一次发布。
 * 窗末若确有 pending 就发布并续窗，保证突发期间每窗至多一帧；窗末无 pending 则关窗，
 * 下一条请求重新走 leading edge。
 */
export class SessionsIndexFanoutThrottle {
  private readonly windows = new Map<string, WindowState>();
  private readonly publish: (sessionId: string) => void;
  private readonly windowMs: number;
  private readonly setTimer: (callback: () => void, delayMs: number) => FanoutTimerHandle;
  private readonly clearTimer: (handle: FanoutTimerHandle) => void;

  constructor(options: SessionsIndexFanoutThrottleOptions) {
    this.publish = options.publish;
    this.windowMs = options.windowMs ?? WORKFLOW_PROGRESS_INDEX_FANOUT_MS;
    this.setTimer = options.setTimer ?? defaultSetTimer;
    this.clearTimer = options.clearTimer ?? defaultClearTimer;
  }

  /** 高频事件的发布请求：立即发布（leading）或并入窗末的一次发布（trailing）。 */
  request(sessionId: string): void {
    const open = this.windows.get(sessionId);
    if (open) {
      open.pending = true;
      return;
    }
    this.openWindow(sessionId);
    this.publish(sessionId);
  }

  /**
   * 任意即时发布（非进度事件、hydration 补发等）都已经带上了窗内合并的进度，
   * 待发的 trailing 因此被它满足：只清 pending，窗口继续限流，不再补一帧空增量。
   */
  notePublished(sessionId: string): void {
    const open = this.windows.get(sessionId);
    if (open) open.pending = false;
  }

  /** 会话运行态清理：窗口定时器必须随之消失（cleanupSessionRuntime）。 */
  clearSession(sessionId: string): void {
    const open = this.windows.get(sessionId);
    if (!open) return;
    this.clearTimer(open.handle);
    this.windows.delete(sessionId);
  }

  /** 网关 dispose：清掉全部窗口定时器。 */
  clear(): void {
    for (const open of this.windows.values()) this.clearTimer(open.handle);
    this.windows.clear();
  }

  private openWindow(sessionId: string): void {
    this.windows.set(sessionId, {
      pending: false,
      handle: this.setTimer(() => this.onWindowElapsed(sessionId), this.windowMs),
    });
  }

  private onWindowElapsed(sessionId: string): void {
    const open = this.windows.get(sessionId);
    if (!open) return;
    if (!open.pending) {
      // 窗内没有新进度 → 关窗；下一条进度重新立即发布。
      this.windows.delete(sessionId);
      return;
    }
    // 先续窗再发布：publish 会回调 notePublished，状态必须已是新窗口的。
    open.pending = false;
    open.handle = this.setTimer(() => this.onWindowElapsed(sessionId), this.windowMs);
    this.publish(sessionId);
  }
}
