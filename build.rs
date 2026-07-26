fn main() {
    println!("cargo:rerun-if-env-changed=PLANERADAR_REVISION");
    let revision =
        std::env::var("PLANERADAR_REVISION").unwrap_or_else(|_| "development".to_owned());
    println!("cargo:rustc-env=PLANERADAR_REVISION={revision}");
}
