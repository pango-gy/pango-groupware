//! iOS 시뮬레이터 빌드에 키체인 권한(entitlement)을 넣는다.
//!
//! 시뮬레이터는 서명이 아니라 실행 파일의 `__TEXT,__entitlements` 섹션에서 권한을 읽는다(Xcode와
//! 같은 방식). dx는 시뮬레이터 빌드에 서명하지 않으므로 여기서 링크 단계에 섹션을 붙인다.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.ends_with("apple-ios-sim") || target == "x86_64-apple-ios" {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/simulator.entitlements");
        println!("cargo:rerun-if-changed={}", path.display());
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__entitlements,{}",
            path.display()
        );
    }
}
