use std::path::Path;

#[test]
fn application_has_no_raw_hyperpixel_i2c_path() {
    let manifest = include_str!("../Cargo.toml");
    let display = include_str!("../src/display.rs");
    let library = include_str!("../src/lib.rs");

    assert!(!manifest.contains("i2cdev"));
    assert!(!display.contains("/dev/i2c-"));
    assert!(!display.contains("HyperpixelTouch"));
    assert!(!library.contains("mod hyperpixel"));
    assert!(!Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/hyperpixel.rs")).exists());
}
