import type { TuiSlashCommandSuggestion } from "@zcode/tui";
import {
  findCustomCommandHelpEntry,
  formatAvailableCommandNames,
  formatCustomCommandHelpEntry,
  listCustomCommandSuggestions,
  type CommandCenterCustomCommandListOutcome,
} from "../command-center-custom.js";
import { SLASH_COMMAND_HELP_ENTRIES, type SlashCommandHelpEntry } from "./slash-command-help.js";
import type { SlashCommand } from "./slash-command-types.js";
import { splitArgs } from "./utils.js";

export const AVAILABLE_COMMANDS = SLASH_COMMAND_HELP_ENTRIES.map((entry) => `/${entry.name}`);

const SKILL_COMMAND_USAGE = "Usage: /skill [<skill-name> [task]]";

export function parseSlashCommand(input: string): SlashCommand | null {
  if (!input.startsWith("/")) return null;

  const trimmed = input.trim();
  const commandEnd = trimmed.search(/\s/);
  const rawName = (
    commandEnd === -1 ? trimmed.slice(1) : trimmed.slice(1, commandEnd)
  ).toLowerCase();
  const args = commandEnd === -1 ? "" : trimmed.slice(commandEnd + 1).trim();

  if (rawName === "compact") {
    return {
      args,
      name: "compact",
      rawName,
      type: "known",
    };
  }

  if (rawName === "expert") {
    return {
      args,
      name: "expert",
      rawName,
      type: "known",
    };
  }

  if (rawName === "effort" || rawName === "variant") {
    return {
      args,
      name: "effort",
      rawName,
      type: "known",
    };
  }

  if (rawName === "dwf") {
    return {
      args,
      name: "dwf",
      rawName,
      type: "known",
    };
  }

  if (rawName === "fork") {
    return {
      args,
      name: "fork",
      rawName,
      type: "known",
    };
  }

  if (rawName === "help") {
    return {
      args,
      name: "help",
      rawName,
      type: "known",
    };
  }

  if (rawName === "init") {
    return {
      args,
      name: "init",
      rawName,
      type: "known",
    };
  }

  // `/workflow` 与 `/init` 同为 bootstrap 展开的内置 prompt 命令：这里只识别名字，
  // 正文由 builtin-prompt-command.ts 生成；headless 下按 known 且非 expert/goal 走普通 prompt 路径。
  if (rawName === "workflow") {
    return {
      args,
      name: "workflow",
      rawName,
      type: "known",
    };
  }

  if (rawName === "login") {
    return {
      args,
      name: "login",
      rawName,
      type: "known",
    };
  }

  if (rawName === "logout") {
    return {
      args,
      name: "logout",
      rawName,
      type: "known",
    };
  }

  if (rawName === "locale" || rawName === "language") {
    return {
      args,
      name: "locale",
      rawName,
      type: "known",
    };
  }

  if (rawName === "resume" || rawName === "continue") {
    return {
      args,
      name: "resume",
      rawName,
      type: "known",
    };
  }

  if (rawName === "new" || rawName === "clear") {
    return {
      args,
      name: "new",
      rawName,
      type: "known",
    };
  }

  if (rawName === "rewind") {
    return {
      args,
      name: "rewind",
      rawName,
      type: "known",
    };
  }

  if (rawName === "mode") {
    return {
      args,
      name: "mode",
      rawName,
      type: "known",
    };
  }

  if (rawName === "mcp") {
    return {
      args,
      name: "mcp",
      rawName,
      type: "known",
    };
  }

  if (rawName === "plugins" || rawName === "plugin") {
    return {
      args,
      name: "plugins",
      rawName,
      type: "known",
    };
  }

  if (rawName === "model") {
    return {
      args,
      name: "model",
      rawName,
      type: "known",
    };
  }

  if (rawName === "goal" || rawName === "target") {
    return {
      args,
      name: "goal",
      rawName,
      type: "known",
    };
  }

  if (rawName === "skill") {
    const parsed = parseSkillCommandArgs(args);
    if (!parsed) {
      return {
        args,
        name: "skill",
        rawName,
        skillName: "",
        task: "",
        type: "known",
      };
    }
    return {
      args,
      name: "skill",
      rawName,
      skillName: parsed.skillName,
      task: parsed.task,
      type: "known",
    };
  }

  return {
    args,
    rawName,
    type: "unknown",
  };
}

