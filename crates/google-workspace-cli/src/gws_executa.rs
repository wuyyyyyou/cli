use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const MAX_STDIO_MESSAGE_BYTES: usize = 512 * 1024;
const TOOL_NAME: &str = "run_gws";
const PLUGIN_NAME: &str = "gws-executa";
const TOKEN_CREDENTIAL_NAME: &str = "GOOGLE_WORKSPACE_CLI_TOKEN";
const INTERNAL_MODE_ARG: &str = "__anna_run_gws_internal";

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    id: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Debug, Serialize)]
struct FileTransportPointer {
    jsonrpc: &'static str,
    id: Value,
    #[serde(rename = "__file_transport")]
    file_transport: String,
}

#[derive(Debug, Default, Deserialize)]
struct InvokeContext {
    #[serde(default)]
    credentials: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct InvokeParams {
    tool: String,
    #[serde(default)]
    arguments: Map<String, Value>,
    #[serde(default)]
    context: InvokeContext,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
    success: bool,
    executable: String,
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if is_internal_cli_mode(&args) {
        run_internal_cli(args).await;
        return;
    }

    run_rpc_server();
}

async fn run_internal_cli(args: Vec<String>) {
    google_workspace_cli::initialize_process();
    let cli_args = internal_cli_args(&args);

    if let Err(err) = google_workspace_cli::run_cli_with_args(cli_args).await {
        google_workspace_cli::print_error_json(&err);
        std::process::exit(err.exit_code());
    }
}

fn run_rpc_server() {
    let current_exe = std::env::current_exe().ok();
    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let response = match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => match serde_json::from_value::<RpcRequest>(value) {
                        Ok(request) => handle_request(request, current_exe.as_deref()),
                        Err(err) => error_response(
                            Value::Null,
                            -32600,
                            "Invalid request",
                            Some(json!({ "details": err.to_string() })),
                        ),
                    },
                    Err(err) => error_response(
                        Value::Null,
                        -32700,
                        "Parse error",
                        Some(json!({ "details": err.to_string() })),
                    ),
                };

                if let Err(err) = send_response(&response) {
                    let _ = writeln!(io::stderr(), "failed to send response: {err}");
                    break;
                }
            }
            Err(err) => {
                let _ = writeln!(io::stderr(), "failed to read stdin: {err}");
                break;
            }
        }
    }
}

fn handle_request(request: RpcRequest, current_exe: Option<&Path>) -> RpcResponse {
    if request.jsonrpc != "2.0" || request.method.is_empty() {
        return error_response(
            request.id,
            -32600,
            "Invalid request",
            Some(json!({
                "expected_jsonrpc": "2.0",
                "expected_fields": ["jsonrpc", "method", "id"],
            })),
        );
    }

    match request.method.as_str() {
        "describe" => success_response(request.id, manifest()),
        "health" => success_response(
            request.id,
            json!({
                "status": "healthy",
                "timestamp": Utc::now().to_rfc3339(),
                "version": env!("CARGO_PKG_VERSION"),
                "tools_count": 1,
            }),
        ),
        "invoke" => handle_invoke(request.id, request.params, current_exe),
        _ => error_response(
            request.id,
            -32601,
            format!("Method not found: {}", request.method),
            None,
        ),
    }
}

fn handle_invoke(id: Value, params: Value, current_exe: Option<&Path>) -> RpcResponse {
    let params: InvokeParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(err) => {
            return error_response(
                id,
                -32602,
                "Invalid params",
                Some(json!({ "details": err.to_string() })),
            );
        }
    };

    if params.tool != TOOL_NAME {
        return error_response(
            id,
            -32601,
            format!("Unknown tool: {}", params.tool),
            Some(json!({ "available_tools": [TOOL_NAME] })),
        );
    }

    let argv = match extract_argv(&params.arguments) {
        Ok(argv) => argv,
        Err(message) => return error_response(id, -32602, message, None),
    };

    let Some(current_exe) = current_exe else {
        return error_response(
            id,
            -32603,
            "Failed to resolve current executable path",
            None,
        );
    };

    let token = params
        .context
        .credentials
        .get(TOKEN_CREDENTIAL_NAME)
        .filter(|value| !value.is_empty())
        .cloned();

    let output = match execute_embedded_gws(current_exe, &argv, token.as_deref()) {
        Ok(output) => output,
        Err(err) => {
            return error_response(
                id,
                -32603,
                format!(
                    "Failed to execute embedded gws mode from '{}': {err}",
                    current_exe.display()
                ),
                None,
            );
        }
    };

    success_response(
        id,
        json!({
            "success": output.success,
            "tool": TOOL_NAME,
            "data": build_tool_data(&output, &argv),
        }),
    )
}

