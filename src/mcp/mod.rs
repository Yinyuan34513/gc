//! `gc mcp start` —— 把 `gc` 的能力通过 MCP（Model Context Protocol）暴露给 AI。
//!
//! 传输层是 stdio 上的换行分隔 JSON-RPC 2.0（不是 LSP 那种 Content-Length 分帧）。
//!
//! **最重要的约束：stdout 是协议通道。** 任何写到 stdout 的非 JSON-RPC 内容都会
//! 破坏会话，所以日志一律走 stderr，工具实现也只调用不打印的 [`crate::api`] 层。

pub mod jsonrpc;
pub mod tools;

use anyhow::Result;
use serde_json::{json, Value};

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;

use jsonrpc::{
    Request, Response, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    UNSUPPORTED_PROTOCOL_VERSION,
};

/// 我们实现的 **遗留（legacy）** 协议版本：需要 `initialize` 握手（2025-11-25 及更早）。
/// 顺序按「新 → 旧」排列；`initialize` 协商回退时取第一个（即最新的遗留版本）。
const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// 我们实现的 **现代（modern）** 协议版本：每请求用 `_meta` 声明版本、靠 `server/discover`
/// 发现（2026-07-28 及之后）。
const MODERN_VERSIONS: &[&str] = &["2026-07-28"];

/// 默认并发度。`tools/call` 基本都在等网络，开几个线程就能把串行等待摊平；
/// 开太多则会同时打爆 GitCode 接口，4 是个保守值。
pub const DEFAULT_CONCURRENCY: usize = 4;

/// 一次待执行的 `tools/call`。
struct Job {
    id: Value,
    params: Value,
}

/// 读取线程对一行报文的分类结果。
enum Action {
    /// 通知，按规范不回复。
    Silent,
    /// 已经可以立刻发出去的整行响应。
    Reply(String),
    /// `tools/call`：可能很慢。串行模式就地执行，并发模式派给工作线程。
    Call(Job),
}

pub struct Server {
    allow_destructive: bool,
    /// 收到 `initialize` 之前不接受 `tools/*`。
    initialized: bool,
    /// 同时执行的 `tools/call` 上限。1 表示完全串行。
    max_concurrency: usize,
}

impl Server {
    pub fn new(allow_destructive: bool) -> Self {
        Server {
            allow_destructive,
            initialized: false,
            max_concurrency: 1,
        }
    }

