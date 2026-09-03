// 路由可达性护栏。
//
// 背景：TS SDK 曾按「协议里有这条路由常量」就给它加了客户端封装，结果发布出去
// 三个方法打到的 handler 是 `Ok(json!({"status":"success"}))` 的占位实现，
// 调用方拿到假成功。另有一整个模块（channel_broadcast）定义了 register_routes
// 却从没被 rpc/mod.rs 调用过，5 条路由不可达。
//
// 这类问题在客户端侧的测试里永远看不见——那边只能验证自己写下的字符串等于
// 自己写下的字符串。所以护栏放在 server：直接扫源码，把「假成功 handler」和
// 「定义了 register_routes 却没人调用的模块」钉成已知清单，再出现新的就红。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn rpc_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/rpc")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("读取目录失败") {
        let path = entry.expect("读取目录项失败").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn rel(path: &Path) -> String {
    path.strip_prefix(rpc_dir())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 占位 handler 的特征：函数体里带 `// TODO: 实现` 且直接返回 `"status": "success"`。
fn is_fake_success_handler(source: &str) -> bool {
    source.contains("// TODO: 实现") && source.contains("\"status\": \"success\"")
}

#[test]
fn 假成功占位_handler_不得增加() {
    let mut files = Vec::new();
    rust_files(&rpc_dir(), &mut files);

    let found: BTreeSet<String> = files
        .iter()
        .filter(|p| is_fake_success_handler(&fs::read_to_string(p).expect("读取文件失败")))
        .map(|p| rel(p))
        .collect();

    // 已知欠债，逐条清理时从这里删。**只减不增**。
    //
    // 注意区分两种欠债：
    //   - account/profile/*、account/user/update：**已注册**，客户端调得到，
    //     拿到的是假成功。这类最危险，绝不能有客户端封装。
    //   - contact/block/*、channel_broadcast/*：register_routes 是空的 / 从没被
    //     调用，纯死文件。不可达，但留着就有人会照着"协议里有"去接。
    let known: BTreeSet<String> = [
        "account/profile/get.rs",
        "account/profile/update.rs",
        "account/user/update.rs",
        "channel_broadcast/channel/create.rs",
        "channel_broadcast/content/list.rs",
        "channel_broadcast/content/publish.rs",
        "contact/block/add.rs",
        "contact/block/list.rs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let added: Vec<_> = found.difference(&known).collect();
    assert!(
        added.is_empty(),
        "出现新的假成功占位 handler，不要给它们加客户端封装：{:#?}",
        added
    );

    let cleaned: Vec<_> = known.difference(&found).collect();
    assert!(
        cleaned.is_empty(),
        "这些 handler 已经实现了，请从 known 清单里删掉：{:#?}",
        cleaned
    );
}

#[test]
fn 定义了_register_routes_的模块必须被调用() {
    let mut files = Vec::new();
    rust_files(&rpc_dir(), &mut files);

    let root = fs::read_to_string(rpc_dir().join("mod.rs")).expect("读取 rpc/mod.rs 失败");

    // 顶层模块 = src/rpc/<name>/mod.rs 或 src/rpc/<name>.rs
    let mut unreachable = Vec::new();
    for path in &files {
        let r = rel(path);
        let top = match r.split('/').next() {
            Some(t) if r.ends_with("/mod.rs") && r.matches('/').count() == 1 => t.to_string(),
            _ => continue,
        };
        let source = fs::read_to_string(path).expect("读取文件失败");
        if !source.contains("pub async fn register_routes") {
            continue;
        }
        if !root.contains(&format!("{}::register_routes", top)) {
            unreachable.push(top);
        }
    }

    assert_eq!(
        unreachable,
        vec!["channel_broadcast".to_string()],
        "顶层模块的 register_routes 调用情况变了。新增的不可达模块必须接线或删除；\
         channel_broadcast 是有意不接：create/publish/list 是占位实现，且模块内注册的
         路由名是 channel/channel/*，和 protocol 的 channel/broadcast/* 对不上"
    );
}
