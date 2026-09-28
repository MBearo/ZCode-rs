import type { ZCodeAutomationBotDeliveryTarget } from "@zcode/shared";

interface CronBotDeliveryRepo {
  getBotDeliveryTarget(
    automationId: string,
    workspaceKey?: string,
  ): Promise<ZCodeAutomationBotDeliveryTarget | undefined>;
}

interface CronBotDeliveryService {
  watchAutomationRun(params: {
    target: ZCodeAutomationBotDeliveryTarget;
    taskId: string;
    workspacePath: string;
    workspaceIdentity?: string;
  }): Promise<void>;
}

/**
 * 在 prompt 派发前完成 Bot 终态订阅，避免快速任务先完成、后注册 listener 而漏回推。
 */
export async function watchCronRunBotDelivery(params: {
  automationId: string;
  workspaceKey: string;
  workspacePath: string;
  workspaceIdentity?: string;
  taskId: string;
  repo: CronBotDeliveryRepo;
  botsService: CronBotDeliveryService;
}): Promise<boolean> {
  const target = await params.repo.getBotDeliveryTarget(
    params.automationId,
    params.workspaceKey,
  );
  if (!target) return false;
  await params.botsService.watchAutomationRun({
    target,
    taskId: params.taskId,
    workspacePath: params.workspacePath,
    ...(params.workspaceIdentity
      ? { workspaceIdentity: params.workspaceIdentity }
      : {}),
  });
  return true;
}
