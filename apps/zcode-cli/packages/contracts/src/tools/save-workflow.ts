// ============================================================
// SaveWorkflow Tool - 把一段 dwf 脚本连同元数据存成项目里的可复用定义
// ============================================================

import { z } from "zod";
import { toToolJsonSchema } from "./json-schema.js";
import { CreateWorkflowDiagnosticSchema } from "./create-workflow.js";
import {
  SAVED_WORKFLOW_MAX_NAME_CHARS,
  SavedWorkflowArgsDeclarationSchema,
  SavedWorkflowScopeSchema,
  SavedWorkflowShadowingSchema,
} from "./saved-workflow.js";

export const SAVE_WORKFLOW_TOOL_NAME = "SaveWorkflow";

/**
 * 「恰好给一个正文来源」的违规说明。字段不叫 `path` 是因为 `path` 已经是本工具**解析回填的
 * 落点**（确认窗与 UI 读它），两个同名字段一个是输入一个是输出，只会让模型把保存目标当成
 * 源文件传进来。
 */
export const SAVE_WORKFLOW_SOURCE_ERROR =
  "Provide exactly one script source: `script` for the body inline, or `script_path` for the file holding it (a draft, usually). Passing both, or neither, is ambiguous.";

/** 模型把自带 frontmatter 的脚本传进来时的业务失败说明。 */
export const SAVE_WORKFLOW_SENTINEL_IN_SCRIPT_ERROR =
  "The `script` must be the workflow body only — it already starts with a `/* zcode-workflow` metadata block. Pass the metadata through the `description` / `whenToUse` / `args` fields instead; the block is written for you.";

export const SaveWorkflowInputSchema = z
  .object({
    name: z
      .string()
      .min(1)
      .max(SAVED_WORKFLOW_MAX_NAME_CHARS)
      .describe("File-safe identifier: letters, digits, dot, dash and underscore."),
    description: z.string().min(1).describe("One line saying what the workflow does."),
    whenToUse: z.string().min(1).optional().describe("When this workflow is the right answer."),
    args: SavedWorkflowArgsDeclarationSchema.optional().describe(
      "Argument declarations the script reads off `args`.",
    ),
    script: z
      .string()
      .optional()
      .describe(
        "The script body, inline, without a metadata block. This OR `script_path`, never both.",
      ),
    /**
     * 正文的第二条来源：一个已经在盘上的文件，
     * 通常是刚跑过的那份草稿。文件带元数据块时**块被丢掉**——元数据由本次调用的字段说了算，
     * 用户批准的是那些字段，不是文件里那一份。
     */
    script_path: z
      .string()
      .min(1)
      .optional()
      .describe(
        "A file holding the script body, usually a draft a result named. This OR `script`, never both.",
      ),
    // 作用域由**模型**说出，不由系统猜——没有默认值，每次都要判断。判据中性：脚本是否
    // 引用本仓库的东西？是 → project，否 → global。
    scope: SavedWorkflowScopeSchema.describe(
      '"project" when the script depends on this repository; "global" when it depends on nothing in it. Required, no default.',
    ),
    // ——以下三个字段由 `resolveInput` 解析回填，模型不填——
    // 它们是**确认窗要展示的事实**：这次保存落到哪个文件、是不是一次覆盖、是否遮蔽了另一档。
    // 走入参而不是 display，是因为入参通道对每个客户端版本都是无 schema 的透传，旧桌面与
    // legacy v3 因此也能看到完整内容。
    /** 解析回填：文件落点。 */
    path: z.string().min(1).optional(),
    /** 解析回填：目标已存在，这次批准的是一次**覆盖**。 */
    overwrite: z.boolean().optional(),
    /** 解析回填：另一档已有同名定义（遮蔽事实，供确认窗展示）。缺席 = 无同名。 */
    shadowing: SavedWorkflowShadowingSchema.optional(),
  })
  .strict();

export type SaveWorkflowInput = z.infer<typeof SaveWorkflowInputSchema>;

export const SaveWorkflowInputJsonSchema = toToolJsonSchema(SaveWorkflowInputSchema);

/**
 * 输出与 CreateWorkflow 的诊断形状共用一个 schema：两个工具跑的是**同一个**类型检查器，
 * 让模型在两处看到不同形状的诊断只会教它写两套解析。
 *
 * `overwritten` 只在真写了文件时出现——诊断-only 的结果没有"覆盖了没有"可言，发一个
 * `false` 会让模型以为落盘成功了。
 */
export const SaveWorkflowOutputSchema = z
  .object({
    diagnostics: z.array(CreateWorkflowDiagnosticSchema),
    ok: z.boolean(),
    response: z.string(),
    name: z.string().min(1),
    scope: SavedWorkflowScopeSchema,
    /** 文件落点（未写成时是**本该**写到的位置，让模型能把错误说清楚）。 */
    path: z.string().min(1),
    overwritten: z.boolean().optional(),
  })
  .strict();

export type SaveWorkflowOutput = z.infer<typeof SaveWorkflowOutputSchema>;

export const SaveWorkflowOutputJsonSchema = toToolJsonSchema(SaveWorkflowOutputSchema);