    /// 设定并发度；小于 1 会被当作 1（串行）。
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = n.max(1);
        self
    }

    /// 读 stdin、写 stdout，直到对端关闭。
    pub fn serve(&mut self, input: impl BufRead, output: impl Write + Send) -> Result<()> {
        if self.max_concurrency <= 1 {
            self.serve_sequential(input, output)
        } else {
            self.serve_concurrent(input, output)
        }
    }

    /// 串行模式：一次只处理一条，慢工具会阻塞后面所有请求。
    fn serve_sequential(&mut self, input: impl BufRead, output: impl Write) -> Result<()> {
        let out = Mutex::new(output);
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match self.classify(&line) {
                Action::Silent => {}
                Action::Reply(l) => emit(&out, &l)?,
                Action::Call(job) => {
                    let l = self.tools_call_line(job.id, &job.params);
                    emit(&out, &l)?;
                }
            }
        }
        Ok(())
    }

    /// 并发模式：`tools/call` 交给有界工作线程池，读取线程继续收报文。
    ///
    /// 这样一个卡住的网络请求不会连累 `ping` / `tools/list`，也不会让后面排队的
    /// 工具调用干等。快方法仍在读取线程上就地answer——它们本来就是纯内存操作，
    /// 派发过去反而多一次上下文切换，而且能避开 `initialized` 这类可变状态的共享。
    fn serve_concurrent(&mut self, input: impl BufRead, output: impl Write + Send) -> Result<()> {
        let allow = self.allow_destructive;
        let workers = self.max_concurrency;

        let out = Mutex::new(output);
        // 已被 notifications/cancelled 取消的请求 id。
        let cancelled: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
        // 容量有界：客户端灌得太快时读取线程会自然背压，不至于无限堆积。
        let (tx, rx) = mpsc::sync_channel::<Job>(workers);
        let rx = Mutex::new(rx);

        thread::scope(|s| {
            for _ in 0..workers {
                s.spawn(|| loop {
                    // 只在取任务时持锁，执行期间必须放开，否则就退化成串行。
                    let job = {
                        let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
                        guard.recv()
                    };
                    let Ok(job) = job else { break };

                    let line = tools_call_line(allow, job.id.clone(), &job.params);

                    // 规范要求：被取消的请求不要再回响应。
                    let key = id_key(&job.id);
                    let was_cancelled = cancelled
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key);
                    if !was_cancelled {
                        let _ = emit(&out, &line);
                    }
                });
            }

            let res = self.read_loop(input, &out, &tx, &cancelled);
            // 丢掉发送端，工作线程收到断开信号后退出；scope 会等它们做完在途任务。
            drop(tx);
            res
        })
    }

    fn read_loop(
        &mut self,
        input: impl BufRead,
        out: &Mutex<impl Write>,
        tx: &mpsc::SyncSender<Job>,
        cancelled: &Mutex<HashSet<String>>,
    ) -> Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(id) = cancellation_target(&line) {
                cancelled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id);
            }
            match self.classify(&line) {
                Action::Silent => {}
                Action::Reply(l) => emit(out, &l)?,
                // 通道满了说明工作线程都在忙，这里阻塞就是我们要的背压。
                Action::Call(job) => {
                    if tx.send(job).is_err() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// 处理一行报文。返回 `None` 表示这是通知，按规范不回复。
    ///
    /// 就地执行 `tools/call`，主要留给测试和串行路径使用。
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        match self.classify(line) {
            Action::Silent => None,
            Action::Reply(l) => Some(l),
            Action::Call(job) => Some(self.tools_call_line(job.id, &job.params)),
        }
    }

    /// 解析并分流一行报文，但**不执行**工具。
    fn classify(&mut self, line: &str) -> Action {
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                // 解析失败时拿不到 id，按规范用 null。
                return Action::Reply(
                    Response::err(Value::Null, PARSE_ERROR, format!("JSON 解析失败: {e}")).to_line(),
                );
            }
        };

        if req.method.is_empty() {
            let id = req.id.clone().unwrap_or(Value::Null);
            return Action::Reply(Response::err(id, INVALID_REQUEST, "缺少 method 字段").to_line());
        }

        if req.is_notification() {
            self.handle_notification(&req);
            return Action::Silent;
        }

        let id = req.id.clone().unwrap_or(Value::Null);

        // tools/call 单独拎出来：只有它可能很慢。
        if req.method == "tools/call" {
            return Action::Call(Job {
                id,
                params: req.params.clone(),
            });
        }

        Action::Reply(match self.dispatch(&req) {
            Ok(result) => Response::ok(id, result).to_line(),
            Err(e) => response_err(id, &e),
        })
    }

    fn tools_call_line(&self, id: Value, params: &Value) -> String {
        tools_call_line(self.allow_destructive, id, params)
    }

    fn handle_notification(&mut self, req: &Request) {
        if req.method == "notifications/initialized" {
            self.initialized = true;
        }
        // cancelled 在 read_loop 里单独处理；其余通知（progress 等）忽略即可。
    }

    fn dispatch(&mut self, req: &Request) -> std::result::Result<Value, RpcError> {
        // 现代 era：除 `initialize`（自带协商）与 `server/discover`（自带处理）外，
        // 若请求在 `_meta` 里声明了一个我们不支持的协议版本，回
        // `UnsupportedProtocolVersionError`，让客户端挑一个支持的版本重试。
        if req.method != "initialize" && req.method != "server/discover" {
            if let Some(v) = meta_version(&req.params) {
                if !is_supported_version(&v) {
                    return Err(unsupported_version_error(&v));
                }
            }
        }

        match req.method.as_str() {
            "initialize" => Ok(self.initialize(&req.params)),
            "server/discover" => self.discover(&req.params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(&req.params),
            // 我们没声明这些能力，但礼貌地返回空列表比报错更兼容。
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            other => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("不支持的方法: {other}"),
                data: None,
            }),
        }
    }

    /// 现代 era 的「发现」入口：客户端在任何其他请求之前先发它来确定服务端支持的
    /// 版本、能力与身份。对应 spec 2026-07-28 的 `server/discover`。
    fn discover(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        // 客户端可能在 `_meta` 里声明它偏好的版本；不支持就回现代错误，
        // 这同时也是 stdio 向后兼容探测用来判断「这是个现代服务端」的标志。
        if let Some(v) = meta_version(params) {
            if !is_supported_version(&v) {
                return Err(unsupported_version_error(&v));
            }
        }
        Ok(json!({
            "resultType": "complete",
            "supportedVersions": all_supported_versions(),
            "capabilities": { "tools": { "listChanged": false } },
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {
                    "name": "gc",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            },
            "instructions": "GitCode (AtomGit) 工具集。仓库参数一律用 owner/name 格式。\
                            Issue 与文件读取走网页端 JWT，其余走个人访问令牌（PAT）。\
                            创建类工具的标题和正文必须显式给出。",
            "ttlMs": 3_600_000,
            "cacheScope": "public",
        }))
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = match asked {
            Some(v) if LEGACY_VERSIONS.contains(&v) => v,
            _ => LEGACY_VERSIONS[0],
        };
        // 有些客户端不发 notifications/initialized，这里就先放行，避免死锁。
        self.initialized = true;
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": {
                "name": "gc",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": "GitCode (AtomGit) 工具集。仓库参数一律用 owner/name 格式。\
                             Issue 与文件读取走网页端 JWT，其余走个人访问令牌（PAT）。\
                             创建类工具的标题和正文必须显式给出。",
        })
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = tools::registry(self.allow_destructive)
            .iter()
            .map(|t| t.to_json())
            .collect();
        json!({ "tools": tools })
    }

    fn tools_call(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        tools_call_impl(self.allow_destructive, params)
    }
}

