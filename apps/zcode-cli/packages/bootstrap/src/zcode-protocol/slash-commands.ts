import { BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES, type ZCodeSlashCommand } from "@zcode/shared";
import { BUILTIN_WORKFLOW_COMMAND_NAME } from "../builtin-workflow-command.js";
import {
  listZCodeCustomCommands,
  type ListZCodeCustomCommandsOptions,
} from "../custom-commands.js";
import {
  APP_PROTOCOL_APP_ONLY_BUILTIN_SLASH_COMMANDS,
  APP_PROTOCOL_VISIBLE_BUILTIN_SLASH_COMMAND_NAMES,
  isReservedZCodeSlashCommandName,
} from "../slash-command-surface.js";

export interface ListProtocolSlashCommandsOptions extends ListZCodeCustomCommandsOptions {
  /**
   * 动态工作流开关。只有显式 false 才从目录中剔除内置 `workflow`。
   * 未传入该字段的调用方保留默认目录；协议服务端从 appRuntimePreferences 传入显式布尔。
   */
  dynamicWorkflowEnabled?: boolean;
}

export async function listProtocolSlashCommands(
  options: ListProtocolSlashCommandsOptions = {},
): Promise<ZCodeSlashCommand[]> {
  // 动态工作流关闭时：composer 的加号菜单与 `/` 面板都只读这份目录，剔除即两个入口一起消失。
  // `workflow` 是内置命令且是保留名，用户/插件的同名自定义命令在下面的 reserved 过滤里一并消失，
  // 不会在门关着时借自定义命令的身份漏回目录。
  const builtins = listAppProtocolBuiltinSlashCommands().filter(
    (command) =>
      options.dynamicWorkflowEnabled !== false || command.name !== BUILTIN_WORKFLOW_COMMAND_NAME,
  );
  let customCommands: Awaited<ReturnType<typeof listZCodeCustomCommands>>["commands"] = [];
  try {
    const outcome = await listZCodeCustomCommands(options);
    customCommands = outcome.commands;
  } catch {
    // 自定义命令发现失败不应阻断 session snapshot；保留可执行的内置协议命令。
    customCommands = [];
  }

  return [
    ...builtins,
    ...customCommands
      .filter((command) => !command.disableNonInteractive)
      .filter((command) => !isReservedZCodeSlashCommandName(command.name))
      .map((command) => ({
        description: command.description,
        inputHint: `/${command.name}${command.argumentHint ? ` ${command.argumentHint}` : ""}`,
        name: command.name,
        source: "custom" as const,
      })),
  ];
}

/** App `/` 面板按本目录顺序展示；内置段的顺序由 APP_PROTOCOL_VISIBLE_BUILTIN_SLASH_COMMAND_NAMES 决定。 */
function listAppProtocolBuiltinSlashCommands(): ZCodeSlashCommand[] {
  const sharedBuiltins = APP_PROTOCOL_VISIBLE_BUILTIN_SLASH_COMMAND_NAMES.flatMap((name) => {
    const command = BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES.find((entry) => entry.name === name);
    if (!command) return [];
    return [
      {
        description: command.summary,
        inputHint: command.usage,
        name: command.name,
        source: "builtin" as const,
      },
    ];
  });
  return [...sharedBuiltins, ...APP_PROTOCOL_APP_ONLY_BUILTIN_SLASH_COMMANDS];
}