fn manifest() -> Value {
    json!({
        "name": PLUGIN_NAME,
        "display_name": "Google Workspace CLI",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Single-file Anna Executa wrapper with embedded gws runtime.",
        "author": "googleworkspace/cli",
        "credentials": [
            {
                "name": TOKEN_CREDENTIAL_NAME,
                "display_name": "Google Workspace Access Token",
                "description": "OAuth2 access token forwarded to the embedded gws runtime as GOOGLE_WORKSPACE_CLI_TOKEN.",
                "required": true,
                "sensitive": true
            }
        ],
        "tools": [
            {
                "name": TOOL_NAME,
                "description": "Run a gws command with a structured argv array. Do not include shell quoting or a leading gws binary name.",
                "parameters": [
                    {
                        "name": "argv",
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "gws arguments as a string array, for example [\"drive\", \"files\", \"list\", \"--params\", \"{\\\"pageSize\\\":5}\"].",
                        "required": true
                    }
                ]
            }
        ],
        "runtime": {
            "type": "binary",
            "min_version": "1.0.0"
        }
    })
}

fn extract_argv(arguments: &Map<String, Value>) -> Result<Vec<String>, String> {
    let raw_argv = arguments
        .get("argv")
        .and_then(Value::as_array)
        .ok_or_else(|| "Missing required array argument: argv".to_string())?;

    let mut argv = Vec::with_capacity(raw_argv.len());
    for value in raw_argv {
        let arg = value
            .as_str()
            .ok_or_else(|| "Each argv entry must be a string".to_string())?;
        validate_command_arg(arg)?;
        argv.push(arg.to_string());
    }

    if matches!(argv.first().map(String::as_str), Some("gws" | "gws.exe")) {
        argv.remove(0);
    }

    if argv.is_empty() {
        return Err("argv must contain at least one gws argument".to_string());
    }

    Ok(argv)
}

fn validate_command_arg(arg: &str) -> Result<(), String> {
    if arg == INTERNAL_MODE_ARG {
        return Err("Reserved internal command argument is not allowed".to_string());
    }

    if arg.chars().any(char::is_control) {
        return Err("Command arguments must not contain control characters".to_string());
    }

    Ok(())
}

fn is_internal_cli_mode(args: &[String]) -> bool {
    matches!(args.get(1).map(String::as_str), Some(INTERNAL_MODE_ARG))
}

fn internal_cli_args(plugin_args: &[String]) -> Vec<String> {
    let mut cli_args = vec!["gws".to_string()];
    cli_args.extend(plugin_args.iter().skip(2).cloned());
    cli_args
}

fn execute_embedded_gws(
    current_exe: &Path,
    argv: &[String],
    token: Option<&str>,
) -> io::Result<CommandOutput> {
    let mut command = build_embedded_gws_command(current_exe, argv, token);
    let output = command.output()?;
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        success: output.status.success(),
        executable: current_exe.display().to_string(),
    })
}

fn build_embedded_gws_command(current_exe: &Path, argv: &[String], token: Option<&str>) -> Command {
    let mut command = Command::new(current_exe);
    command
        .arg(INTERNAL_MODE_ARG)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(token) = token {
        command.env(TOKEN_CREDENTIAL_NAME, token);
    }

    command
}

fn build_tool_data(output: &CommandOutput, argv: &[String]) -> Value {
    let mut data = Map::new();
    data.insert("argv".to_string(), json!(argv));
    data.insert(
        "plugin_executable".to_string(),
        Value::String(output.executable.clone()),
    );
    data.insert("stdout".to_string(), Value::String(output.stdout.clone()));
    data.insert("stderr".to_string(), Value::String(output.stderr.clone()));
    data.insert("exit_code".to_string(), Value::from(output.exit_code));
    data.insert("embedded_cli".to_string(), Value::Bool(true));

    if let Ok(parsed) = serde_json::from_str::<Value>(output.stdout.trim()) {
        data.insert("stdout_json".to_string(), parsed);
    }

    Value::Object(data)
}