/// 执行一次 `tools/call`。抽成自由函数，好让工作线程不必持有 `&Server`。
fn tools_call_impl(
    allow_destructive: bool,
    params: &Value,
) -> std::result::Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "tools/call 缺少 name 参数".into(),
                data: None,
            })?;

        let Some(tool) = tools::find(name, allow_destructive) else {
            // 工具不存在属于协议层错误。
            let hint = if tools::find(name, true).is_some() {
                format!("工具 `{name}` 是破坏性操作，需要以 --allow-destructive 启动服务器才能使用")
            } else {
                format!("未知工具: {name}")
            };
            return Err(RpcError {
                code: INVALID_PARAMS,
                message: hint,
                data: None,
            });
        };

        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

        // 工具**执行**失败不是 JSON-RPC 错误：要作为结果回给模型，让它能看到并自愈。
        Ok(match (tool.handler)(&args) {
            Ok(v) => tool_result(&v, false),
            Err(e) => tool_result(&json!({ "error": format!("{e:#}") }), true),
        })
}

/// 执行一次 `tools/call` 并序列化成一行 JSON-RPC 响应（含 `\n`），供串行/并发两条路径共用。
fn tools_call_line(allow_destructive: bool, id: Value, params: &Value) -> String {
    match tools_call_impl(allow_destructive, params) {
        Ok(v) => Response::ok(id, v).to_line(),
        Err(e) => response_err(id, &e),
    }
}

/// 把一行协议响应写到 stdout（stdout 是协议通道，所有写入都走这里）。
/// 用 `Mutex` 串行化，保证并发模式下多个工作线程的报文不会交错。
fn emit<W: Write>(out: &Mutex<W>, line: &str) -> Result<()> {
    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
    writeln!(w, "{line}")?;
    w.flush()?;
    Ok(())
}

