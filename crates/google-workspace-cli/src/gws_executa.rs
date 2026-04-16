use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

const MAX_STDIO_MESSAGE_BYTES: usize = 512 * 1024;
const TOOL_NAME: &str = "run_gws";
const PLUGIN_NAME: &str = "gws-executa";
const TOKEN_CREDENTIAL_NAME: &str = "GOOGLE_ACCESS_TOKEN";
const INTERNAL_GWS_TOKEN_ENV: &str = "GOOGLE_WORKSPACE_CLI_TOKEN";
const CREDENTIALS_FILE_CREDENTIAL_NAME: &str = "GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE";
const PROJECT_ID_ENV: &str = "GOOGLE_WORKSPACE_PROJECT_ID";
const CONFIG_DIR_ENV: &str = "GOOGLE_WORKSPACE_CLI_CONFIG_DIR";
const INTERNAL_MODE_ARG: &str = "__anna_run_gws_internal";
const PLUGIN_VERSION: &str = "1.0.0";
const PROJECT_ID_ARG_NAME: &str = "project_id";
const ISOLATED_CHILD_ENV_REMOVE: &[&str] = &[
    INTERNAL_GWS_TOKEN_ENV,
    CREDENTIALS_FILE_CREDENTIAL_NAME,
    PROJECT_ID_ENV,
    CONFIG_DIR_ENV,
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_WORKSPACE_CLI_CLIENT_ID",
    "GOOGLE_WORKSPACE_CLI_CLIENT_SECRET",
    "GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND",
    "GOOGLE_WORKSPACE_CLI_LOG_FILE",
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileTransportMode {
    Auto,
    Always,
}

#[derive(Debug)]
struct ResponsePlan {
    response: RpcResponse,
    file_transport_dir: Option<PathBuf>,
    file_transport_mode: FileTransportMode,
}

#[derive(Debug, Default)]
struct ResolvedCredentials {
    env: HashMap<&'static str, String>,
    sources: Vec<&'static str>,
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

                let plan = match serde_json::from_str::<Value>(trimmed) {
                    Ok(value) => match serde_json::from_value::<RpcRequest>(value) {
                        Ok(request) => handle_request(request, current_exe.as_deref()),
                        Err(err) => response_plan(error_response(
                            Value::Null,
                            -32600,
                            "Invalid request",
                            Some(json!({ "details": err.to_string() })),
                        )),
                    },
                    Err(err) => response_plan(error_response(
                        Value::Null,
                        -32700,
                        "Parse error",
                        Some(json!({ "details": err.to_string() })),
                    )),
                };

                if let Err(err) = send_response(
                    &plan.response,
                    plan.file_transport_dir.as_deref(),
                    plan.file_transport_mode,
                ) {
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

fn handle_request(request: RpcRequest, current_exe: Option<&Path>) -> ResponsePlan {
    if request.jsonrpc != "2.0" || request.method.is_empty() {
        return response_plan(error_response(
            request.id,
            -32600,
            "Invalid request",
            Some(json!({
                "expected_jsonrpc": "2.0",
                "expected_fields": ["jsonrpc", "method", "id"],
            })),
        ));
    }

    match request.method.as_str() {
        "describe" => response_plan(success_response(request.id, manifest())),
        "health" => response_plan(success_response(
            request.id,
            json!({
                "status": "healthy",
                "timestamp": Utc::now().to_rfc3339(),
                "version": env!("CARGO_PKG_VERSION"),
                "tools_count": 1,
            }),
        )),
        "invoke" => handle_invoke(request.id, request.params, current_exe),
        _ => response_plan(error_response(
            request.id,
            -32601,
            format!("Method not found: {}", request.method),
            None,
        )),
    }
}

fn handle_invoke(id: Value, params: Value, current_exe: Option<&Path>) -> ResponsePlan {
    let params: InvokeParams = match serde_json::from_value(params) {
        Ok(params) => params,
        Err(err) => {
            return response_plan(error_response(
                id,
                -32602,
                "Invalid params",
                Some(json!({ "details": err.to_string() })),
            ));
        }
    };

    if params.tool != TOOL_NAME {
        return response_plan(error_response(
            id,
            -32601,
            format!("Unknown tool: {}", params.tool),
            Some(json!({ "available_tools": [TOOL_NAME] })),
        ));
    }

    let argv = match extract_argv(&params.arguments) {
        Ok(argv) => argv,
        Err(message) => return response_plan(error_response(id, -32602, message, None)),
    };

    let project_id = match extract_project_id(&params.arguments) {
        Ok(project_id) => project_id,
        Err(message) => return response_plan(error_response(id, -32602, message, None)),
    };

    let Some(current_exe) = current_exe else {
        return response_plan(error_response(
            id,
            -32603,
            "Failed to resolve current executable path",
            None,
        ));
    };

    let file_transport_dir = match resolve_file_transport_dir(&params.arguments, current_exe) {
        Ok(path) => path,
        Err(message) => return response_plan(error_response(id, -32602, message, None)),
    };

    let mut resolved_credentials = match resolve_credential_env(&params.context.credentials) {
        Ok(credentials) => credentials,
        Err(message) => return response_plan(error_response(id, -32602, message, None)),
    };

    if let Some(project_id) = project_id {
        resolved_credentials
            .env
            .insert(PROJECT_ID_ENV, project_id.to_string());
    }

    let output = match execute_embedded_gws(current_exe, &argv, &resolved_credentials.env) {
        Ok(output) => output,
        Err(err) => {
            return response_plan(error_response(
                id,
                -32603,
                format!(
                    "Failed to execute embedded gws mode from '{}': {err}",
                    current_exe.display()
                ),
                None,
            ));
        }
    };

    let tool_data = build_tool_data(&output, &argv, &resolved_credentials, &file_transport_dir);
    let response = if output.success {
        success_response(
            id,
            json!({
                "success": true,
                "tool": TOOL_NAME,
                "data": tool_data,
            }),
        )
    } else {
        build_gws_failure_response(id, &output, tool_data)
    };

    ResponsePlan {
        response,
        file_transport_dir: Some(file_transport_dir),
        file_transport_mode: FileTransportMode::Always,
    }
}

fn manifest() -> Value {
    json!({
        "name": PLUGIN_NAME,
        "display_name": PLUGIN_NAME,
        "version": PLUGIN_VERSION,
        "description": "Single-file Anna Executa wrapper with embedded gws runtime.",
        "author": "googleworkspace/cli",
        "credentials": [
            {
                "name": TOKEN_CREDENTIAL_NAME,
                "display_name": "Google Access Token",
                "description": "OAuth2 access token injected by Anna. The plugin forwards it to the embedded gws runtime as GOOGLE_WORKSPACE_CLI_TOKEN.",
                "required": false,
                "sensitive": true
            },
            {
                "name": CREDENTIALS_FILE_CREDENTIAL_NAME,
                "display_name": "Google Workspace Credentials File",
                "description": "Path to a credentials JSON file forwarded to the embedded gws runtime as GOOGLE_WORKSPACE_CLI_CREDENTIALS_FILE.",
                "required": false,
                "sensitive": false
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
                    },
                    {
                        "name": PROJECT_ID_ARG_NAME,
                        "type": "string",
                        "description": "Optional explicit GCP project ID forwarded as GOOGLE_WORKSPACE_PROJECT_ID. Embedded mode does not infer a project from local gws config files.",
                        "required": false
                    },
                    {
                        "name": "cwd",
                        "type": "string",
                        "description": "Optional existing directory used for file transport temporary files. Defaults to the plugin binary directory.",
                        "required": false
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

fn extract_cwd(arguments: &Map<String, Value>) -> Result<Option<&str>, String> {
    match arguments.get("cwd") {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| "cwd must be a string when provided".to_string()),
    }
}

fn extract_project_id(arguments: &Map<String, Value>) -> Result<Option<&str>, String> {
    match arguments.get(PROJECT_ID_ARG_NAME) {
        None => Ok(None),
        Some(value) => {
            let project_id = value
                .as_str()
                .ok_or_else(|| format!("{PROJECT_ID_ARG_NAME} must be a string when provided"))?;
            validate_text_input(PROJECT_ID_ARG_NAME, project_id)?;
            Ok(Some(project_id))
        }
    }
}

fn validate_command_arg(arg: &str) -> Result<(), String> {
    if arg == INTERNAL_MODE_ARG {
        return Err("Reserved internal command argument is not allowed".to_string());
    }

    validate_text_input("Command arguments", arg)?;

    Ok(())
}

fn validate_text_input(label: &str, value: &str) -> Result<(), String> {
    if value.chars().any(char::is_control) {
        return Err(format!("{label} must not contain control characters"));
    }
    if value.trim().is_empty() {
        return Err(format!("{label} must not be empty"));
    }

    Ok(())
}

fn resolve_file_transport_dir(
    arguments: &Map<String, Value>,
    current_exe: &Path,
) -> Result<PathBuf, String> {
    if let Some(raw_cwd) = extract_cwd(arguments)? {
        let path = PathBuf::from(raw_cwd);
        if !path.exists() {
            return Err(format!("cwd does not exist: {}", path.display()));
        }
        if !path.is_dir() {
            return Err(format!("cwd is not a directory: {}", path.display()));
        }
        return Ok(path);
    }

    current_exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Failed to determine plugin binary directory".to_string())
}

fn resolve_credential_env(
    credentials: &HashMap<String, String>,
) -> Result<ResolvedCredentials, String> {
    let mut resolved = ResolvedCredentials::default();

    if let Some(token) = credentials
        .get(TOKEN_CREDENTIAL_NAME)
        .filter(|value| !value.trim().is_empty())
    {
        resolved.env.insert(INTERNAL_GWS_TOKEN_ENV, token.clone());
        resolved.sources.push(TOKEN_CREDENTIAL_NAME);
    }

    if let Some(path) = credentials
        .get(CREDENTIALS_FILE_CREDENTIAL_NAME)
        .filter(|value| !value.trim().is_empty())
    {
        resolved
            .env
            .insert(CREDENTIALS_FILE_CREDENTIAL_NAME, path.clone());
        resolved.sources.push(CREDENTIALS_FILE_CREDENTIAL_NAME);
    }

    if !resolved.env.contains_key(INTERNAL_GWS_TOKEN_ENV) {
        if let Ok(token) = std::env::var(INTERNAL_GWS_TOKEN_ENV) {
            if !token.trim().is_empty() {
                resolved.env.insert(INTERNAL_GWS_TOKEN_ENV, token);
                resolved.sources.push(INTERNAL_GWS_TOKEN_ENV);
            }
        }
    }

    if !resolved.env.contains_key(CREDENTIALS_FILE_CREDENTIAL_NAME) {
        if let Ok(path) = std::env::var(CREDENTIALS_FILE_CREDENTIAL_NAME) {
            if !path.trim().is_empty() {
                resolved.env.insert(CREDENTIALS_FILE_CREDENTIAL_NAME, path);
                resolved.sources.push(CREDENTIALS_FILE_CREDENTIAL_NAME);
            }
        }
    }

    if resolved.env.is_empty() {
        return Err(format!(
            "One of '{}' or '{}' must be provided in context.credentials or environment",
            TOKEN_CREDENTIAL_NAME, CREDENTIALS_FILE_CREDENTIAL_NAME
        ));
    }

    resolved.sources.sort_unstable();
    Ok(resolved)
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
    credential_env: &HashMap<&'static str, String>,
) -> io::Result<CommandOutput> {
    let mut command = build_embedded_gws_command(current_exe, argv, credential_env);
    let output = command.output()?;
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
        success: output.status.success(),
        executable: current_exe.display().to_string(),
    })
}

fn build_embedded_gws_command(
    current_exe: &Path,
    argv: &[String],
    credential_env: &HashMap<&'static str, String>,
) -> Command {
    let mut command = Command::new(current_exe);
    command
        .arg(INTERNAL_MODE_ARG)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for key in ISOLATED_CHILD_ENV_REMOVE {
        command.env_remove(key);
    }

    command.env(google_workspace_cli::ISOLATED_MODE_ENV, "1");
    command.env(CONFIG_DIR_ENV, isolated_config_dir());

    for (key, value) in credential_env {
        command.env(key, value);
    }

    command
}

fn isolated_config_dir() -> PathBuf {
    std::env::temp_dir().join(PLUGIN_NAME).join("config")
}

fn build_tool_data(
    output: &CommandOutput,
    argv: &[String],
    resolved_credentials: &ResolvedCredentials,
    file_transport_dir: &Path,
) -> Value {
    let mut data = Map::new();
    data.insert("argv".to_string(), json!(argv));
    data.insert(
        "plugin_executable".to_string(),
        Value::String(output.executable.clone()),
    );
    data.insert(
        "file_transport_dir".to_string(),
        Value::String(file_transport_dir.display().to_string()),
    );
    data.insert(
        "credential_sources".to_string(),
        json!(resolved_credentials.sources),
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

fn build_gws_failure_response(id: Value, output: &CommandOutput, tool_data: Value) -> RpcResponse {
    let stdout_json = serde_json::from_str::<Value>(output.stdout.trim()).ok();
    let gws_message = stdout_json
        .as_ref()
        .and_then(|json| json.get("error"))
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .filter(|message| !message.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            let stderr = output.stderr.trim();
            if stderr.is_empty() {
                None
            } else {
                Some(stderr.to_string())
            }
        })
        .unwrap_or_else(|| "gws command failed".to_string());

    let reason = stdout_json
        .as_ref()
        .and_then(|json| json.get("error"))
        .and_then(|error| error.get("reason"))
        .and_then(Value::as_str);

    let code = match reason {
        Some("validationError") => -32602,
        _ => -32603,
    };

    error_response(
        id,
        code,
        gws_message,
        Some(json!({
            "tool": TOOL_NAME,
            "gws_exit_code": output.exit_code,
            "reason": reason,
            "tool_data": tool_data,
        })),
    )
}

fn send_response(
    response: &RpcResponse,
    file_transport_dir: Option<&Path>,
    file_transport_mode: FileTransportMode,
) -> io::Result<()> {
    let payload = encode_json(response)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    let should_use_file_transport = match file_transport_mode {
        FileTransportMode::Always => true,
        FileTransportMode::Auto => payload.len() > MAX_STDIO_MESSAGE_BYTES,
    };

    if should_use_file_transport {
        let dir = file_transport_dir.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file transport directory is required when file transport is enabled",
            )
        })?;
        let mut temp = tempfile::Builder::new()
            .prefix("executa-resp-")
            .suffix(".json")
            .tempfile_in(dir)?;
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

fn response_plan(response: RpcResponse) -> ResponsePlan {
    ResponsePlan {
        response,
        file_transport_dir: None,
        file_transport_mode: FileTransportMode::Auto,
    }
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

fn error_response(
    id: Value,
    code: i32,
    message: impl Into<String>,
    data: Option<Value>,
) -> RpcResponse {
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
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::tempdir;

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
        let credential_env = HashMap::from([(INTERNAL_GWS_TOKEN_ENV, "token-123".to_string())]);
        let command = build_embedded_gws_command(&current_exe, &argv, &credential_env);

        assert_eq!(command.get_program(), current_exe.as_os_str());
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec![
                INTERNAL_MODE_ARG.to_string(),
                "drive".to_string(),
                "files".to_string()
            ]
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(INTERNAL_GWS_TOKEN_ENV))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().to_string()),
            Some("token-123".to_string())
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(google_workspace_cli::ISOLATED_MODE_ENV))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().to_string()),
            Some("1".to_string())
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new(CONFIG_DIR_ENV))
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().to_string()),
            Some(isolated_config_dir().display().to_string())
        );
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == OsStr::new("GOOGLE_APPLICATION_CREDENTIALS"))
                .map(|(_, value)| value.is_none()),
            Some(true)
        );
    }

    #[test]
    fn build_tool_data_parses_json_stdout() {
        let resolved_credentials = ResolvedCredentials {
            env: HashMap::from([(INTERNAL_GWS_TOKEN_ENV, "token-123".to_string())]),
            sources: vec![TOKEN_CREDENTIAL_NAME],
        };
        let output = CommandOutput {
            stdout: "{\"files\":[]}".to_string(),
            stderr: "warning".to_string(),
            exit_code: 0,
            success: true,
            executable: "gws-executa".to_string(),
        };

        let data = build_tool_data(
            &output,
            &["drive".to_string()],
            &resolved_credentials,
            Path::new("/tmp"),
        );
        assert_eq!(data["stdout_json"]["files"], json!([]));
        assert_eq!(data["exit_code"], json!(0));
        assert_eq!(data["embedded_cli"], json!(true));
        assert_eq!(data["credential_sources"], json!([TOKEN_CREDENTIAL_NAME]));
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

        assert_eq!(response.response.error.unwrap().code, -32601);
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

        assert_eq!(response.response.error.unwrap().code, -32603);
    }

    #[test]
    fn build_gws_failure_response_uses_validation_error_shape() {
        let output = CommandOutput {
            stdout: "{\n  \"error\": {\n    \"code\": 400,\n    \"message\": \"error: unrecognized subcommand 'usrs'\",\n    \"reason\": \"validationError\"\n  }\n}\n".to_string(),
            stderr: "error[validation]: error: unrecognized subcommand 'usrs'\n".to_string(),
            exit_code: 3,
            success: false,
            executable: "gws-executa".to_string(),
        };

        let response = build_gws_failure_response(
            json!(5),
            &output,
            json!({
                "argv": ["gmail", "usrs"],
                "exit_code": 3
            }),
        );

        let error = response.error.expect("expected top-level error");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("unrecognized subcommand"));
        assert_eq!(response.result, None);
        assert_eq!(error.data.unwrap()["tool"], json!(TOOL_NAME));
    }

    #[test]
    fn resolve_credential_env_accepts_credentials_file_only() {
        let credentials = HashMap::from([(
            CREDENTIALS_FILE_CREDENTIAL_NAME.to_string(),
            "/tmp/creds.json".to_string(),
        )]);

        let env = resolve_credential_env(&credentials).unwrap();
        assert_eq!(
            env.env.get(CREDENTIALS_FILE_CREDENTIAL_NAME),
            Some(&"/tmp/creds.json".to_string())
        );
        assert_eq!(env.sources, vec![CREDENTIALS_FILE_CREDENTIAL_NAME]);
    }

    #[test]
    fn resolve_credential_env_maps_google_access_token_to_internal_gws_env() {
        let credentials =
            HashMap::from([(TOKEN_CREDENTIAL_NAME.to_string(), "token-123".to_string())]);

        let env = resolve_credential_env(&credentials).unwrap();
        assert_eq!(
            env.env.get(INTERNAL_GWS_TOKEN_ENV),
            Some(&"token-123".to_string())
        );
        assert_eq!(env.sources, vec![TOKEN_CREDENTIAL_NAME]);
    }

    #[test]
    fn resolve_credential_env_uses_ambient_internal_env_when_context_missing() {
        std::env::set_var(INTERNAL_GWS_TOKEN_ENV, "ambient-token");

        let env = resolve_credential_env(&HashMap::new()).unwrap();

        assert_eq!(
            env.env.get(INTERNAL_GWS_TOKEN_ENV),
            Some(&"ambient-token".to_string())
        );
        assert_eq!(env.sources, vec![INTERNAL_GWS_TOKEN_ENV]);

        std::env::remove_var(INTERNAL_GWS_TOKEN_ENV);
    }

    #[test]
    fn resolve_credential_env_requires_one_credential_source() {
        std::env::remove_var(INTERNAL_GWS_TOKEN_ENV);
        std::env::remove_var(CREDENTIALS_FILE_CREDENTIAL_NAME);

        let err = resolve_credential_env(&HashMap::new()).unwrap_err();
        assert!(err.contains(TOKEN_CREDENTIAL_NAME));
        assert!(err.contains(CREDENTIALS_FILE_CREDENTIAL_NAME));
    }

    #[test]
    fn extract_project_id_accepts_string_input() {
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            PROJECT_ID_ARG_NAME: "anna-project"
        }))
        .unwrap();

        assert_eq!(
            extract_project_id(&arguments).unwrap(),
            Some("anna-project")
        );
    }

    #[test]
    fn extract_project_id_rejects_control_characters() {
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            PROJECT_ID_ARG_NAME: "anna\nproject"
        }))
        .unwrap();

        let err = extract_project_id(&arguments).unwrap_err();
        assert!(err.contains("control characters"));
    }

    #[test]
    fn resolve_file_transport_dir_uses_cwd_argument() {
        let dir = tempdir().unwrap();
        let arguments = serde_json::from_value::<Map<String, Value>>(json!({
            "argv": ["--version"],
            "cwd": dir.path().to_string_lossy()
        }))
        .unwrap();

        let resolved =
            resolve_file_transport_dir(&arguments, Path::new("/tmp/gws-executa")).unwrap();
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn send_response_always_uses_file_transport_when_requested() {
        let dir = tempdir().unwrap();
        let response = success_response(json!(1), json!({"ok": true}));

        send_response(&response, Some(dir.path()), FileTransportMode::Always).unwrap();

        let entries = fs::read_dir(dir.path())
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let content = fs::read_to_string(entries[0].path()).unwrap();
        assert!(content.contains("\"jsonrpc\":\"2.0\""));
    }
}
