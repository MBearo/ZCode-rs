import { useEffect, useMemo, useReducer } from "react";
import useSWR from "swr";
import type { IZCodeSessionService } from "@zcode/services";
import { ZCODE_AGENT_PROVIDER, type ZCodeProvider } from "@zcode/shared";
import { prepareWorkspaceWithZCodeSessionService } from "./workspacePrepareRpc.js";
import { useZCodeSessionStore } from "@/store/zcodeSessionStore.js";

const serviceIds = new WeakMap<object, number>();
let nextServiceId = 0;
function serviceId(service: object): number {
  let id = serviceIds.get(service);
  if (id === undefined) {
    id = ++nextServiceId;
    serviceIds.set(service, id);
  }
  return id;
}

/** One read projection per routed service/workspace/generation; never a permission preference. */
export function useWorkspacePresentation(params: {
  workspacePath: string;
  workspaceIdentity?: string;
  provider?: ZCodeProvider;
  enabled: boolean;
  service: IZCodeSessionService;
  onRuntimeRestart?: (listener: () => void) => () => void;
  onRuntimeLifecycle?: (listener: (state: "available" | "unavailable") => void) => () => void;
}) {
  const {
    workspacePath,
    workspaceIdentity,
    provider = ZCODE_AGENT_PROVIDER,
    enabled,
    service,
    onRuntimeRestart,
    onRuntimeLifecycle,
  } = params;
  const [epoch, invalidate] = useReducer((value: number) => value + 1, 0);
  useEffect(() => {
    // 换代时旧权限能力不能继续放行；有 lifecycle 时只订一条通道，避免双重失效。
    if (onRuntimeLifecycle)
      return onRuntimeLifecycle((state) => {
        if (state === "unavailable") invalidate();
      });
    return onRuntimeRestart?.(invalidate);
  }, [onRuntimeLifecycle, onRuntimeRestart]);
  const key = useMemo(
    () =>
      enabled
        ? ([
            "workspace-presentation",
            serviceId(service),
            workspaceIdentity?.trim() || workspacePath,
            epoch,
          ] as const)
        : null,
    [enabled, service, workspaceIdentity, workspacePath, epoch],
  );
  const read = useSWR(
    key,
    () =>
      prepareWorkspaceWithZCodeSessionService({
        workspacePath,
        workspaceIdentity,
        provider,
        zcodeSessionService: service,
      }),
    {
      keepPreviousData: false,
      revalidateOnMount: true,
      revalidateOnFocus: false,
      revalidateOnReconnect: true,
      dedupingInterval: 0,
    },
  );
  const ready = enabled && Boolean(read.data) && !read.isValidating && !read.error;
  useEffect(() => {
    const store = useZCodeSessionStore.getState();
    if (!ready || !read.data) {
      store.setConfigOptionsStatus(
        workspacePath,
        read.error ? "error" : enabled ? "loading" : "idle",
        workspaceIdentity,
      );
      return;
    }
    // SWR 按 key 隔离迟到响应；只把当前代已完成的 presentation 写入既有显示投影。
    store.setConfigOptions(workspacePath, read.data.configOptions ?? [], workspaceIdentity);
    store.setSlashCommands(workspacePath, read.data.slashCommands ?? [], workspaceIdentity);
    store.setConfigOptionsStatus(workspacePath, "ready", workspaceIdentity);
  }, [ready, read.data, read.error, enabled, workspacePath, workspaceIdentity]);
  return {
    ready,
    error: read.error,
    executionCapabilities: ready ? read.data?.executionCapabilities : undefined,
  };
}
