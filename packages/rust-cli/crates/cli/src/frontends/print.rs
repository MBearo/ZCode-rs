//! `-p` 的入口（docs/specs/rust-cli/headless.md；cli-args.md §14.3）：读输入、组装（与交互界面共用 [`crate::assemble`]）、
//! 跑 headless。夹在阶段之间的只有 `--session-id` 的检查；建好内核之后短等 MCP（ext.md §3.3.1）。

use anyhow::Result;
use rust_cli_base::names::env;
use rust_cli_headless::{InputFormat, Output, PrintOptions, Stamps};

use crate::assemble::{self, CliInputs, Controller};
use crate::commands::args::Invocation;
use crate::process::output::eprint_raw;
use crate::process::{ControllerRun, Start};
use crate::{
    VERSION, assemble::setup, commands::flags, commands::print_format, commands::stdin_prompt,
};

pub(crate) async fn run(invocation: Invocation) -> Result<u8> {
    let run = ControllerRun::begin(Start {
        mode: "print",
        surface: None,
    })?;
    let result = print(invocation, &run).await;
    run.finish(result).await
}

async fn print(invocation: Invocation, run: &ControllerRun) -> Result<u8> {
    let ControllerRun {
        facts,
        paths,
        clock,
        stop,
        model_io,
        ..
    } = run;
    let spec = Controller::print(CliInputs::of(&invocation), &facts.hooks);
    let Invocation {
        prompt,
        target,
        fork_session,
        session_id,
        format,
        ..
    } = invocation;
    // `-p` 的输入：prompt 参数与管道里的文字（cli-args.md §2.3）。stream-json 输入时 stdin 是逐行的 user 消息
    // （headless.md §4），不读成 prompt，prompt 参数忽略。
    let prompt = match format.input {
        InputFormat::StreamJson => prompt.unwrap_or_default(),
        InputFormat::Text => stdin_prompt::print_prompt(prompt).await?,
    };
    // 读到输入之后的校验（headless.md §1.1 第 5、6 条）：只有空白的输入、stream-json 输出没有 `--verbose`。
    print_format::check_input(format, &prompt).map_err(flags::Fatal)?;
    let opened = assemble::open(&spec, facts, (paths, clock)).await?;
    // `--session-id`（cli-args.md §7）：会话 ID 用 Node 的写法 `sess_<uuid>`；已有这个会话时报错退出。
    let session_id = match session_id {
        Some(uuid) => Some(setup::fresh_session_id(opened.store.as_ref(), &uuid).await?),
        None => None,
    };
    let ported = assemble::ports(&spec, facts, opened, (clock, model_io)).await?;
    let built = assemble::kernel(&spec, facts, ported, clock).await?;
    // 等 MCP 连接期间收到退出信号：不再等（headless 看到令牌已取消就不开回合）。
    tokio::select! {
        () = built.kernel.wait_for_extensions() => {}
        () = stop.token.cancelled() => {}
    }
    let output = Output {
        format: format.print_format(),
        stamps: Some(Stamps {
            ids: built.opened.ids.clone(),
            clock: clock.clone(),
        }),
        version: VERSION.to_owned(),
        // system/init 的 `apiKeySource`（headless.md §3.3）：key 来自哪个变量（本工程读 `ZCODE_API_KEY`，config.md §2.5；
        // 显式覆盖时没有模型目录）。
        api_key_source: if built.opened.directory.is_none() {
            env::ZCODE_API_KEY
        } else {
            "none"
        }
        .to_owned(),
    };
    let options = PrintOptions {
        prompt,
        session_id,
        workspace: built.opened.workspace.clone(),
        // `-p` 组装时已等缺省模型定下（`Controller::print` 的 `wait_model`）。
        selection: built.selection.current(),
        mode: built.mode,
        target,
        fork: fork_session,
        stop: stop.token.clone(),
        cancel: stop.cancel.clone(),
        output,
    };
    let kernel = built.kernel.clone();
    let stdout = &mut tokio::io::stdout();
    let printed = match format.input {
        InputFormat::Text => rust_cli_headless::run(kernel, options, stdout).await,
        // stdin 每行一条 user 消息（headless.md §4）。
        InputFormat::StreamJson => {
            let input = tokio::io::BufReader::new(tokio::io::stdin());
            rust_cli_headless::run_stream(kernel, options, input, stdout).await
        }
    };
    if let Some(peers) = built.peers {
        peers.shutdown().await;
    }
    printed?;
    // 未信任的项目 hooks 被跳过：按 Node 写一段提示到 stderr（config.md §4.4）。
    if let Some(pending) = &built.opened.config.pending {
        eprint_raw(&pending.notice());
    }
    Ok(0)
}
