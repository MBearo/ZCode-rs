import { z } from "zod";

/** auto 保留为内部权限；plan 仅在旧格式读取边界接受。 */
export const executionPermissionModeSchema = z.enum(["build", "edit", "yolo", "auto"]);
export const executionStateSchema = z.object({
  mode: executionPermissionModeSchema,
  planEnabled: z.boolean(),
});
export type ExecutionState = z.infer<typeof executionStateSchema>;

/** 在接纳边界固定旧请求语义，不能在队列消费时按当前配置重新解释。 */
export function resolveExecutionState(
  input: { mode?: string; planEnabled?: boolean },
  current: ExecutionState = { mode: "build", planEnabled: false },
): ExecutionState {
  const mode = executionPermissionModeSchema.safeParse(input.mode);
  return {
    mode: mode.success ? mode.data : current.mode,
    planEnabled:
      input.planEnabled ??
      (input.mode === "plan" ? true : mode.success ? false : current.planEnabled),
  };
}

/** Workspace runtime facts; an absent field preserves older TS runtime behavior. */
export const runtimeExecutionCapabilitiesSchema = z
  .object({
    permissionModes: z.array(executionPermissionModeSchema).min(1).max(4),
    independentPlanState: z.boolean(),
  })
  .strict();
export type RuntimeExecutionCapabilities = z.infer<typeof runtimeExecutionCapabilitiesSchema>;

/** Pure admission hint for App controls. Runtime remains the final authority. */
export function supportsRuntimeExecution(
  input: { mode?: string; planEnabled?: boolean },
  capabilities?: RuntimeExecutionCapabilities,
): boolean {
  if (!capabilities) return true;
  const state = resolveExecutionState(input);
  return (
    capabilities.permissionModes.includes(state.mode) &&
    (!state.planEnabled || capabilities.independentPlanState)
  );
}