fn send_response(response: &RpcResponse) -> io::Result<()> {
    let payload = encode_json(response)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    if payload.len() > MAX_STDIO_MESSAGE_BYTES {
        let mut temp = tempfile::Builder::new()
            .prefix("executa-resp-")
            .suffix(".json")
            .tempfile()?;
        temp.write_all(&payload)?;
        let (_file, path) = temp.keep().map_err(|err| err.error)?;
        let pointer = FileTransportPointer {
            jsonrpc: "2.0",
            id: response.id.clone(),
            file_transport: path.to_string_lossy().to_string(),
        };
        let pointer_payload = encode_json(&pointer)?;
        stdout.write_all(&pointer_payload)?;
    } else {
        stdout.write_all(&payload)?;
    }

    stdout.write_all(b"\n")?;
    stdout.flush()
}

fn encode_json<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(io::Error::other)
}

fn success_response(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Value, code: i32, message: impl Into<String>, data: Option<Value>) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(RpcError {
            code,
            message: message.into(),
            data,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsStr;
    use std::path::PathBuf;

    use serde_json::json;

    #[test]
    fn extract_argv_strips_leading_gws() {
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            "argv": ["gws", "drive", "files", "list"]
        }))
        .unwrap();

        let argv = extract_argv(&arguments).unwrap();
        assert_eq!(argv, vec!["drive", "files", "list"]);
    }

    #[test]
    fn extract_argv_rejects_control_characters() {
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            "argv": ["drive", "files\nlist"]
        }))
        .unwrap();

        let err = extract_argv(&arguments).unwrap_err();
        assert!(err.contains("control characters"));
    }

    #[test]
    fn extract_argv_rejects_internal_mode_arg() {
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            "argv": [INTERNAL_MODE_ARG, "drive", "files", "list"]
        }))
        .unwrap();

        let err = extract_argv(&arguments).unwrap_err();
        assert!(err.contains("Reserved internal command"));
    }

    #[test]
    fn internal_cli_args_rewrite_program_name() {
        let args = vec![
            "/tmp/gws-executa".to_string(),
            INTERNAL_MODE_ARG.to_string(),
            "drive".to_string(),
            "files".to_string(),
            "list".to_string(),
        ];

        assert_eq!(
            internal_cli_args(&args),
            vec!["gws", "drive", "files", "list"]
        );
    }

    #[test]
    fn build_embedded_command_uses_current_exe_and_token() {
        let current_exe = PathBuf::from("/tmp/gws-executa");
        let argv = vec!["drive".to_string(), "files".to_string()];
        let command = build_embedded_gws_command(&current_exe, &argv, Some("token-123"));

        assert_eq!(command.get_program(), current_exe.as_os_str());
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec![INTERNAL_MODE_ARG.to_string(), "drive".to_string(), "files".to_string()]
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(TOKEN_CREDENTIAL_NAME))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().to_string()),
            Some("token-123".to_string())
        );
    }

    #[test]
    fn build_tool_data_parses_json_stdout() {
        let output = CommandOutput {
            stdout: "{\"files\":[]}".to_string(),
            stderr: "warning".to_string(),
            exit_code: 0,
            success: true,
            executable: "gws-executa".to_string(),
        };

        let data = build_tool_data(&output, &["drive".to_string()]);
        assert_eq!(data["stdout_json"]["files"], json!([]));
        assert_eq!(data["exit_code"], json!(0));
        assert_eq!(data["embedded_cli"], json!(true));
    }

    #[test]
    fn handle_invoke_rejects_unknown_tool() {
        let response = handle_invoke(
            json!(1),
            json!({
                "tool": "not_gws",
                "arguments": { "argv": ["drive", "files", "list"] }
            }),
            Some(Path::new("/tmp/gws-executa")),
        );

        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[test]
    fn handle_invoke_requires_current_executable() {
        let response = handle_invoke(
            json!(1),
            json!({
                "tool": TOOL_NAME,
                "arguments": { "argv": ["drive", "files", "list"] }
            }),
            None,
        );

        assert_eq!(response.error.unwrap().code, -32603);
    }
}
