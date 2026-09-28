import type { TuiSubmitPrompt } from "@zcode/tui";
import {
  formatAvailableCommandNames,
  listCustomCommandsForHelp,
} from "../command-center-custom.js";
import { formatNewSessionResult, formatResumeResult } from "./formatters.js";
import { handleCustomCommand } from "./handlers/custom.js";
import { handleDwfCommand } from "./handlers/dwf.js";
import { handleEffortCommand } from "./handlers/effort.js";
import { handleExpertCommand } from "./handlers/expert.js";
import { handleLocaleCommand } from "./handlers/locale.js";
import { handleMcpCommand } from "./handlers/mcp.js";
import { handleModeCommand } from "./handlers/mode.js";
import { handleModelCommand } from "./handlers/model.js";
import { handlePluginsCommand } from "./handlers/plugins.js";
import { handleSkillListCommand } from "./handlers/skill.js";
import { handleTargetCommand } from "./handlers/goal.js";
import { recordSlashCommandInHistory } from "./history.js";
import { attachCurrentSessionMetadata, normalizeTuiPromptInput } from "./metadata.js";
import { buildCheckpointSelection, buildSessionSelection } from "./selections.js";
import {
  AVAILABLE_COMMANDS,
  buildManualSkillPrompt,
  formatSlashCommandHelp,
  parseSlashCommand,
} from "./slash-commands.js";
import {
  buildLoginSelection,
  emitLoginAuthorizeMessage,
  formatLoginResult,
  formatProviderSetupResult,
  loginSetupResponse,
  parseApiKeyLoginArgs,
} from "./login-flow.js";
import { loginRequiredResponse } from "../tui-login-state.js";
import type { CommandCenterDeps } from "./types.js";

