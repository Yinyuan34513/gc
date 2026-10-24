//! `gc mcp start` 的端到端测试：真的把二进制拉起来，从 stdin 喂 JSON-RPC 报文。
//!
//! 单元测试覆盖不到的恰恰是这里要盯的几件事：进程能不能起来、stdout 有没有被
//! 别处的 `println!` 污染、stdin EOF 之后能不能干净退出。stdout 是协议通道，
//! 混进去一行普通日志整个会话就废了。
//!
//! 这些用例全部走离线路径（握手、列表、参数校验失败），不依赖网络和真实凭据。

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Run {
    /// stdout 的每一行都必须是合法的 JSON-RPC 报文，否则直接失败。
    fn messages(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("stdout 混入了非 JSON 内容: {l:?}（{e}）"));
                assert_eq!(v["jsonrpc"], "2.0", "报文缺少 jsonrpc 字段: {l}");
                v
            })
            .collect()
    }

    fn by_id(&self, id: i64) -> Value {
        self.messages()
            .into_iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("找不到 id={id} 的响应，stdout=\n{}", self.stdout))
    }
}

/// 启动 `gc mcp start`，喂完 `lines` 后关闭 stdin，等它自己退出。
fn mcp(extra: &[&str], lines: &[&str]) -> Run {
    let mut args = vec!["mcp", "start"];
    args.extend_from_slice(extra);

    let mut child = Command::new(env!("CARGO_BIN_EXE_gc"))
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 gc mcp start 失败");

    {
        let mut stdin = child.stdin.take().expect("拿不到 stdin");
        for l in lines {
            writeln!(stdin, "{l}").expect("写 stdin 失败");
        }
        // stdin 在这里 drop，制造 EOF —— 服务器应当据此收尾退出。
    }

    let out = child.wait_with_output().expect("等待子进程失败");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
    }
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"itest","version":"0"}}}"#;
const INITED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

fn tool_names(run: &Run, id: i64) -> Vec<String> {
    run.by_id(id)["result"]["tools"]
        .as_array()
        .expect("tools 必须是数组")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn handshake_succeeds_and_eof_exits_cleanly() {
    let r = mcp(&[], &[INIT, INITED]);
    assert_eq!(r.code, Some(0), "EOF 后应正常退出，stderr=\n{}", r.stderr);

    let msgs = r.messages();
    // initialize 有响应，notifications/initialized 没有 —— 一共一条。
    assert_eq!(msgs.len(), 1, "stdout=\n{}", r.stdout);

    let init = &msgs[0];
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "gc");
    assert!(init["error"].is_null());
}

#[test]
fn stdout_is_protocol_only_and_logs_go_to_stderr() {
    let r = mcp(
        &[],
        &[
            INIT,
            INITED,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ],
    );
    // messages() 会逐行断言可解析；这里再确认没有多余的行。
    assert_eq!(r.messages().len(), 2, "stdout=\n{}", r.stdout);
    // 启动横幅必须在 stderr 上，不能污染 stdout。
    assert!(
        r.stderr.contains("MCP 服务器已启动"),
        "启动日志应打到 stderr，实际 stderr=\n{}",
        r.stderr
    );
    assert!(!r.stdout.contains("MCP 服务器已启动"));
}

#[test]
fn modern_client_discovers_server_via_server_discover() {
    // 现代 era（spec 2026-07-28）的客户端先发 `server/discover` 来发现服务端，
    // 而不是遗留的 `initialize` 握手。这里验证 gc 能被这类客户端检测到。
    let r = mcp(
        &[],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        ],
    );
    assert_eq!(r.code, Some(0), "EOF 后应正常退出，stderr=\n{}", r.stderr);

    // 第一条是 server/discover 的 DiscoverResult。
    let disc = &r.messages()[0];
    assert_eq!(disc["id"], 1);
    let versions: Vec<String> = disc["result"]["supportedVersions"]
        .as_array()
        .expect("supportedVersions 必须是数组")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        versions.iter().any(|v| v == "2026-07-28"),
        "应声明现代版本: {versions:?}"
    );
    assert_eq!(
        disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "gc"
    );

    // 第二条是带 _meta 的 tools/list，应正常返回工具清单。
    let list = &r.messages()[1];
    assert_eq!(list["id"], 2);
    assert!(list["result"]["tools"].as_array().unwrap().len() >= 20);
}

