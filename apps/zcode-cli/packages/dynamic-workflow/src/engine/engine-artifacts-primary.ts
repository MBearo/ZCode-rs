/**
 * 交付物的 primary 标记：全 run 至多一个 ID 带有该标记。
 * 内容成员与预置成员共用同一组准入判据。
 *
 * 这些函数只读 `state.artifacts`，不落库、不发事件；状态从 journal 派生，resume 后保持一致。
 * 调用方根据判据决定拒绝发布还是使整个 run 失败。
 */

import type { EngineState } from "./engine-state.js";
import type { InstanceRef } from "./types.js";
import { refToString } from "./types.js";

/** 本 run 目前的 primary id（至多一个）；没有则 undefined。从 `state.artifacts` 派生，resume 后自然一致。 */
export function primaryArtifactId(state: EngineState): string | undefined {
  for (const [id, idState] of state.artifacts) if (idState.primary) return id;
  return undefined;
}

/** `id` 想当 primary 而**别的** id 已经是 ⇒ 返回那个 id；否则 undefined（不想当 / 就是它自己）。 */
export function primaryConflict(
  state: EngineState,
  id: string,
  primary: boolean,
): string | undefined {
  if (!primary) return undefined;
  const holder = primaryArtifactId(state);
  return holder === undefined || holder === id ? undefined : holder;
}

export function primaryConflictMessage(
  id: string,
  holder: string,
  fix: string,
  instance: InstanceRef | undefined,
): string {
  const where = instance === undefined ? "" : ` (at ${refToString(instance)})`;
  return (
    `Cannot mark "${id}" as primary${where}: "${holder}" is already this run's primary artifact. ` +
    `A run has one deliverable; ${fix}, or publish it as a new version of "${holder}".`
  );
}
