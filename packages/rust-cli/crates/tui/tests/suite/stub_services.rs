//! 入口服务的替身（架构整理 U13）：入口总是给齐服务，测试不关心的服务用这里的。写入都成功、不写文件；剪贴板没有图片、
//! 复制只走 OSC 52；编辑器原样交回；工作区都已信任；登录不结束。
use std::sync::Arc;

use futures_util::future::BoxFuture;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_tui::services::{CancellationToken, RedactedString, WorkspaceTrust};
use rust_cli_tui::trust::TrustPrompt;
use rust_cli_tui::{
    Account, Authorize, Clipboard, Configured, CopyRoute, Editor, Family, ImageBytes, LocaleStore,
    LoginFailure, Preferences, Services, UiLocale,
};
use tokio::sync::oneshot;

/// 写入的“路径”（提示里会写出来）。
pub const SAVED_TO: &str = "/Users/dev/.zcode/config.json";

pub struct Stub;

impl LocaleStore for Stub {
    fn save(&self, _locale: UiLocale) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Ok(SAVED_TO.to_owned()) })
    }
}

impl Preferences for Stub {
    fn save_default_model(
        &self,
        _selection: ModelSelection,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Ok(SAVED_TO.to_owned()) })
    }
}

impl Account for Stub {
    fn login(
        &self,
        _family: Family,
        _authorize: oneshot::Sender<Authorize>,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Configured, LoginFailure>> {
        Box::pin(std::future::pending())
    }

    fn configure_api_key(
        &self,
        _family: Family,
        _key: RedactedString,
    ) -> BoxFuture<'static, Result<Configured, String>> {
        Box::pin(std::future::pending())
    }

    fn logout(&self) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Ok(SAVED_TO.to_owned()) })
    }
}

impl Clipboard for Stub {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn image_file(&self, _path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        Box::pin(async { Ok(None) })
    }
}

impl Editor for Stub {
    fn edit(&self, text: String) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async move { Ok(text) })
    }

    fn open(&self, _path: String) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }

    fn source(&self) -> Option<(String, String)> {
        None
    }
}

impl WorkspaceTrust for Stub {
    fn check(&self, _workspace: WorkspaceRef) -> BoxFuture<'static, Option<TrustPrompt>> {
        Box::pin(async { None })
    }

    fn accept(&self, _workspace: WorkspaceRef) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// 全是替身的服务；没有远端机器，本分支的可选端口（存储、看板、压缩阈值、文件长度、套餐用量、看文件）都没有。
pub fn services() -> Services {
    let stub = Arc::new(Stub);
    Services {
        locale_store: stub.clone(),
        account: stub.clone(),
        clipboard: stub.clone(),
        editor: stub.clone(),
        preferences: stub.clone(),
        trust: stub,
        machines: None,
        stores: None,
        cockpit: None,
        compact_at: None,
        file_len: None,
        plan_usage: None,
        peek_file: None,
    }
}

/// 剪贴板替身（架构整理 U15：以前 3 个测试文件各写一份）：`image` 给出 `image`；`image_file` 只认 `files` 里的路径；
/// 复制的文字记在 `texts`，按 `route` 回答（缺省 `Native`）。
#[derive(Default)]
pub struct FakeClipboard {
    pub image: Option<Vec<u8>>,
    pub files: Vec<(String, Vec<u8>)>,
    pub route: Option<CopyRoute>,
    pub texts: std::sync::Mutex<Vec<String>>,
}

impl Clipboard for FakeClipboard {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        let bytes = self.image.clone();
        Box::pin(async move { Ok(bytes.map(|bytes| ImageBytes { bytes })) })
    }

    fn image_file(&self, path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        let bytes = self
            .files
            .iter()
            .find(|(file, _)| *file == path)
            .map(|(_, bytes)| bytes.clone());
        Box::pin(async move { Ok(bytes.map(|bytes| ImageBytes { bytes })) })
    }

    fn copy_text(&self, text: String) -> BoxFuture<'static, CopyRoute> {
        self.texts.lock().unwrap().push(text);
        let route = self.route.unwrap_or(CopyRoute::Native);
        Box::pin(async move { route })
    }
}
