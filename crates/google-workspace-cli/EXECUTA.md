# gws-executa 测试说明

本文档提供可直接复制执行的命令，用于测试单文件 Anna Executa 插件二进制 `gws-executa`。

## 构建

在仓库根目录执行：

```bash
cargo build -p google-workspace-cli --bin gws-executa
```

或者在当前 crate 目录使用辅助脚本：

```bash
./build_binary.sh --test
```

## 二进制路径

Debug 构建产物：

```bash
./cli/target/debug/gws-executa
```

Release 构建产物：

```bash
./cli/target/release/gws-executa
```

## 协议关键规则

Executa 协议要求一行只发送一条 JSON-RPC 消息。如果你用 `jq` 生成 JSON，请始终加上 `jq -c`，保证输出是单行 JSON。

`run_gws` 现在统一使用 file transport。也就是说：

- `describe` 和 `health` 会直接把 JSON 返回到 stdout
- `invoke` 会先返回一个带 `__file_transport` 的指针 JSON
- 你需要再读取这个临时文件，才能拿到完整响应

临时文件默认生成在插件二进制所在目录。你也可以在 `arguments.cwd` 里传一个已存在目录，显式指定 file transport 临时目录。

## bash / zsh

先设置二进制路径：

```bash
BIN="./cli/target/debug/gws-executa"
```

测试 `describe`：

```bash
echo '{"jsonrpc":"2.0","method":"describe","id":1}' | "$BIN" | jq .
```

测试 `health`：

```bash
echo '{"jsonrpc":"2.0","method":"health","id":2}' | "$BIN" | jq .
```

调用 `--version`：

```bash
PTR=$(echo '{"jsonrpc":"2.0","method":"invoke","id":3,"params":{"tool":"run_gws","arguments":{"argv":["--version"]},"context":{"credentials":{"GOOGLE_WORKSPACE_CLI_TOKEN":"dummy-token"}}}}' | "$BIN")
FILE=$(echo "$PTR" | jq -r '."__file_transport"')
cat "$FILE" | jq .
rm -f "$FILE"
```

调用 `--help`：

```bash
PTR=$(echo '{"jsonrpc":"2.0","method":"invoke","id":4,"params":{"tool":"run_gws","arguments":{"argv":["--help"]},"context":{"credentials":{"GOOGLE_WORKSPACE_CLI_TOKEN":"dummy-token"}}}}' | "$BIN")
FILE=$(echo "$PTR" | jq -r '."__file_transport"')
cat "$FILE" | jq .
rm -f "$FILE"
```

准备访问 token：

```bash
TOKEN="$(gcloud auth print-access-token)"
```

列出 Gmail 邮件：

```bash
PTR=$(jq -c -n --arg token "$TOKEN" '{
  jsonrpc: "2.0",
  method: "invoke",
  id: 5,
  params: {
    tool: "run_gws",
    arguments: {
      argv: ["gmail", "users", "messages", "list", "--params", "{\"userId\":\"me\",\"maxResults\":5}"],
      cwd: "/tmp"
    },
    context: {
      credentials: {
        GOOGLE_WORKSPACE_CLI_TOKEN: $token
      }
    }
  }
}' | "$BIN")
FILE=$(echo "$PTR" | jq -r '."__file_transport"')
cat "$FILE" | jq .
rm -f "$FILE"
```

读取 Gmail profile：

```bash
PTR=$(jq -c -n --arg token "$TOKEN" '{
  jsonrpc: "2.0",
  method: "invoke",
  id: 6,
  params: {
    tool: "run_gws",
    arguments: {
      argv: ["gmail", "users", "getProfile", "--params", "{\"userId\":\"me\"}"]
    },
    context: {
      credentials: {
        GOOGLE_WORKSPACE_CLI_TOKEN: $token
      }
    }
  }
}' | "$BIN")
FILE=$(echo "$PTR" | jq -r '."__file_transport"')
cat "$FILE" | jq .
rm -f "$FILE"
```

如果你想用凭据文件，而不是直接传 token：

```bash
PTR=$(jq -c -n --arg cred_file "/absolute/path/to/credentials.json" '{
  jsonrpc: "2.0",
  method: "invoke",
  id: 7,
  params: {
    tool: "run_gws",
    arguments: {
      argv: ["gmail", "users", "getProfile", "--params", "{\"userId\":\"me\"}"]
    },
    context: {
      credentials: {
        GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE: $cred_file
      }
    }
  }
}' | "$BIN")
FILE=$(echo "$PTR" | jq -r '."__file_transport"')
cat "$FILE" | jq .
rm -f "$FILE"
```

## fish

先设置二进制路径：

```fish
set BIN ./cli/target/debug/gws-executa
```

准备访问 token：

```fish
set TOKEN (gcloud auth print-access-token)
```

测试 `describe`：

```fish
echo '{"jsonrpc":"2.0","method":"describe","id":1}' | $BIN | jq .
```

测试 `health`：

```fish
echo '{"jsonrpc":"2.0","method":"health","id":2}' | $BIN | jq .
```

调用 `--version`：

```fish
set PTR (echo '{"jsonrpc":"2.0","method":"invoke","id":3,"params":{"tool":"run_gws","arguments":{"argv":["--version"]},"context":{"credentials":{"GOOGLE_WORKSPACE_CLI_TOKEN":"dummy-token"}}}}' | $BIN)
set FILE (echo $PTR | jq -r '."__file_transport"')
cat $FILE | jq .
rm -f $FILE
```

列出 Gmail 邮件：

```fish
set PTR (jq -c -n --arg token "$TOKEN" '{
  jsonrpc: "2.0",
  method: "invoke",
  id: 5,
  params: {
    tool: "run_gws",
    arguments: {
      argv: ["gmail", "users", "messages", "list", "--params", "{\"userId\":\"me\",\"maxResults\":5}"],
      cwd: "/tmp"
    },
    context: {
      credentials: {
        GOOGLE_WORKSPACE_CLI_TOKEN: $token
      }
    }
  }
}' | $BIN)
set FILE (echo $PTR | jq -r '."__file_transport"')
cat $FILE | jq .
rm -f $FILE
```

## 说明

- `run_gws` 的完整响应不在 stdout，而是在 `__file_transport` 指向的临时文件里。
- `context.credentials` 里可以传 `GOOGLE_WORKSPACE_CLI_TOKEN` 或 `GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE`，两者至少提供一个。
- `run_gws` 执行成功时返回顶层 `result`；执行失败时返回顶层 `error`，符合 Executa JSON-RPC 约定。
- 调试失败时优先看 `error.data.tool_data.stderr` 和 `error.data.tool_data.stdout_json`。
- 如果你看到一连串 parse error，通常说明传入的是多行 JSON，而不是单行 JSON-RPC 消息。

## token导出

```shell
gws auth setup
gws auth login
gws auth export --unmasked > credentials.json
```

* scopes 至少要包含：
  * auth/gmail.send
  * auth/gmail.modify
  * auth/gmail.compose
  * auth/gmail.readonly	