export function createCommandCenter(deps: CommandCenterDeps): TuiSubmitPrompt {
  return async (input, options) => {
    const promptInput = normalizeTuiPromptInput(input);
    const command = parseSlashCommand(promptInput.text);
    const hasAttachments = (promptInput.attachments?.length ?? 0) > 0;

    if (!command) {
      if (await isLoginRequired(deps)) {
        return {
          loginRequired: true,
          mode: deps.getMode?.(),
          response: loginRequiredResponse(deps.getLocale?.()),
        };
      }
      const app = await deps.getApp();
      return attachCurrentSessionMetadata(await app.submitPrompt(input, options), deps, app);
    }

    if (hasAttachments) {
      return {
        mode: deps.getMode?.(),
        response: "Image attachments are only supported for normal prompts.",
      };
    }

    if (command.type === "unknown") {
      const customResult = await handleCustomCommand(command.rawName, command.args, deps, options);
      if (customResult) {
        await recordSlashCommandInHistory(deps, promptInput.text, command);
        return customResult;
      }
      const customCommands = await listCustomCommandsForHelp(deps);
      return {
        mode: deps.getMode?.(),
        response: `Unknown command: /${command.rawName}. Available commands: ${formatAvailableCommandNames(AVAILABLE_COMMANDS, customCommands)}.`,
      };
    }

    const result = await (async () => {
      if (command.name === "help") {
        const customCommands = await listCustomCommandsForHelp(deps);
        return {
          mode: deps.getMode?.(),
          response: formatSlashCommandHelp(command.args, customCommands),
        };
      }

      if (command.name === "login") {
        if (command.args.length === 0) {
          return {
            loginRequired: await isLoginRequired(deps),
            mode: deps.getMode?.(),
            response: loginSetupResponse(deps.getLocale?.()),
            selection: buildLoginSelection(deps.getLocale?.()),
          };
        }
        if (command.args === "zai-coding-plan") {
          if (!deps.login) {
            return {
              mode: deps.getMode?.(),
              response: "Z.AI Coding Plan login is not available in this client.",
            };
          }

          return {
            loginRequired: false,
            mode: deps.getMode?.(),
            response: formatLoginResult(
              await deps.login({
                abortSignal: options.abortSignal,
                onAuthorizeUrl: async (data) => {
                  await emitLoginAuthorizeMessage(
                    options,
                    data.authorize_url,
                    "Z.AI",
                    await deps.getApp(),
                  );
                },
              }),
            ),
          };
        }
        if (command.args === "bigmodel-coding-plan") {
          if (!deps.loginBigmodel) {
            return {
              mode: deps.getMode?.(),
              response: "BigModel Coding Plan login is not available in this client.",
            };
          }

          return {
            loginRequired: false,
            mode: deps.getMode?.(),
            response: formatProviderSetupResult(
              await deps.loginBigmodel({
                abortSignal: options.abortSignal,
                onAuthorizeUrl: async (data) => {
                  await emitLoginAuthorizeMessage(
                    options,
                    data.authorize_url,
                    "BigModel",
                    await deps.getApp(),
                  );
                },
              }),
            ),
          };
        }

        const apiKeyCommand = parseApiKeyLoginArgs(command.args);
        if (apiKeyCommand) {
          if (!deps.configureApiKey) {
            return {
              mode: deps.getMode?.(),
              response: "Manual API key setup is not available in this client.",
            };
          }
          if (!apiKeyCommand.apiKey) {
            return {
              loginRequired: await isLoginRequired(deps),
              mode: deps.getMode?.(),
              response: `Usage: /login ${apiKeyCommand.kind} <api-key>`,
            };
          }
          return {
            loginRequired: false,
            mode: deps.getMode?.(),
            response: formatProviderSetupResult(
              await deps.configureApiKey({
                apiKey: apiKeyCommand.apiKey,
                providerId: apiKeyCommand.providerId,
              }),
            ),
          };
        }

        return {
          mode: deps.getMode?.(),
          response:
            "Usage: /login [zai-coding-plan|bigmodel-coding-plan|zai-coding-plan-api-key <api-key>|bigmodel-coding-plan-api-key <api-key>]",
        };
      }

      if (command.name === "logout") {
        if (command.args.length > 0) {
          return {
            mode: deps.getMode?.(),
            response: "Usage: /logout",
          };
        }
        if (!deps.logout) {
          return {
            mode: deps.getMode?.(),
            response: "Logout is not available in this client.",
          };
        }

        const result = await deps.logout();
        return {
          mode: deps.getMode?.(),
          response: `Logged out from Coding Plan accounts. Credentials: ${result.credentialsPath}`,
        };
      }

      if (command.name === "compact") {
        const app = await deps.getApp();
        const prompt = command.args ? `/compact ${command.args}` : "/compact";
        return attachCurrentSessionMetadata(await app.submitPrompt(prompt, options), deps, app);
      }

      if (command.name === "init") {
        const app = await deps.getApp();
        const prompt = command.args ? `/init ${command.args}` : "/init";
        // TUI 已知 slash command 若没有显式分支，会落到文件末尾的
        // resume 兜底。/init 是普通 prompt command，必须交给 app.submitPrompt
        // 进入 bootstrap resolver，才能和 app --stdio 复用同一套展开逻辑。
        return attachCurrentSessionMetadata(await app.submitPrompt(prompt, options), deps, app);
      }

      if (command.name === "workflow") {
        const app = await deps.getApp();
        const prompt = command.args ? `/workflow ${command.args}` : "/workflow";
        // 与 /init 同款：原文交给 app.submitPrompt，由 bootstrap 的 builtin resolver 展开成
        // 「先加载 dynamic-workflows 技能，再写脚本调 CreateWorkflow」的提示词。
        return attachCurrentSessionMetadata(await app.submitPrompt(prompt, options), deps, app);
      }

      if (command.name === "expert") {
        return handleExpertCommand(command.args, deps, options);
      }

      if (command.name === "effort") {
        return handleEffortCommand(command.args, deps);
      }

      if (command.name === "dwf") {
        return handleDwfCommand(command.args, deps);
      }

      if (command.name === "rewind") {
        const app = await deps.getApp();
        if (command.args.length === 0 && app.listCheckpoints) {
          return {
            mode: deps.getMode?.(),
            response: "Select a checkpoint to rewind.",
            selection: buildCheckpointSelection("rewind", await app.listCheckpoints({ limit: 50 })),
          };
        }
        const prompt = command.args ? `/rewind ${command.args}` : "/rewind";
        return attachCurrentSessionMetadata(await app.submitPrompt(prompt, options), deps, app);
      }

      if (command.name === "fork") {
        if (command.args.length === 0) {
          const app = await deps.getApp();
          if (app.listCheckpoints) {
            return {
              mode: deps.getMode?.(),
              response: "Select a checkpoint to fork.",
              selection: buildCheckpointSelection("fork", await app.listCheckpoints({ limit: 50 })),
            };
          }
        }
        const targetCheckpointId = parseForkTarget(command.args);
        if (deps.forkApp) {
          const result = await deps.forkApp(targetCheckpointId);
          return {
            mode: deps.getMode?.(),
            response: result.response,
            traceId: undefined,
          };
        }

        const app = await deps.getApp();
        const prompt = targetCheckpointId ? `/fork ${targetCheckpointId}` : "/fork latest";
        return attachCurrentSessionMetadata(await app.submitPrompt(prompt, options), deps, app);
      }

      if (command.name === "mode") {
        return handleModeCommand(command.args, deps);
      }

      if (command.name === "locale") {
        return handleLocaleCommand(command.args, deps);
      }

      if (command.name === "mcp") {
        return handleMcpCommand(command.args, deps);
      }

      if (command.name === "plugins") {
        return handlePluginsCommand(command.args, deps);
      }

      if (command.name === "model") {
        return handleModelCommand(command.args, deps, promptInput.modelSelection);
      }

      if (command.name === "goal") {
        return handleTargetCommand(command.args, deps, options);
      }

      if (command.name === "new") {
        if (command.args.length > 0) {
          return {
            mode: deps.getMode?.(),
            response: "Usage: /new",
          };
        }
        if (!deps.newApp) {
          return {
            mode: deps.getMode?.(),
            response: "Creating a new session is not available in this client.",
          };
        }

        const app = await deps.newApp();
        return {
          mode: deps.getMode?.(),
          locale: app.getLocale?.(),
          model: app.getModel?.(),
          theme: app.getTheme?.(),
          resetSessionProjection: true,
          response: formatNewSessionResult(app.sessionId),
          sessionId: app.sessionId,
          thoughtLevel: app.getThoughtLevel?.(),
          traceId: app.traceId,
        };
      }

      if (command.name === "skill") {
        if (!command.skillName) {
          return handleSkillListCommand(deps);
        }
        const app = await deps.getApp();
        return attachCurrentSessionMetadata(
          await app.submitPrompt(buildManualSkillPrompt(command.skillName, command.task), options),
          deps,
          app,
        );
      }

      if (
        command.name === "resume" &&
        command.args.length === 0 &&
        command.rawName === "resume" &&
        deps.listSessions
      ) {
        return {
          mode: deps.getMode?.(),
          response: "Select a session to resume.",
          selection: buildSessionSelection(await deps.listSessions()),
        };
      }

      const app = await deps.resumeApp(command.args || undefined);
      const result = await app.resume({
        onEvent: options.onEvent,
      });
      const restoredMessages = app.loadSessionTranscript
        ? await app.loadSessionTranscript()
        : undefined;

      return {
        mode: deps.getMode?.(),
        locale: app.getLocale?.(),
        model: app.getModel?.(),
        theme: app.getTheme?.(),
        ...(restoredMessages !== undefined
          ? {
              resetSessionProjection: true,
              restoredMessages,
            }
          : {}),
        response: formatResumeResult(app.sessionId, result),
        thoughtLevel: app.getThoughtLevel?.(),
        traceId: result.traceId ?? app.traceId,
      };
    })();

    await recordSlashCommandInHistory(deps, promptInput.text, command);
    return result;
  };
}

function parseForkTarget(args: string): string | undefined {
  const trimmed = args.trim();
  if (trimmed.length === 0 || trimmed === "latest") return undefined;
  return trimmed;
}

async function isLoginRequired(deps: CommandCenterDeps): Promise<boolean> {
  if (!deps.hasSelectableModels) return false;
  try {
    return !(await deps.hasSelectableModels());
  } catch {
    return false;
  }
}
