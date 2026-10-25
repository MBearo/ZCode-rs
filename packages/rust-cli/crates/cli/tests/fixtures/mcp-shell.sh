#!/usr/bin/env bash
# 远端 MCP 的测试夹具（docs/specs/rust-cli/remote-exec.md §22.10）：最小的 stdio MCP 服务器，一行一条 JSON-RPC 消息。
# 只用 bash 与系统自带的命令（macOS 的 bash 3.2 与测试容器里的 bash 5 都能跑），不装任何包。
# 工具：where（这个进程的 pid、所在目录、系统、看到的 MCP_PROBE 与当前目录的 notes.txt）、slow（等几秒再回答）。
re_id='"id":([0-9]+)'
re_method='"method":"([^"]+)"'
re_tool='"name":"(where|slow)"'
re_seconds='"seconds":([0-9]+)'

esc() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  s=${s//$'\t'/\\t}
  s=${s//$'\r'/}
  printf '%s' "$s"
}

reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$1" "$2"; }

text() { reply "$1" "{\"content\":[{\"type\":\"text\",\"text\":\"$(esc "$2")\"}]}"; }

while IFS= read -r line; do
  # 通知（没有 id）不回答。
  [[ $line =~ $re_id ]] || continue
  id=${BASH_REMATCH[1]}
  [[ $line =~ $re_method ]] || continue
  method=${BASH_REMATCH[1]}
  case $method in
    initialize)
      reply "$id" '{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"shell-fixture","version":"1.0.0"}}'
      ;;
    tools/list)
      reply "$id" '{"tools":[{"name":"where","description":"Report where this server runs.","inputSchema":{"type":"object","properties":{}}},{"name":"slow","description":"Wait, then answer.","inputSchema":{"type":"object","properties":{"seconds":{"type":"number"}}}}]}'
      ;;
    tools/call)
      tool=
      [[ $line =~ $re_tool ]] && tool=${BASH_REMATCH[1]}
      case $tool in
        where)
          text "$id" "pid=$$
cwd=$(pwd)
os=$(uname -s)
probe=${MCP_PROBE-}
notes=$(cat notes.txt 2>/dev/null)"
          ;;
        slow)
          seconds=1
          [[ $line =~ $re_seconds ]] && seconds=${BASH_REMATCH[1]}
          sleep "$seconds"
          text "$id" "slept $seconds"
          ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"unknown tool"}}\n' "$id"
          ;;
      esac
      ;;
    ping)
      reply "$id" '{}'
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id"
      ;;
  esac
done
