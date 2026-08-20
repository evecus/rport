//! 编译前置检查：确保 `web-ui/dist`（前端构建产物）存在，供 `rust-embed` 内嵌。
//!
//! 行为：
//! - 若 `web-ui/dist/index.html` 已存在，直接跳过（CI 里通常会先手动 `npm run build`）
//! - 否则尝试自动执行 `npm ci && npm run build`（本地开发时的便利）
//! - 若 npm 不可用或构建失败，报出清晰的编译错误，而不是让后续 rust-embed
//!   产生一个难以理解的"目录不存在"报错

use std::path::Path;
use std::process::Command;

fn main() {
    let web_ui_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web-ui");
    let dist_index = web_ui_dir.join("dist").join("index.html");

    println!("cargo:rerun-if-changed={}", web_ui_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        web_ui_dir.join("index.html").display()
    );

    if dist_index.exists() {
        return;
    }

    eprintln!(
        "tunx build.rs: web-ui/dist 不存在，尝试自动执行 `npm ci && npm run build` \
         （目录: {}）",
        web_ui_dir.display()
    );

    let npm = if cfg!(target_os = "windows") { "npm.cmd" } else { "npm" };

    let install_ok = Command::new(npm)
        .arg("ci")
        .current_dir(&web_ui_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !install_ok {
        // npm ci 依赖 package-lock.json 且要求严格匹配；失败时退回 npm install
        let _ = Command::new(npm)
            .arg("install")
            .current_dir(&web_ui_dir)
            .status();
    }

    let build_ok = Command::new(npm)
        .arg("run")
        .arg("build")
        .current_dir(&web_ui_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !build_ok || !dist_index.exists() {
        panic!(
            "\n\n\
             ────────────────────────────────────────────────────────────\n\
             无法自动构建前端（web-ui/dist/index.html 仍不存在）。\n\
             请手动执行：\n\
               cd web-ui && npm ci && npm run build\n\
             然后重新 `cargo build`。\n\
             （需要 Node.js 18+ 和 npm；CI 环境建议在 cargo build 前显式构建前端，\n\
             而不是依赖这里的自动构建。）\n\
             ────────────────────────────────────────────────────────────\n"
        );
    }
}
