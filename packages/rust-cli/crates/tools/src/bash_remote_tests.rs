//! 远端机器上的 Bash（docs/specs/rust-cli/remote-exec.md §25.2.3，F98）：结果里写了输出文件的路径时，写明那个文件在哪台
//! 机器上（文件在那台机器上，不写 `machine` 的 Read 在本机找不到它）。本机的调用逐字不变。

use super::*;

const NOTE: &str =
    "The output file is on machine \"dev-a\"; pass machine: \"dev-a\" when you Read it.";

/// `env` 当成远端机器 dev-a 上的执行环境。
async fn on_dev_a(env: Arc<Scripted>, input: Value) -> String {
    let mut context = context(env.clone(), None);
    context.machine = machine(env).named("dev-a");
    let (progress, _updates) = mpsc::channel(8);
    let output = BashTool::new()
        .call(input, context, progress)
        .await
        .unwrap();
    text_of(&output).to_owned()
}

async fn locally(env: Arc<Scripted>, input: Value) -> String {
    text_of(&call(env, input, None).await.unwrap()).to_owned()
}

#[tokio::test]
async fn remote_output_files_name_their_machine() {
    let task = output::task_id(&OperationId::new("op_1"));
    // 后台启动。
    let held = || {
        Arc::new(Scripted {
            hold: true,
            ..Scripted::default()
        })
    };
    let background =
        json!({"command": "sleep 9", "description": "Wait", "run_in_background": true});
    let started = output::backgrounded(&task, "/t/b.output");
    assert_eq!(
        on_dev_a(held(), background.clone()).await,
        format!("{started}\n{NOTE}")
    );
    assert_eq!(locally(held(), background).await, started);
    // 到了超时、转到后台。
    let moving = || {
        Arc::new(Scripted {
            output: b"start\n".to_vec(),
            backgrounded: true,
            ..Scripted::default()
        })
    };
    let slow = json!({"command": "npm test", "timeout": 1000});
    let moved = locally(moving(), slow.clone()).await;
    assert!(!moved.contains("on machine"), "{moved}");
    assert_eq!(on_dev_a(moving(), slow).await, format!("{moved}\n{NOTE}"));
    // 输出太长、存进文件（`<persisted-output>`）。
    let long = || {
        Arc::new(Scripted {
            output: "line of output\n".repeat(3000).into_bytes(),
            ..Scripted::default()
        })
    };
    let loud = json!({"command": "cat big.log"});
    let persisted = locally(long(), loud.clone()).await;
    assert!(persisted.contains("<persisted-output>"), "{persisted}");
    assert_eq!(on_dev_a(long(), loud).await, format!("{persisted}\n{NOTE}"));
    // 短输出不提文件：远端也不加。
    let short = Arc::new(Scripted {
        output: b"hi\n".to_vec(),
        ..Scripted::default()
    });
    assert_eq!(on_dev_a(short, json!({"command": "echo hi"})).await, "hi");
}
