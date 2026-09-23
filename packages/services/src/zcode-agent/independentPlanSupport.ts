import { zcodeProtocolMethods, zcodeRuntimeCapabilitiesSchema } from "@zcode/shared";
import type { ZCodeProtocolClient } from "./zcodeProtocolClient.js";

const checks = new WeakMap<object, Promise<void>>();
type RuntimeCapabilities = ReturnType<typeof zcodeRuntimeCapabilitiesSchema.parse>;
const capabilitiesByClient = new WeakMap<object, Promise<RuntimeCapabilities>>();

/** 能力属于当前 runtime connection；进程换代时新的 client 自然重新协商。 */
export function readRuntimeCapabilities(
  client: Pick<ZCodeProtocolClient, "request">,
): Promise<RuntimeCapabilities> {
  const cached = capabilitiesByClient.get(client);
  if (cached) return cached;
  const pending = client
    .request(zcodeProtocolMethods.runtimeCapabilities, {}, zcodeRuntimeCapabilitiesSchema)
    .catch((error: unknown) => {
      if (error && typeof error === "object" && "code" in error && error.code === -32601) return {};
      capabilitiesByClient.delete(client);
      throw error;
    });
  capabilitiesByClient.set(client, pending);
  return pending;
}

/** Host 更新不代表远端 CLI 已更新；旧 CLI 会剥掉 Plan 字段，必须在发送前确认执行端。 */
export function ensureIndependentPlanSupport(
  client: Pick<ZCodeProtocolClient, "request">,
): Promise<void> {
  const cached = checks.get(client);
  if (cached) return cached;
  const check = readRuntimeCapabilities(client)
    .then((result) => {
      if (result.independentPlanState !== true) throw new Error("proto.independentPlanUnsupported");
    })
    .catch((cause: unknown) => {
      checks.delete(client);
      throw new Error("proto.independentPlanUnsupported", { cause });
    });
  checks.set(client, check);
  return check;
}
