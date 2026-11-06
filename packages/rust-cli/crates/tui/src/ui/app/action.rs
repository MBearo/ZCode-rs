//! runner 要执行的动作（docs/specs/rust-cli/tui.md §2、§4）：按键产生它，runner 只经内核会话 API 执行。从 `app.rs` 原样移来。
use rust_cli_kernel_api::ids::{InteractionId, MessageId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Delivery, RewindScope};

/// runner 要执行的动作（只经内核会话 API）。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// ⌫ 取回栈上最新的一条排队的消息（Puffin「发出的消息」的「取回」）：请内核出队，成功后文字放回输入框。
    TakeBack {
        command: rust_cli_kernel_api::ids::CommandId,
        text: String,
    },
    /// 发送一条输入；没有会话时先建会话（tui.md §4.1）。
    Submit {
        text: String,
        delivery: Delivery,
    },
    /// 带图片的输入（tui-input.md §3）：文字块在前，图片在后。
    SubmitContent {
        content: Vec<rust_cli_kernel_api::message::ContentBlock>,
        delivery: Delivery,
    },
    /// ctrl+g、ctrl+x ctrl+e：用外部编辑器编辑输入（tui-input.md §4.1）。
    ExternalEditor,
    /// ctrl+v / alt+v：读剪贴板里的图片（tui-input.md §3.1）。
    PasteImage,
    /// 粘贴的文字是图片文件的路径：读出文件，不是图片时把 `text` 当文字粘贴。
    PasteImagePath {
        path: String,
        text: String,
    },
    /// bash 模式的输入（tui-input.md §2）。
    Bash {
        command: String,
        delivery: Delivery,
    },
    /// ctrl+enter（运行中）：低延迟投递，入队后把前台 Bash 转到后台、不中断（修复线 L-A，tui-input.md §11.1）。
    SendNow {
        text: String,
        delivery: Delivery,
    },
    /// ctrl+x ctrl+b（运行中）：前台 Bash 转到后台（修复线 L-A，tui-input.md §11.2）。
    BackgroundForeground,
    /// 输入框为空时的立即发送（修复线 F105，tui.md §5.1.1）：只把前台 Bash 转到后台，排着的插话在工具边界并入。
    DeliverQueued,
    Interrupt,
    /// 几个动作依次做（全部允许、完全访问的确认之后允许这一次再切模式，tui-kit.md §4.2「请求」）。
    Many(Vec<Action>),
    Resolve {
        interaction: InteractionId,
        answer: rust_cli_kernel_api::orchestration::Answer,
    },
    /// 批准或拒绝扣住的同伴消息（cross-session.md §2.6）。
    ResolvePeer {
        message: String,
        approve: bool,
    },
    /// `/model`、meta+p 的选择：先换模型，被接受后再写成新工作区的默认模型（workspace-trust.md §3.2）。`auto`：选的是
    /// 最低一档之前的 `auto`（`reasoning` 为 None），只对本会话，不写默认模型（没有档位的默认在下次启动时回到目录第一项，
    /// workspace-trust.md §3.1）。
    ChooseModel {
        selection: ModelSelection,
        auto: bool,
    },
    SetMode(PermissionMode),
    /// `/plan`：打开或关掉 plan 开关（修复线 F20）。
    SetPlan(bool),
    Compact(Option<String>),
    /// `/model`：读模型目录后打开列表。
    OpenModels,
    /// `/clear`：离开当前会话，下一次发送建新会话。
    Clear,
    /// 复制这段文字（系统剪贴板与 OSC 52，tui-select.md §3.2）。
    Copy(String),
    /// 复制鼠标选区：runner 先重画一次，再取选区文字（`clear` 为真时复制后清掉选区）。
    CopySelection {
        clear: bool,
    },
    /// ctrl+l：清屏并重画。
    Redraw,
    /// `/rewind` 或 esc 连按两次：取可选消息后打开选择器（session-lifecycle.md §5.4）。
    OpenRewind,
    /// 选择器确认：回退到 `target` 之前；对话被回退时把 `text` 放回输入框。
    Rewind {
        target: MessageId,
        scope: RewindScope,
        text: String,
    },
    /// `/resume`：列出会话后打开选择器。
    OpenResume,
    /// 切到这个会话。
    Resume(String),
    /// `/rename 名字`：改会话标题（来源记为 custom，docs/specs/rust-cli/titles.md §4）。
    Rename(String),
    /// 不带名字的 `/rename`：会话 actor 用模型起名（titles.md §8）。
    GenerateName,
    /// 数字的卡或上下文的页开着：问内核上下文的计数（tui-panels.md §4.1；只有一次在途，`runner/services.rs`）。
    CountContext,
    /// 统计的页显示 Session：问它的事实（MCP、hooks、账号、分支；tui-panels.md §4.5，`runner/services.rs`）。
    SessionFacts,
    /// `/locale <值>`：写回配置文件，成功后切换界面语言（tui.md §7.1）。
    SetLocale(crate::services::UiLocale),
    /// 浏览器授权登录（tui.md §7.2）。
    Login(crate::services::Family),
    /// 取消在途的登录（一条消息）。
    CancelLogin,
    /// 保存手工 key（key 不经输入框，不进历史）。
    ConfigureKey {
        family: crate::services::Family,
        key: crate::services::RedactedString,
    },
    /// `/goal` 不带参数：取会话的目标后打开面板（goal.md §6.2）。
    ShowGoal,
    /// 挑选回退的点时的 ⏎（`rewind_pick.rs`，Puffin `go_back`）：在 `message` 上面的回答上 fork 出新会话，回答经
    /// `App::rewound` 交回挑选，新会话的视图到了经 `App::rewound_into` 发出草稿。
    RewindFork {
        message: MessageId,
    },
    /// `/fork`：取可选的用户输入后打开选择器（session-lifecycle.md §6.5.3）。
    OpenFork,
    /// 选择器选中一条输入：新会话带到这一轮为止，成功后切过去。
    Fork {
        input: MessageId,
    },
    Quit,
    /// L28 的管理命令（docs/specs/rust-cli/tui-display.md）。
    Manage(crate::ui::manage::Manage),
    /// L30 的面板（docs/specs/rust-cli/tui-panels.md）。
    Panel(crate::ui::panels::Panel),
    /// ← 项目页与 `/resume` 页（修复线 F29，tui-panels.md §2、tui-manage.md §10）。
    Projects(crate::ui::projects::Request),
    /// `/machines` 的子命令与列表里的动作（远端执行 R4，remote-exec.md §21.3）：经入口的机器服务。
    Machines(crate::ui::machines::MachineRequest),
    /// 设置页要 runner 做的：读段、写改动、打开文件、写主题（tui-kit.md §4.2「设置」）。
    Settings(crate::ui::settings::Request),
    /// `/usage` 的卡与详情页要问入口的（usage_plans.rs）。
    PlanUsage(Vec<crate::ui::usage_plans::PlanAsk>),
    /// 套餐的上限停下了回合 `row`：问一次 `provider` 的计划什么时候回来（usage_back.rs，Puffin `ask_back`）。
    UsageBack {
        row: u64,
        provider: String,
    },
}
