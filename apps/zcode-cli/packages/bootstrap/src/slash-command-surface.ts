import { BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES, type ZCodeSlashCommand } from "@zcode/shared";

/**
 * App `/` 面板与加号菜单按本顺序展示（UI 不维护排序白名单）。`workflow` 紧随 `goal`：两者都是
 * 「开启一段工作」的入口；它受动态工作流开关约束，
 * 由 zcode-protocol/slash-commands.ts 在装配时剔除。
 */
export const APP_PROTOCOL_VISIBLE_BUILTIN_SLASH_COMMAND_NAMES = [
  "goal",
  "workflow",
  "compact",
  "init",
] as const;

/** 仅供 App Composer 使用的命令，不扩展 CLI TUI/help surface。 */
export const APP_PROTOCOL_APP_ONLY_BUILTIN_SLASH_COMMANDS = [
  {
    description: "Switch to Plan mode and optionally send a task.",
    inputHint: "/plan [task]",
    name: "plan",
    source: "builtin",
  },
] as const satisfies readonly ZCodeSlashCommand[];

const EXTRA_RESERVED_SLASH_COMMAND_NAMES = ["compress", "plan"] as const;

const RESERVED_SLASH_COMMAND_NAMES = new Set(
  BUILTIN_ZCODE_SLASH_COMMAND_HELP_ENTRIES.flatMap((entry) => [
    entry.name,
    ...(entry.aliases ?? []),
  ]).concat([...EXTRA_RESERVED_SLASH_COMMAND_NAMES]),
);

function normalizeZCodeSlashCommandName(name: string): string {
  return name.trim().replace(/^\/+/, "").toLowerCase();
}

export function isReservedZCodeSlashCommandName(name: string): boolean {
  return RESERVED_SLASH_COMMAND_NAMES.has(normalizeZCodeSlashCommandName(name));
}