export function buildManualSkillPrompt(skillName: string, task: string): string {
  const trimmedTask = task.trim();
  const taskBlock =
    trimmedTask.length > 0
      ? `User request:\n${trimmedTask}`
      : "No additional user request was provided. Load the skill and respond according to its instructions.";

  return [
    `Use the skill named \`${skillName}\` for this turn.`,
    `First call the \`Skill\` tool with name \`${skillName}\` before doing the task.`,
    "After the skill content is loaded, follow its instructions and continue.",
    "",
    taskBlock,
  ].join("\n");
}

export function manualSkillCommandUsage(): string {
  return SKILL_COMMAND_USAGE;
}

export function listSlashCommandSuggestions(
  customCommands?: CommandCenterCustomCommandListOutcome,
): TuiSlashCommandSuggestion[] {
  return [
    ...SLASH_COMMAND_HELP_ENTRIES.map((entry) => ({
      ...(entry.aliases ? { aliases: entry.aliases } : {}),
      name: entry.name,
      summary: entry.summary,
      usage: entry.usage,
    })),
    ...listCustomCommandSuggestions(customCommands),
  ];
}

export function formatSlashCommandHelp(
  args = "",
  customCommands?: CommandCenterCustomCommandListOutcome,
): string {
  const target = normalizeHelpTarget(args);
  if (target) {
    const entry = findSlashCommandHelpEntry(target);
    if (entry) return formatSlashCommandHelpEntry(entry);

    const custom = findCustomCommandHelpEntry(target, customCommands);
    if (custom) return formatCustomCommandHelpEntry(custom);

    return `Unknown slash command: /${target}. Available commands: ${formatAvailableCommandNames(AVAILABLE_COMMANDS, customCommands)}.`;
  }

  const lines = [
    "Slash commands:",
    ...SLASH_COMMAND_HELP_ENTRIES.map((entry) => `- ${entry.usage}: ${entry.summary}`),
  ];
  if (customCommands && customCommands.commands.length > 0) {
    lines.push(
      "",
      "Custom commands:",
      ...customCommands.commands.map(
        (command) =>
          `- /${command.name}${command.argumentHint ? ` ${command.argumentHint}` : ""}: ${command.description}`,
      ),
    );
  }
  lines.push("", "Use /help <command> for details.");
  return lines.join("\n");
}

function normalizeHelpTarget(args: string): string | undefined {
  const [target] = splitArgs(args);
  const normalized = target?.replace(/^\/+/, "").toLowerCase();
  return normalized && normalized.length > 0 ? normalized : undefined;
}

function findSlashCommandHelpEntry(name: string): SlashCommandHelpEntry | undefined {
  return SLASH_COMMAND_HELP_ENTRIES.find(
    (entry) => entry.name === name || entry.aliases?.includes(name),
  );
}

function formatSlashCommandHelpEntry(entry: SlashCommandHelpEntry): string {
  const aliasLine =
    entry.aliases && entry.aliases.length > 0
      ? [`Aliases: ${entry.aliases.map((alias) => `/${alias}`).join(", ")}`]
      : [];
  return [entry.usage, entry.summary, ...aliasLine, ...entry.details].join("\n");
}

function parseSkillCommandArgs(args: string): { skillName: string; task: string } | null {
  const trimmed = args.trim();
  if (trimmed.length === 0) return null;
  const firstWhitespace = trimmed.search(/\s/);
  if (firstWhitespace === -1) {
    return {
      skillName: trimmed,
      task: "",
    };
  }
  return {
    skillName: trimmed.slice(0, firstWhitespace),
    task: trimmed.slice(firstWhitespace + 1).trim(),
  };
}
