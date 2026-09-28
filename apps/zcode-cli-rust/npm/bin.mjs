#!/usr/bin/env node
import { parseArgs } from "node:util";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { discoverApp, runningAppPids } from "./lib/app.mjs";
import { binarySource, installBinary } from "./lib/install.mjs";
import { prepareLaunch, startApp, launchStatus } from "./lib/launch.mjs";

try {
  const { values, positionals } = parseArgs({
    allowPositionals: true,
    options: {
      app: { type: "string" },
      binary: { type: "string" },
      "runtime-dir": { type: "string" },
      help: { type: "boolean" },
      json: { type: "boolean" },
    },
  });
  if (values.help || !positionals.length) {
    console.log(
      "Usage: zcode-rust app <launch|doctor|status> [--app <ZCode.app or executable>] [--runtime-dir <directory>] [--binary <local Rust binary>] [--json]\nSupports the unmodified ZCode 3.14.3 app. Fully quit ZCode before launch. The original app icon continues to use its bundled CLI.",
    );
  } else {
    if (
      positionals.length !== 2 ||
      positionals[0] !== "app" ||
      !["launch", "doctor", "status"].includes(positionals[1])
    )
      throw Error("Expected app launch, app doctor, or app status. Use --help.");
    const runtimeDir = resolve(
      values["runtime-dir"] ?? join(homedir(), ".zcode/runtimes/rust-launcher"),
    );
    if (positionals[1] === "status") {
      const records = await launchStatus(runtimeDir);
      console.log(
        JSON.stringify(
          { note: "Historical launch receipts, not a live-process guarantee.", launches: records },
          null,
          2,
        ),
      );
    } else {
      const app = await discoverApp(values.app);
      const pids = await runningAppPids(app.executable);
      const source = await binarySource(values.binary);
      if (positionals[1] === "doctor")
        console.log(
          JSON.stringify(
            {
              app,
              platform: source.platform,
              version: source.version,
              runningAppPids: pids,
              runtimeDir,
            },
            null,
            2,
          ),
        );
      else {
        if (pids.length)
          throw Error("ZCode is already running. Fully quit it before launching the Rust runtime.");
        const binary = await installBinary(source, runtimeDir);
        const launch = await prepareLaunch({ app, binary, runtimeDir });
        const started = await startApp(app, launch);
        console.log(
          values.json
            ? JSON.stringify({ ...started, launchDir: launch.cwd })
            : `ZCode ${app.version} launched (PID ${started.appPid}). Rust will be selected when Host starts. Run app status to inspect bridge receipts.\nTo return to the bundled CLI, fully quit ZCode and launch its original icon.`,
        );
      }
    }
  }
} catch (error) {
  console.error(`zcode-rust: ${error.message}`);
  process.exitCode = 1;
}
