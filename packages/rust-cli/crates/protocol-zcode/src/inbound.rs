//! 入站参数的校验（docs/specs/rust-cli/app-protocol.md §7）：schema 由 `scripts/rust-cli/app-protocol/schema.mjs` 从 App 的
//! zod 生成（`schema/inbound.json`），这里按方法、命令类型编译一次，校验失败回 `-32602`（命令回 rejected）。
//! zod 的 superRefine 跨字段规则（schema 表达不了的）在这里手写镜像。

use std::collections::HashMap;
use std::sync::OnceLock;

use rust_cli_protocol_core::rpc::RpcError;
use rust_cli_protocol_core::schema::Node;
use serde_json::Value;

const INBOUND: &str = include_str!("../schema/inbound.json");

/// 编译好的入站 schema。
pub struct Inbound {
    methods: HashMap<String, Node>,
    commands: HashMap<String, Node>,
    replies: HashMap<String, Node>,
    requires_base_revision: Vec<String>,
    requires_base_epoch: Vec<String>,
}

fn compile(section: &Value) -> Result<HashMap<String, Node>, String> {
    section
        .as_object()
        .ok_or("the schema section is not an object")?
        .iter()
        .map(|(name, schema)| {
            Node::compile(schema)
                .map(|node| (name.clone(), node))
                .map_err(|error| format!("{name}: {error}"))
        })
        .collect()
}

fn names(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl Inbound {
    pub fn parse(text: &str) -> Result<Self, String> {
        let data: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
        Ok(Self {
            methods: compile(&data["methods"])?,
            commands: compile(&data["commands"])?,
            replies: compile(&data["replies"])?,
            requires_base_revision: names(&data["requiresBaseRevision"]),
            requires_base_epoch: names(&data["requiresBaseLogEpoch"]),
        })
    }

    /// 随 crate 带的 schema（编译一次）。schema 文件由生成脚本写、`--check` 守着，编译失败是构建错误：测试覆盖。
    pub fn get() -> &'static Self {
        static INBOUND_SCHEMA: OnceLock<Inbound> = OnceLock::new();
        INBOUND_SCHEMA.get_or_init(|| {
            Self::parse(INBOUND).unwrap_or_else(|error| {
                tracing::error!(event = "protocol.schema.invalid", error = %error, "the inbound schema does not compile");
                Self {
                    methods: HashMap::new(),
                    commands: HashMap::new(),
                    replies: HashMap::new(),
                    requires_base_revision: Vec::new(),
                    requires_base_epoch: Vec::new(),
                }
            })
        })
    }

    /// 方法的参数；没有登记 schema 的方法不校验。缺省的参数按 `{}` 校验（JSON-RPC 允许省略 params）。
    pub fn method(&self, method: &str, params: &Value) -> Result<(), RpcError> {
        let Some(node) = self.methods.get(method) else {
            return Ok(());
        };
        let empty = Value::Object(Default::default());
        let params = if params.is_null() { &empty } else { params };
        node.validate(params, "").map_err(|error| {
            RpcError::invalid_params(format!("Invalid params for {method}: {error}"))
                .with_data(serde_json::json!({"issue": error}))
        })
    }

    /// 命令的 payload 与跨字段规则。错误是给 ACK 的说明文字。
    pub fn command(&self, envelope: &Value) -> Result<(), String> {
        let kind = envelope["type"].as_str().unwrap_or_default();
        let node = self
            .commands
            .get(kind)
            .ok_or_else(|| format!("unknown command type {kind}"))?;
        let payload = &envelope["payload"];
        node.validate(payload, "/payload")?;
        if self.requires_base_revision.iter().any(|name| name == kind)
            && envelope.get("baseRevision").is_none_or(Value::is_null)
        {
            return Err("CAS commands require baseRevision and baseLogEpoch".to_owned());
        }
        if self.requires_base_epoch.iter().any(|name| name == kind)
            && envelope.get("baseLogEpoch").is_none_or(Value::is_null)
        {
            return Err("CAS commands require baseRevision and baseLogEpoch".to_owned());
        }
        // sendText 的 superRefine 镜像（command.ts）。
        if kind == "sendText" {
            if payload.get("automationId").is_some() && payload.get("offPeakTaskId").is_some() {
                return Err("automationId and offPeakTaskId are mutually exclusive".to_owned());
            }
            if payload.get("offPeakRunType").is_some() && payload.get("offPeakTaskId").is_none() {
                return Err(
                    "/payload/offPeakRunType: offPeakRunType requires offPeakTaskId".to_owned(),
                );
            }
            if payload.get("modelExecution").is_some() && payload.get("modelSelection").is_none() {
                return Err(
                    "/payload/modelExecution: modelExecution requires modelSelection".to_owned(),
                );
            }
        }
        Ok(())
    }

    /// 本端发出的请求得到的回复。
    pub fn reply(&self, method: &str, result: &Value) -> Result<(), String> {
        match self.replies.get(method) {
            Some(node) => node.validate(result, ""),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "inbound_tests.rs"]
mod tests;
