import { spawn } from "node:child_process";

export function run(
  command,
  args,
  { cwd, env = process.env, input, inherit = false, allowFailure = false } = {},
) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      shell: false,
      stdio: ["pipe", inherit ? "inherit" : "pipe", inherit ? "inherit" : "pipe"],
    });
    const stdout = [],
      stderr = [];
    child.stdout?.on("data", (chunk) => stdout.push(chunk));
    child.stderr?.on("data", (chunk) => stderr.push(chunk));
    child.on("error", reject);
    child.stdin.on("error", (error) => {
      if (error.code !== "EPIPE") reject(error);
    });
    child.stdin.end(input);
    child.on("close", (code, signal) => {
      const result = {
        code: code ?? 1,
        stdout: Buffer.concat(stdout),
        stderr: Buffer.concat(stderr),
      };
      if (result.code && !allowFailure)
        reject(
          new Error(
            `${command} ${args[0] ?? ""}: ${result.stderr.toString().trim() || signal || result.code}`,
          ),
        );
      else resolve(result);
    });
  });
}

export async function git(root, args, options = {}) {
  return (await run("git", args, { cwd: root, ...options })).stdout;
}

export async function cleanGitEnv(root, env = process.env) {
  const names = (await git(root, ["rev-parse", "--local-env-vars"]))
    .toString()
    .trim()
    .split(/\r?\n/u);
  const clean = { ...env };
  for (const name of names) delete clean[name];
  return clean;
}