#[test]
fn server_discover_probe_with_unsupported_version_is_modern_error() {
    // 向后兼容探测：若客户端用不支持的版本探测 server/discover，
    // 服务端应回现代错误码 -32022（带 supported/requested），而非 METHOD_NOT_FOUND，
    // 这样双 era 客户端能据此判定这是个现代服务端。
    let r = mcp(
        &[],
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}"#,
        ],
    );
    let disc = &r.messages()[0];
    assert_eq!(disc["error"]["code"], -32022);
    assert!(disc["error"]["data"]["supported"].as_array().is_some());
}

#[test]
fn tools_list_is_nonempty_and_well_formed() {
    let r = mcp(
        &[],
        &[INIT, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#],
    );
    let tools = r.by_id(2)["result"]["tools"].clone();
    let arr = tools.as_array().unwrap();
    assert!(arr.len() >= 20, "工具数量偏少: {}", arr.len());
    for t in arr {
        assert!(t["name"].as_str().is_some_and(|n| n.starts_with("gc_")));
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
        assert_eq!(t["inputSchema"]["type"], "object");
    }
}

#[test]
fn destructive_tools_need_the_flag() {
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;

    let safe = mcp(&[], &[INIT, list]);
    let safe_names = tool_names(&safe, 2);
    assert!(safe_names.iter().any(|n| n == "gc_repo_view"));
    assert!(
        !safe_names.iter().any(|n| n == "gc_repo_delete"),
        "默认不该暴露删仓库工具"
    );

    let open = mcp(&["--allow-destructive"], &[INIT, list]);
    let open_names = tool_names(&open, 2);
    assert!(open_names.iter().any(|n| n == "gc_repo_delete"));
    assert!(open_names.len() > safe_names.len());
    assert!(open.stderr.contains("破坏性操作已开启"));
}

#[test]
fn calling_a_gated_tool_points_at_the_flag() {
    let r = mcp(
        &[],
        &[
            INIT,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gc_repo_delete","arguments":{"repo":"a/b"}}}"#,
        ],
    );
    let resp = r.by_id(2);
    assert_eq!(resp["error"]["code"], -32602);
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--allow-destructive"));
}

#[test]
fn bad_arguments_surface_as_tool_errors() {
    // 参数错误要作为 isError 结果回给模型（它才能改），而不是 JSON-RPC 协议错误。
    let r = mcp(
        &[],
        &[
            INIT,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gc_repo_view","arguments":{"repo":"没有斜杠"}}}"#,
        ],
    );
    let resp = r.by_id(2);
    assert!(resp["error"].is_null(), "不应是协议错误: {resp}");
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("owner/name"), "错误信息应说明格式: {text}");
}

#[test]
fn protocol_errors_do_not_kill_the_session() {
    // 一行坏 JSON 之后，会话必须还能继续服务。
    let r = mcp(
        &[],
        &[
            INIT,
            "{ 这不是 JSON",
            r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"ping"}"#,
        ],
    );
    assert_eq!(r.code, Some(0));

    let msgs = r.messages();
    assert_eq!(msgs.len(), 4, "stdout=\n{}", r.stdout);
    // 解析失败拿不到 id，按规范用 null。
    assert_eq!(msgs[1]["id"], Value::Null);
    assert_eq!(msgs[1]["error"]["code"], -32700);
    assert_eq!(r.by_id(3)["error"]["code"], -32601);
    assert_eq!(r.by_id(4)["result"], serde_json::json!({}));
}

#[test]
fn notifications_alone_produce_no_stdout() {
    let r = mcp(
        &[],
        &[
            INITED,
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#,
        ],
    );
    assert_eq!(r.code, Some(0));
    assert!(r.stdout.is_empty(), "通知不该有任何输出: {:?}", r.stdout);
}

#[test]
fn empty_stdin_starts_and_stops_without_output() {
    let r = mcp(&[], &[]);
    assert_eq!(r.code, Some(0));
    assert!(r.stdout.is_empty());
}
