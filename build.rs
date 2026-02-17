use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    let migrations_dir = Path::new("migrations");
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("migrations.rs");

    // 告诉 cargo：migrations 目录变化时重新编译
    println!("cargo:rerun-if-changed=migrations/");

    let mut entries: Vec<String> = Vec::new();

    if migrations_dir.exists() {
        let mut files: Vec<_> = fs::read_dir(migrations_dir)
            .expect("无法读取 migrations 目录")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.ends_with(".sql") && !name.starts_with("000_")
            })
            .collect();

        // 按文件名排序（001_, 002_, 003_...）
        files.sort_by_key(|e| e.file_name());

        for entry in &files {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let name = file_name.trim_end_matches(".sql");
            let path = entry.path().display().to_string();
            entries.push(format!(
                "    (\"{name}\", include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/{path}\")))",
            ));
        }
    }

    let mut f = fs::File::create(&dest_path).expect("无法创建 migrations.rs");
    writeln!(
        f,
        "/// 编译时自动扫描 migrations/ 目录生成（跳过 000_ 开头的文件）\n\
         pub const MIGRATIONS: &[(&str, &str)] = &[\n{}\n];",
        entries.join(",\n")
    )
    .expect("无法写入 migrations.rs");
}
