use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

#[test]
fn driver_artifact_bundle_matches_its_manifest() {
    let Ok(artifact_dir) = env::var("PLANERADAR_DRIVER_ARTIFACT_DIR") else {
        println!("PLANERADAR_DRIVER_ARTIFACT_DIR is unset; skipping driver artifact test");
        return;
    };
    let artifact_dir = Path::new(&artifact_dir);
    let module_path = artifact_dir.join("planeradar_hyperpixel2r.ko");
    let manifest_path = artifact_dir.join("manifest.txt");
    let checksum_path = artifact_dir.join("module.sha256");
    let modinfo_path = artifact_dir.join("module.modinfo.txt");

    for path in [&module_path, &manifest_path, &checksum_path, &modinfo_path] {
        assert!(
            path.is_file(),
            "required artifact is missing: {}",
            path.display()
        );
    }

    let manifest_text =
        fs::read_to_string(&manifest_path).expect("manifest.txt must be valid UTF-8");
    let mut manifest = BTreeMap::new();
    for (line_number, line) in manifest_text.lines().enumerate() {
        let (key, value) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("manifest line {} is not tab-separated", line_number + 1));
        assert!(
            !key.is_empty() && !value.is_empty() && !value.contains('\t'),
            "manifest line {} must contain exactly one non-empty key/value pair",
            line_number + 1
        );
        assert!(
            manifest.insert(key, value).is_none(),
            "manifest key is duplicated: {key}"
        );
    }

    const REQUIRED_KEYS: &[&str] = &[
        "source_revision",
        "source_dirty",
        "kernel_release",
        "kernel_arch",
        "build_image",
        "build_command",
        "base_dtb_sha256",
        "module_file",
        "module_sha256",
        "module_vermagic",
        "module_license",
    ];
    for key in REQUIRED_KEYS {
        assert!(manifest.contains_key(key), "manifest key is missing: {key}");
    }

    assert!(
        matches!(manifest["source_dirty"], "true" | "false"),
        "source_dirty must be exactly true or false"
    );
    assert_eq!(manifest["kernel_arch"], "aarch64");
    assert_eq!(manifest["module_license"], "GPL");
    assert_eq!(
        manifest["build_image"],
        "planeradar-kernel-builder:debian-trixie-gcc14"
    );
    assert_eq!(
        manifest["build_command"],
        format!(
            "make -C /usr/src/linux-headers-{} M=/build/kernel ARCH=arm64 \
             CROSS_COMPILE=aarch64-linux-gnu- W=1 modules",
            manifest["kernel_release"]
        )
    );
    assert!(
        manifest["module_vermagic"].starts_with(manifest["kernel_release"]),
        "module_vermagic must begin with kernel_release"
    );
    assert_eq!(
        manifest["module_file"], "planeradar_hyperpixel2r.ko",
        "module_file must name the required module"
    );

    let module = fs::read(&module_path).expect("module must be readable");
    let actual_checksum = format!("{:x}", Sha256::digest(module));
    assert_eq!(manifest["module_sha256"], actual_checksum);

    let checksum_text =
        fs::read_to_string(&checksum_path).expect("module.sha256 must be valid UTF-8");
    let checksum_fields: Vec<_> = checksum_text.split_whitespace().collect();
    assert_eq!(
        checksum_fields,
        [actual_checksum.as_str(), "planeradar_hyperpixel2r.ko"],
        "module.sha256 must be a sha256sum record for the module"
    );
}
