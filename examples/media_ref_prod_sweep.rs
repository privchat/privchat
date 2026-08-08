// 生产 metadata 全量普查：把导出的 (content_type, metadata) 逐条喂给
// legacy 适配器，报告有多少条会被 strict 策略拒绝、拒绝原因是什么。
//
// 这是「敢不敢把 strict 开到线上」的判据——凭直觉开 strict 会在
// 用户发图那一刻集体失败。用法：
//   cargo run -p privchat --example media_ref_prod_sweep -- /tmp/media_meta.jsonl
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};

fn main() {
    let path = std::env::args().nth(1).expect("用法: <jsonl 路径>");
    let file = std::fs::File::open(&path).expect("打不开导出文件");
    let mut total = 0usize;
    let mut refused = 0usize;
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut ref_counts: BTreeMap<usize, usize> = BTreeMap::new();

    for line in BufReader::new(file).lines() {
        let line = line.expect("读行失败");
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(&line).expect("导出行不是 JSON");
        let content_type = row["t"].as_i64().expect("缺 t") as i32;
        let metadata = &row["m"];
        total += 1;

        let report = privchat::service::legacy_media_refs::parse_legacy_media_refs_by_code(
            content_type,
            metadata,
        );
        *ref_counts.entry(report.refs.len()).or_default() += 1;
        if let Err(audit) = report.into_strict() {
            refused += 1;
            *reasons.entry(format!("{audit:?}")).or_default() += 1;
        }
    }

    println!("总行数            {total}");
    println!("strict 会拒绝     {refused}  ({:.4}%)", refused as f64 * 100.0 / total.max(1) as f64);
    println!("引用条数分布      {ref_counts:?}");
    for (reason, count) in &reasons {
        println!("  拒绝原因 {reason} × {count}");
    }
}
