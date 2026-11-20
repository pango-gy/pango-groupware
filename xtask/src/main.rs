//! 개발 작업.
//!
//! `cargo xtask web [--dev]` : 웹 UI를 WASM으로 빌드해 `crates/server/web-dist`에 둔다.
//!                             서버는 이 디렉터리를 실행 파일에 포함한다.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

const WASM_BINDGEN_VERSION: &str = "0.2.130";

/// CSP가 인라인 스크립트를 막으므로 시작 코드는 별도 파일로 둔다. `{v}`는 버전으로 바뀐다.
const BOOT_TEMPLATE: &str = "import init from \"./app.js?v={v}\";\n\
    init({ module_or_path: new URL(\"./app_bg.wasm?v={v}\", import.meta.url) });\n";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn run(command: &mut Command) -> AnyResult<()> {
    let status = command
        .status()
        .map_err(|e| format!("failed to start {command:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("command failed: {command:?}").into())
    }
}

/// 파일 내용으로 만든 짧은 버전 문자열. 내용이 바뀌면 URL이 바뀌어 캐시가 무효화된다.
fn content_version(paths: &[PathBuf]) -> AnyResult<String> {
    let mut hasher = DefaultHasher::new();
    // 시작 스크립트의 모양이 바뀌어도 버전이 바뀌어야 한다.
    BOOT_TEMPLATE.hash(&mut hasher);
    for path in paths {
        std::fs::read(path)?.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn build_web(dev: bool) -> AnyResult<()> {
    let root = root();
    let profile = if dev { "dev" } else { "wasm-release" };
    let profile_dir = if dev { "debug" } else { "wasm-release" };
    run(Command::new("cargo").current_dir(&root).args([
        "build",
        "--package",
        "pango-web",
        "--target",
        "wasm32-unknown-unknown",
        "--profile",
        profile,
    ]))?;

    let dist = root.join("crates/server/web-dist");
    let assets = dist.join("assets");
    if dist.exists() {
        std::fs::remove_dir_all(&dist)?;
    }
    std::fs::create_dir_all(&assets)?;

    let wasm = root.join(format!(
        "target/wasm32-unknown-unknown/{profile_dir}/pango-web.wasm"
    ));
    run(Command::new("wasm-bindgen")
        .args([
            "--target",
            "web",
            "--no-typescript",
            "--out-name",
            "app",
            "--out-dir",
        ])
        .arg(&assets)
        .arg(&wasm))
    .map_err(|e| {
        format!(
            "{e}\n힌트: cargo install wasm-bindgen-cli --version {WASM_BINDGEN_VERSION} --locked"
        )
    })?;

    let web = root.join("crates/web");
    std::fs::copy(web.join("assets/style.css"), assets.join("style.css"))?;

    let version = content_version(&[
        assets.join("app.js"),
        assets.join("app_bg.wasm"),
        assets.join("style.css"),
    ])?;
    std::fs::write(
        assets.join("boot.js"),
        BOOT_TEMPLATE.replace("{v}", &version),
    )?;
    let index = std::fs::read_to_string(web.join("index.html"))?.replace("{{version}}", &version);
    std::fs::write(dist.join("index.html"), index)?;

    println!("web bundle → {}", dist.display());
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("web") => build_web(args.iter().any(|arg| arg == "--dev")),
        _ => Err("usage: cargo xtask web [--dev]".into()),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
