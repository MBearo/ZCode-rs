// 语义复制自 apps/zcode-cli/packages/bootstrap/src/app/legacy-cli-personal-provider-config-importer.ts 与
// legacy-cli-model-config.ts 的判定部分（修复线 F2）。
//! 旧 CLI 用户文件 `~/.zcode/cli/config.json` 里有没有 Node 要导入的 provider 或默认模型。
//!
//! Node 在个人 Provider 配置 `provider_config.json` 不存在时、第一次读或写之前导入旧文件的 provider 与 `model`
//! （`NodePersonalProviderConfigRepository` 的 `importLegacy`），文件一旦存在就不再导入。本工程不实现这份导入
//! （Provider 规则的编码要整套复制，platform.md §2.4），所以文件不存在、旧文件又有要导入的内容时不抢先建文件，
//! 留给 Node 或 App 导入；没有要导入的内容时，Node 导入的是空配置，本工程直接写与它相同。

use serde_json::Value;

/// Node `RETIRED_ZAPI_PROVIDER_ID`：旧默认模型指向它时不导入。
const RETIRED_ZAPI_PROVIDER_ID: &str = "builtin:zapi";
/// Node 只为这两个旧内置 provider 迁移 key。
const BUILTIN_TEMPLATES: [&str; 2] = ["builtin:bigmodel", "builtin:zai"];

/// Node `parseModelTarget`：`provider/model`，两边去空白后都非空。
fn target(value: Option<&Value>) -> Option<(String, String)> {
    let (provider, model) = value?.as_str()?.split_once('/')?;
    let (provider, model) = (provider.trim(), model.trim());
    (!provider.is_empty() && !model.is_empty()).then(|| (provider.to_owned(), model.to_owned()))
}

/// Node `importLegacyCliConfiguredDefault`：`model` 是字符串，或 `{main}`。
fn has_default(config: &Value) -> bool {
    let main = match &config["model"] {
        Value::String(_) => target(Some(&config["model"])),
        Value::Object(model) => target(model.get("main")),
        _ => None,
    };
    main.is_some_and(|(provider, _)| provider != RETIRED_ZAPI_PROVIDER_ID)
}

/// 旧文件（已解析的 JSON）里有 Node 要导入的内容。Node 遇到 `apiKeyRequired: false` 的自定义 provider 时整份放弃
/// 导入（`UnsupportedLegacyCliProviderConfigError` → null），这时也没有要导入的内容。
pub(crate) fn pending(config: &Value) -> bool {
    let mut providers = false;
    for (raw, provider) in config["provider"].as_object().into_iter().flatten() {
        let id = raw.trim();
        if id.is_empty() {
            continue;
        }
        let options = &provider["options"];
        if BUILTIN_TEMPLATES.contains(&id) {
            providers |= options["apiKey"]
                .as_str()
                .is_some_and(|key| !key.trim().is_empty());
            continue;
        }
        if id.starts_with("builtin:") || id.starts_with("account:") {
            continue;
        }
        if provider
            .get("source")
            .is_some_and(|source| source != "custom")
        {
            continue;
        }
        if options["apiKeyRequired"] == false {
            return false;
        }
        providers = true;
    }
    providers || has_default(config)
}

#[cfg(test)]
#[path = "legacy_tests.rs"]
mod tests;
