use std::process::Command;

#[test]
fn prints_its_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_orchestral"))
        .arg("--version")
        .output()
        .expect("the binary should run");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        concat!("orchestral ", env!("CARGO_PKG_VERSION"), "\n")
    );
}
