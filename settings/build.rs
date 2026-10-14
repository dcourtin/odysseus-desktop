//! Version affichée dans « À propos » : le commit git compilé, s'il est connu.
fn main() {
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(c) = commit {
        println!("cargo:rustc-env=ODYSSEUS_COMMIT=commit {c}");
    }
}
