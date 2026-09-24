import assert from "node:assert/strict";

const BACKGROUND =
  /^Command running in background with ID: (\S+)\. Output is being written to: (.+?)\. You will be notified when it completes\./;

/** Node's Bash background text: the task id and its output file. */
export function backgroundTask(content: string) {
  const match = BACKGROUND.exec(content);
  assert(match, content);
  return { taskId: match[1]!, outputFile: match[2]! };
}

/** Node's TaskOutput blocks (`<name>value</name>` separated by blank lines). */
export function taskOutputBlocks(content: string): Record<string, string> {
  const blocks: Record<string, string> = {};
  for (const match of content.matchAll(/<(\w+)>\n?([\s\S]*?)\n?<\/\1>/g)) {
    blocks[match[1]!] = match[2]!;
  }
  return blocks;
}
