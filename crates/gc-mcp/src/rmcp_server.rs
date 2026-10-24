//! 基于 [`rmcp`] SDK 的 MCP 服务端桥接。
//!
//! 与 [`crate::mcp`]（自研 stdio JSON-RPC 实现）共享同一份工具注册表
//! （[`crate::mcp::tools`]），协议编解码与传输交由 rmcp 处理。

use anyhow::Context as _;
use rmcp::{
    model::{
        CallToolRequestParam, CallToolResult, Content, ErrorCode, Implementation, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, ToolsCapability,
    },
    service::{RequestContext, RoleServer},
    ServerHandler, ServiceExt,
};
use serde_json::json;
use std::borrow::Cow;

use crate::mcp::tools;

/// gc 的 MCP 服务端：把静态工具注册表暴露为 rmcp 服务。
pub struct GcMcpServer {
    allow_destructive: bool,
}

impl GcMcpServer {
    pub fn new(allow_destructive: bool) -> Self {
        Self { allow_destructive }
    }
}

impl ServerHandler for GcMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_06_18,
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(false),
                }),
                ..Default::default()
            },
            server_info: Implementation {
                name: "gc-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "GitCode/AtomGit 命令行 gc 的 MCP 服务端：仓库 / Issue / PR / 搜索等工具。".into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        let tools = tools::registry(self.allow_destructive)
            .into_iter()
            .map(|t| {
                let raw = t.to_json();
                let schema = raw
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                let input_schema = match schema.as_object() {
                    Some(m) => std::sync::Arc::new(m.clone()),
                    None => Default::default(),
                };
                rmcp::model::Tool {
                    name: t.name.to_string().into(),
                    title: None,
                    description: Some(Cow::Borrowed(t.description)),
                    input_schema,
                    output_schema: None,
                    annotations: None,
                    icons: None,
                }
            })
            .collect();
        Ok(ListToolsResult {
            next_cursor: None,
            tools,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let Some(tool) = tools::find(&request.name, self.allow_destructive) else {
            return Err(rmcp::ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("未知工具: {}", request.name),
                None,
            ));
        };
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or_else(|| json!({}));
        // 工具执行失败不是协议错误：作为结果回给模型，让它能看到并自愈。
        Ok(match (tool.handler)(&args) {
            Ok(v) => CallToolResult {
                content: vec![Content::text(v.to_string())],
                structured_content: Some(v),
                is_error: Some(false),
                meta: None,
            },
            Err(e) => {
                let err = json!({ "error": format!("{e:#}") });
                CallToolResult {
                    content: vec![Content::text(err.to_string())],
                    structured_content: Some(err),
                    is_error: Some(true),
                    meta: None,
                }
            }
        })
    }
}

/// 以 rmcp 的 stdio 传输启动服务端（阻塞至对端关闭）。
pub fn serve_stdio(allow_destructive: bool) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("创建 tokio 运行时失败")?;
    rt.block_on(async move {
        let service = GcMcpServer::new(allow_destructive)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow::anyhow!("MCP 服务端启动失败: {e}"))?;
        service
            .waiting()
            .await
            .map_err(|e| anyhow::anyhow!("MCP 服务端异常退出: {e}"))?;
        Ok(())
    })
}
