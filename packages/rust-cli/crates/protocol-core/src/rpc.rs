//! JSON-RPC 形状的消息（docs/specs/rust-cli/app-protocol.md §3.3、§4.1）：请求、回复、错误回复与通知。
//!
//! 没有 `"jsonrpc"` 字段（对端的 schema 是 strict）；id 是字符串或整数，回复时原样带回。入站的消息允许多余的字段
//! （例如追踪信息），由协议 crate 决定要不要看。

use serde_json::{Map, Value, json};

/// 标准错误码。
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// 错误回复的 `error`。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message} ({code})")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// 方法没有实现：文字是 `Method not found: <method>`（对端按码判断）。
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, message)
    }

    fn to_value(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".to_owned(), json!(self.code));
        error.insert("message".to_owned(), json!(self.message));
        if let Some(data) = &self.data {
            error.insert("data".to_owned(), data.clone());
        }
        Value::Object(error)
    }

    fn from_value(value: &Value) -> Self {
        Self {
            code: value.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            data: value.get("data").cloned(),
        }
    }
}

/// 读到的一条消息。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
}

/// 解析失败：`id` 是能认出的请求 id（认不出时不回复，只写诊断）。
#[derive(Debug, Clone, PartialEq)]
pub struct Malformed {
    pub id: Option<Value>,
    pub error: RpcError,
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()
}

/// 解析一行。
pub fn parse(line: &str) -> Result<Incoming, Box<Malformed>> {
    let value: Value = serde_json::from_str(line).map_err(|error| {
        Box::new(Malformed {
            id: None,
            error: RpcError::new(codes::PARSE_ERROR, format!("Parse error: {error}")),
        })
    })?;
    let Value::Object(mut object) = value else {
        return Err(Box::new(Malformed {
            id: None,
            error: RpcError::new(codes::INVALID_REQUEST, "Invalid request: not an object"),
        }));
    };
    let id = object.remove("id");
    let invalid = |id: Option<Value>, message: &str| {
        Box::new(Malformed {
            id,
            error: RpcError::new(
                codes::INVALID_REQUEST,
                format!("Invalid request: {message}"),
            ),
        })
    };
    if let Some(id) = &id
        && !valid_id(id)
    {
        return Err(invalid(None, "the id must be a string or an integer"));
    }
    let params = object.remove("params").unwrap_or(Value::Null);
    match (object.remove("method"), id) {
        (Some(Value::String(method)), Some(id)) => Ok(Incoming::Request { id, method, params }),
        (Some(Value::String(method)), None) => Ok(Incoming::Notification { method, params }),
        (Some(_), id) => Err(invalid(id, "the method must be a string")),
        (None, Some(id)) => {
            if let Some(error) = object.get("error") {
                return Ok(Incoming::Response {
                    id,
                    result: Err(RpcError::from_value(error)),
                });
            }
            match object.remove("result") {
                Some(result) => Ok(Incoming::Response {
                    id,
                    result: Ok(result),
                }),
                None => Err(invalid(Some(id), "missing method, result or error")),
            }
        }
        (None, None) => Err(invalid(None, "missing method and id")),
    }
}

/// 回复一行（不带换行）。
pub fn response(id: &Value, result: &Result<Value, RpcError>) -> String {
    let value = match result {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error) => json!({"id": id, "error": error.to_value()}),
    };
    value.to_string()
}

/// 请求一行。
pub fn request(id: &Value, method: &str, params: &Value) -> String {
    json!({"id": id, "method": method, "params": params}).to_string()
}

/// 通知一行。
pub fn notification(method: &str, params: &Value) -> String {
    json!({"method": method, "params": params}).to_string()
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