/// 把一个 JSON-RPC `id` 规整成「取消集合」用的字符串键。
/// 字符串 id 直接用内容（不带上引号），数字 id 用其文本形式——这样工作线程查取消集合
/// 与 `cancellation_target` 解析出来的键完全一致。
fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 解析 `notifications/cancelled`，返回被取消请求 id 的字符串形式。
/// 不是该通知、或参数里没有 id 时返回 None。
fn cancellation_target(line: &str) -> Option<String> {
    let req: Request = serde_json::from_str(line).ok()?;
    if req.method != "notifications/cancelled" {
        return None;
    }
    let id = req
        .params
        .get("requestId")
        .or_else(|| req.params.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    Some(id_key(&id))
}

/// 现代 era：从请求的 `_meta` 里取出客户端声明的协议版本。
/// 规范约定版本放在 `params._meta["io.modelcontextprotocol/protocolVersion"]`。
fn meta_version(params: &Value) -> Option<String> {
    params
        .get("_meta")
        .and_then(|m| m.get("io.modelcontextprotocol/protocolVersion"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 该版本是否被我们支持（现代或遗留均可）。
fn is_supported_version(v: &str) -> bool {
    MODERN_VERSIONS.contains(&v) || LEGACY_VERSIONS.contains(&v)
}

/// 全部支持的版本（现代在前、遗留在后），供 `server/discover` 上报。
fn all_supported_versions() -> Vec<&'static str> {
    let mut v = MODERN_VERSIONS.to_vec();
    v.extend_from_slice(LEGACY_VERSIONS);
    v
}

/// 构造 `UnsupportedProtocolVersionError`：规范要求在 `data` 里给出
/// `supported`（支持的版本清单）与 `requested`（客户端请求的版本）。
fn unsupported_version_error(requested: &str) -> RpcError {
    let supported: Vec<&str> = all_supported_versions();
    RpcError {
        code: UNSUPPORTED_PROTOCOL_VERSION,
        message: format!(
            "不支持的协议版本: {requested}（服务端支持：{}）",
            supported.join(", ")
        ),
        data: None,
    }
    .with_data(json!({ "supported": supported, "requested": requested }))
}

/// 把一个 `RpcError` 序列化成一行 JSON-RPC 错误响应（含可选 `data`）。
fn response_err(id: Value, e: &RpcError) -> String {
    match &e.data {
        Some(d) => Response::err_with_data(id, e.code, &e.message, d.clone()).to_line(),
        None => Response::err(id, e.code, &e.message).to_line(),
    }
}

/// 组装 `tools/call` 的返回体。
fn tool_result(v: &Value, is_error: bool) -> Value {    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

struct RpcError {
    code: i32,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    /// 附加 `data` 字段（如 `UnsupportedProtocolVersionError` 的
    /// `supported` / `requested`）。
    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// `gc mcp start` 的入口。
pub fn start(allow_destructive: bool, max_concurrency: usize) -> Result<()> {
    // 全部诊断信息走 stderr —— stdout 属于协议。
    eprintln!(
        "gc MCP 服务器已启动（stdio）。已注册 {} 个工具{}，并发度 {}。",
        tools::registry(allow_destructive).len(),
        if allow_destructive {
            "，破坏性操作已开启"
        } else {
            "，破坏性操作已禁用（需要时加 --allow-destructive）"
        },
        max_concurrency.max(1),
    );
    warn_missing_credentials();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = Server::new(allow_destructive).with_concurrency(max_concurrency);
    // 传 `Stdout` 而非 `StdoutLock`：工作线程需要在别的线程持有输出，而 `StdoutLock` 不是 `Send`；
    // 内部的 `Mutex` 负责把并发写入串行化，保证报文不交错。
    server.serve(stdin.lock(), stdout)
}

/// 启动时提示缺失的凭据，但不阻止启动 —— 只用只读 PAT 工具的场景不需要 JWT。
fn warn_missing_credentials() {
    let pat = crate::config::load_token()
        .ok()
        .flatten()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    if !pat {
        eprintln!("警告：未配置 PAT（GITCODE_TOKEN），仓库/PR/发行版/搜索类工具将不可用。");
    }
    if crate::config::load_web_tokens().is_err() {
        eprintln!(
            "警告：未配置网页端 JWT（GITCODE_WEB_TOKEN + GITCODE_WEB_REFRESH_TOKEN），\
             Issue/文件读取/Wiki/讨论类工具将不可用。"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv() -> Server {
        Server::new(false)
    }

    fn call(s: &mut Server, line: &str) -> Value {
        let out = s.handle_line(line).expect("应当有响应");
        serde_json::from_str(&out).expect("响应必须是合法 JSON")
    }

    #[test]
    fn initialize_echoes_a_supported_version() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "gc");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn initialize_falls_back_for_unknown_version() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(r["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
    }

    #[test]
    fn server_discover_returns_supported_versions_and_server_info() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        );
        // 现代 era 的发现结果：必须带 supportedVersions、serverInfo、capabilities。
        assert_eq!(r["result"]["resultType"], "complete");
        let versions: Vec<&str> = r["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(versions.contains(&"2026-07-28"), "应声明现代版本: {versions:?}");
        assert!(versions.contains(&LEGACY_VERSIONS[0]), "应同时声明遗留版本");
        assert_eq!(
            r["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "gc"
        );
        assert!(r["result"]["capabilities"]["tools"].is_object());
        assert!(r["result"]["instructions"].as_str().is_some());
    }

    #[test]
    fn server_discover_works_even_without_meta() {
        // 现代客户端可能只发 `server/discover` 不带 _meta，也要能正常返回。
        let mut s = srv();
        let r = call(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"server/discover"}"#);
        assert!(r["result"]["supportedVersions"].as_array().unwrap().len() >= 1);
    }

    #[test]
    fn server_discover_rejects_unsupported_version() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}"#,
        );
        // 现代错误码 -32022，且 data 里带 supported / requested。
        assert_eq!(r["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
        assert!(r["error"]["data"]["supported"].as_array().is_some());
        assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
    }

    #[test]
    fn modern_request_with_unsupported_meta_version_is_rejected() {
        // 现代 era：请求在 _meta 里带版本。声明不支持的版本应被拒（其余方法同理）。
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}"#,
        );
        assert_eq!(r["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
    }

    #[test]
    fn modern_request_with_supported_meta_version_succeeds() {
        // 带受支持的现代版本 _meta 的普通请求应正常处理（无 _meta 的遗留请求本就放行）。
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        );
        assert!(r["result"]["tools"].as_array().is_some());
    }

    #[test]
    fn notifications_get_no_response() {
        let mut s = srv();
        assert!(s
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
        assert!(s.initialized);
    }

    #[test]
    fn malformed_json_yields_parse_error_with_null_id() {
        let mut s = srv();
        let r = call(&mut s, "{not json");
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert_eq!(r["id"], Value::Null);
    }

    #[test]
    fn unknown_method_yields_method_not_found() {
        let mut s = srv();
        let r = call(&mut s, r#"{"jsonrpc":"2.0","id":9,"method":"nope"}"#);
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(r["id"], 9);
    }

    #[test]
    fn ping_returns_empty_result() {
        let mut s = srv();
        let r = call(&mut s, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn tools_list_hides_destructive_tools_by_default() {
        let mut s = srv();
        let r = call(&mut s, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"gc_issue_list"));
        assert!(!names.contains(&"gc_repo_delete"));

        let mut s2 = Server::new(true);
        let r2 = call(&mut s2, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        let n2 = r2["result"]["tools"].as_array().unwrap().len();
        assert!(n2 > names.len());
    }

    #[test]
    fn every_listed_tool_has_name_description_and_schema() {
        let mut s = Server::new(true);
        let r = call(&mut s, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        for t in r["result"]["tools"].as_array().unwrap() {
            assert!(t["name"].as_str().is_some_and(|n| !n.is_empty()));
            assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(t["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn calling_a_gated_tool_explains_the_flag() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"gc_repo_delete","arguments":{"repo":"a/b"}}}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        assert!(r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--allow-destructive"));
    }

    #[test]
    fn calling_an_unknown_tool_is_invalid_params() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn bad_arguments_come_back_as_tool_errors_not_rpc_errors() {
        // 参数错误必须是 isError 结果，模型才看得到并能改；不能是协议层错误。
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"gc_repo_view","arguments":{"repo":"malformed"}}}"#,
        );
        assert!(r["error"].is_null(), "不应是 JSON-RPC 错误: {r}");
        assert_eq!(r["result"]["isError"], true);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("owner/name"), "应说明格式要求: {text}");
    }

    #[test]
    fn tools_call_without_name_is_invalid_params() {
        let mut s = srv();
        let r = call(
            &mut s,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{}}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn serve_loop_answers_each_line_and_skips_notifications() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut out = Vec::new();
        Server::new(false)
            .serve(std::io::Cursor::new(input), &mut out)
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // 两个请求 → 两行；通知和空行都不产生输出。
        assert_eq!(lines.len(), 2, "实际输出: {text}");
        for l in &lines {
            let v: Value = serde_json::from_str(l).expect("每行都必须是合法 JSON");
            assert_eq!(v["jsonrpc"], "2.0");
        }
        assert_eq!(
            serde_json::from_str::<Value>(lines[1]).unwrap()["id"],
            2
        );
    }

    #[test]
    fn concurrent_serve_answers_all_requests_without_deadlock() {
        // 走并发路径（max_concurrency > 1）：工具调用用「参数非法」快速失败（不触网），
        // 只验证 工作线程池 + 有界通道 + Mutex 这套 plumbing 不会死锁、不丢报文。
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"gc_repo_view","arguments":{"repo":"bad"}}}"#,
            "\n",
            // 取消一个并不存在的请求（99），用来确认「通知本身不产出任何行」，
            // 同时不影响上面 id=4 的调用结果。
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}"#,
            "\n",
        );
        let mut out = Vec::new();
        Server::new(false)
            .with_concurrency(4)
            .serve(std::io::Cursor::new(input), &mut out)
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        // initialize + tools/list + ping + tools/call = 4 行；cancelled 通知本身不产出任何行。
        assert_eq!(lines.len(), 4, "实际输出: {text}");
        for l in &lines {
            let v: Value = serde_json::from_str(l).expect("每行必须是合法 JSON");
            assert_eq!(v["jsonrpc"], "2.0");
        }
        let ids: Vec<i64> = lines
            .iter()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["id"].as_i64())
            .collect();
        for want in [1, 2, 3, 4] {
            assert!(ids.contains(&want), "缺少 id={want} 的响应: {text}");
        }
    }

    #[test]
    fn cancellation_target_parses_request_id() {
        assert_eq!(
            cancellation_target(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#
            ),
            Some("7".to_string())
        );
        assert_eq!(
            cancellation_target(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"abc"}}"#
            ),
            Some("abc".to_string())
        );
        // 不是 cancelled 通知就不该解析出 id。
        assert_eq!(
            cancellation_target(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#),
            None
        );
    }
}
