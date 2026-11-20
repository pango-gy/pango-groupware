//! 웹 번들 디렉터리가 없어도 서버가 빌드되도록 자리 표시 파일을 만든다.
//! 실제 번들은 `cargo xtask web`이 만든다.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("web-dist");
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create web-dist");
        std::fs::write(
            &index,
            "<!doctype html><meta charset=\"utf-8\"><title>pango</title>\
             <p>웹 UI가 빌드되지 않았습니다. <code>cargo xtask web</code>을 실행하세요.</p>",
        )
        .expect("write placeholder index.html");
    }
    println!("cargo:rerun-if-changed=web-dist");
}
