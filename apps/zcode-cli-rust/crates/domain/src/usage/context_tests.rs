use super::*;

#[test]
fn context_tokens_follow_node_formula() {
    let usage = json!({"inputTokens": 130, "outputTokens": 7, "totalTokens": 137});
    assert_eq!(context_tokens(&usage), Some(137));
    // 没有 input：用 total 减 output 作为输入窗口。
    assert_eq!(
        context_tokens(&json!({"totalTokens": 50, "outputTokens": 5})),
        Some(50)
    );
    // 只有 cache：cache 读写之和作为输入窗口。
    let cache = json!({"cacheReadTokens": 20, "cacheWriteTokens": 10});
    assert_eq!(context_tokens(&cache), Some(30));
    assert_eq!(context_tokens(&json!({})), None);
    assert_eq!(context_tokens(&Value::Null), None);
}

#[test]
fn cache_hits_accumulate_and_truncate() {
    let mut hits = CacheHits::default();
    let usage = json!({"inputTokens": 130, "cacheReadTokens": 20, "cacheWriteTokens": 10});
    let first = hits.record(1, &usage).unwrap();
    assert_eq!(
        first,
        json!({"inputTokens": 130, "cacheReadTokens": 20, "cacheWriteTokens": 10,
            "latestHitRate": 20.0 / 130.0, "hitRate": 20.0 / 130.0, "hitRateRequestCount": 1,
            "totalInputTokens": 130, "totalCacheReadTokens": 20, "totalCacheWriteTokens": 10})
    );
    let second = hits.record(3, &usage).unwrap();
    assert_eq!(second["hitRateRequestCount"], 2);
    assert_eq!(second["totalInputTokens"], 260);
    // 没有任何正值的用量不计数，也没有 cache 成员。
    assert_eq!(hits.record(5, &json!({"outputTokens": 3})), None);
    // 回退到消息 3 之前：只剩第一次请求。
    hits.truncate(3);
    assert_eq!(hits.record(3, &usage).unwrap()["hitRateRequestCount"], 2);
    // 只有 cache 写入：命中率为 0，输入窗口取 cache 之和。
    let mut fresh = CacheHits::default();
    let only_write = fresh.record(0, &json!({"cacheWriteTokens": 8})).unwrap();
    assert_eq!(only_write["inputTokens"], 8);
    assert_eq!(only_write["latestHitRate"], 0.0);
}

#[test]
fn stored_tokens_use_raw_input() {
    let tokens = json!({"input": 100, "output": 5, "cache": {"read": 60, "write": 0}});
    assert_eq!(
        CacheUse::stored(&tokens),
        Some(CacheUse {
            input: 100,
            read: 60,
            write: 0
        })
    );
    let empty = json!({"input": 0, "output": 5, "cache": {"read": 0, "write": 0}});
    assert_eq!(CacheUse::stored(&empty), None);
}

#[test]
fn breakdown_counts_node_categories() {
    let sections = SectionChars {
        system: 100,
        meta_user: 41,
        skills: 0,
    };
    let tools = vec![
        json!({"type": "function", "function": {"name": "Read", "description": "d",
            "parameters": {"type": "object"}}}),
        json!({"type": "function", "function": {"name": "mcp__s__t", "description": "",
            "parameters": {}}}),
    ];
    let messages = vec![
        json!({"role": "system", "content": "ignored"}),
        json!({"role": "user", "content": "<system-reminder>\nctx\n</system-reminder>"}),
        json!({"role": "user", "content": "please use tool now"}),
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function",
            "function": {"name": "Read", "arguments": "{\"file_path\":\"a\"}"}}]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "ok"}),
    ];
    let out = breakdown(sections, &tools, &messages);
    let read = r#"{"name":"Read","description":"d","inputSchema":{"type":"object"},"readOnly":true,"destructive":false,"sideEffectScope":"none"}"#;
    let mcp = r#"{"name":"mcp__s__t","description":"","inputSchema":{},"readOnly":false,"destructive":false,"sideEffectScope":"network"}"#;
    let user = r#"{"role":"user","content":"please use tool now"}"#;
    let assistant = r#"{"role":"assistant","content":"","toolCalls":[{"id":"c1","name":"Read","input":{"file_path":"a"}}]}"#;
    let tool = r#"{"role":"tool","content":"ok","toolCallId":"c1","toolName":"Read"}"#;
    assert_eq!(
        out,
        vec![
            json!({"source": "system_prompt", "chars": 100}),
            json!({"source": "meta_user_context", "chars": 41}),
            json!({"source": "system_tool_schemas", "chars": read.len()}),
            json!({"source": "mcp_tool_schemas", "chars": mcp.len()}),
            json!({"source": "messages", "chars": user.len() + assistant.len() + tool.len()}),
        ]
    );
}

#[test]
fn task_notifications_count_as_messages() {
    let notice = "<system-reminder>\n[SYSTEM NOTIFICATION - NOT USER INPUT]\nx\n</system-reminder>";
    let messages = vec![json!({"role": "user", "content": notice})];
    let out = breakdown(SectionChars::default(), &[], &messages);
    assert_eq!(out[0]["source"], "messages");
    assert_eq!(js_len("é😀"), 3);
}
