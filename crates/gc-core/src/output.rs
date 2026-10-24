//! CLI 共享输出层：pretty/list 打印、--json/--jq 结构化输出、termimad 渲染、编辑器助手。

use anyhow::{anyhow, Context as _};
use base64::Engine as _;
use serde_json::Value;

/// 读本地文件并编码成 base64（v5 的 contents 写接口只收 base64）。
pub fn read_as_base64(path: &str) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).map_err(|e| anyhow!("读取 {} 失败: {}", path, e))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// 结构化输出助手：给了 --jq 则执行 jq 表达式，给了 --json 则打印完整 JSON。
pub fn print_structured(v: &Value, json: bool, jq: &Option<String>) -> anyhow::Result<bool> {
    if let Some(f) = jq {
        print!("{}", run_jq(f, v)?);
        return Ok(true);
    }
    if json {
        println!("{}", pretty(v));
        return Ok(true);
    }
    Ok(false)
}

/// 用 termimad 把 Markdown 渲染到终端（空文本跳过）。
pub fn render_md(md: &str) {
    if md.trim().is_empty() {
        return;
    }
    let skin = termimad::MadSkin::default();
    skin.print_text(md);
}

/// 用 jaq 对 JSON 值执行 jq 表达式，返回每个输出一行（JSON 格式）。
pub fn run_jq(filter: &str, input: &Value) -> anyhow::Result<String> {
    use jaq_core::load::{Arena, File, Loader};
    use jaq_core::{Compiler, Ctx, Vars};

    let program = File {
        code: filter,
        path: (),
    };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs::<jaq_core::data::JustLut<jaq_json::Val>>()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|e| anyhow!("jq 表达式解析失败：{:?}", e))?;
    let compiled = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|e| anyhow!("jq 表达式编译失败：{:?}", e))?;
    // jaq-json 的 Val 与 serde_json::Value 之间没有直接转换，走 JSON 字符串
    let json_str = serde_json::to_string(input)?;
    let val = jaq_json::read::parse_single(json_str.as_bytes())
        .map_err(|e| anyhow!("输入解析失败：{}", e))?;
    let ctx = Ctx::<jaq_core::data::JustLut<jaq_json::Val>>::new(&compiled.lut, Vars::new([]));
    let mut out = String::new();
    for r in compiled.id.run((ctx, val)) {
        match r {
            Ok(v) => {
                let s = format!("{}", v);
                out.push_str(&s);
                out.push('\n');
            }
            Err(e) => return Err(anyhow!("jq 执行失败：{:?}", e)),
        }
    }
    Ok(out)
}

pub fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

pub fn print_list(v: &Value, fmt: impl Fn(&Value) -> String) {
    let arr = match v {
        Value::Array(a) => Some(a),
        Value::Object(o) => o
            .get("issues")
            .or_else(|| o.get("data"))
            .or_else(|| o.get("events"))
            .and_then(|d| d.as_array()),
        _ => None,
    };
    match arr {
        Some(a) => {
            if a.is_empty() {
                println!("(无结果)");
            }
            for it in a {
                println!("{}", fmt(it));
            }
        }
        None => println!("{}", pretty(v)),
    }
}

/// 用内嵌的 libreditor 交互编辑文本（类似 git commit 的编辑器体验）。
///
/// 非交互（无 TTY，比如被管道调用）时回退到外部 `$EDITOR` / `$VISUAL`（回退 vi），
/// 保证脚本场景仍然可用。用于 `gc issue edit` / `create` / `reply`、`pr create`、
/// `release create` 等需要正文（以及标题）输入的场景。
pub fn edit_text(initial: &str) -> anyhow::Result<String> {
    edit_text_external(initial)
}

/// 在**同一个**编辑器会话里同时编辑标题（首行）与正文（其余内容），
/// 一次 `:wq` 即可同时保存两者，避免「先编辑标题、再编辑正文」导致要两次 `:wq`。
///
/// 缓冲区布局：第一行是标题，其后（含紧跟的第一个换行）是正文。
pub fn edit_title_body(
    initial_title: &str,
    initial_body: &str,
) -> anyhow::Result<(String, String)> {
    let initial = format!("{}\n{}", initial_title, initial_body);
    let text = edit_text(&initial)?;
    let mut parts = text.splitn(2, '\n');
    let title = parts.next().unwrap_or("").to_string();
    let body = parts.next().unwrap_or("").to_string();
    // 标题按行，去掉可能的 \r（CRLF）与首尾空白。
    let title = title.trim_end_matches('\r').trim().to_string();
    Ok((title, body))
}

/// 外部编辑器回退：写入临时文件，调用 `$EDITOR`，关闭后读回。
pub fn edit_text_external(initial: &str) -> anyhow::Result<String> {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("gc-edit-{}.md", std::process::id()));
    std::fs::write(&path, initial).map_err(|e| anyhow!("写入临时文件失败: {}", e))?;
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .map_err(|e| {
            anyhow!(
                "启动编辑器 {} 失败: {}（可用 EDITOR 环境变量指定）",
                editor,
                e
            )
        })?;
    if !status.success() {
        return Err(anyhow!("编辑器 {} 退出码异常", editor));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| anyhow!("读回临时文件失败: {}", e))?;
    let _ = std::fs::remove_file(&path);
    Ok(text)
}
