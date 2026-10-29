# @mbearo/zcode

Standalone ZCode Rust CLI.

```sh
npm install -g @mbearo/zcode
zcode
```

Or run `npx @mbearo/zcode`.

Requires Node.js 22.16 or newer. Prebuilt binaries are provided for macOS,
Linux with glibc 2.35 or newer, and Windows. macOS supports ARM64; Linux and Windows support x64
and ARM64.
Alpine/musl is not supported by this package.

npm installs the matching platform package automatically. Keep optional
dependencies enabled. No Rust toolchain or installation script is required.

The command is `zcode`. On macOS and Linux it starts the native program
`zcode-rs`, so process lists (`ps`, Activity Monitor) show `zcode-rs`. On
Windows the native program is `zcode.exe`, and Task Manager shows `zcode.exe`.
Each platform package includes `THIRD-PARTY-NOTICES.txt` for the bundled
ripgrep, bfs and ugrep.

Updates: `zcode update`, or **Update now** on the startup prompt, runs
`npm install -g @mbearo/zcode` (or the pnpm / bun equivalent). On Windows a
running `zcode.exe` cannot be replaced, so ZCode exits first and the `zcode`
command then runs the package manager itself; the output is the same. When
ZCode cannot tell how it was installed (for example when you run the native
program directly), `zcode update` asks you to update manually from
https://www.npmjs.com/package/@mbearo/zcode.

Ctrl+C, Ctrl+Break and closing the console reach `zcode.exe` directly; the
`zcode` command waits for it to finish and exits with its exit code.

This package runs the independent Rust CLI. It does not replace the Desktop
application's bundled runtime.

Source and issues: https://github.com/MBearo/ZCode-rs
