use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::time::Duration;

use planeradar::capture::{
    CaptureError, CapturePaths, capture_metadata, capture_snapshot_protocol,
    parse_snapshot_protocol,
};
use sha2::{Digest, Sha256};

fn rgba_png(first: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut pixels = vec![0; 480 * 480 * 4];
    pixels[0] = first;
    {
        let mut encoder = png::Encoder::new(&mut bytes, 480, 480);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&pixels).expect("PNG data");
    }
    bytes
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture() -> (tempfile::TempDir, CapturePaths) {
    let directory = tempfile::tempdir().expect("temporary root");
    let service = directory.path().join("service");
    let installer = directory.path().join("installer");
    let captures = installer.join("captures");
    fs::create_dir(&service).expect("service directory");
    fs::create_dir(&installer).expect("installer directory");
    fs::set_permissions(&service, fs::Permissions::from_mode(0o750)).expect("service mode");
    fs::set_permissions(&installer, fs::Permissions::from_mode(0o700)).expect("installer mode");
    let paths = CapturePaths::new(
        service.join("debug.png"),
        captures,
        installer.join("captures/current.png"),
    )
    .expect("safe fixed-shape paths");
    (directory, paths)
}

#[test]
fn privileged_snapshot_binds_freshness_bytes_and_root_capture_identity() {
    let (_directory, paths) = fixture();
    let old = rgba_png(0);
    fs::write(paths.debug_frame(), &old).expect("old frame");
    fs::set_permissions(paths.debug_frame(), fs::Permissions::from_mode(0o600)).expect("old mode");
    let before = capture_metadata(paths.debug_frame())
        .expect("metadata")
        .expect("old frame");

    let fresh = rgba_png(255);
    let replacement = paths.debug_frame().with_extension("replacement");
    fs::write(&replacement, &fresh).expect("replacement frame");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).expect("replacement mode");
    fs::rename(&replacement, paths.debug_frame()).expect("atomic source replacement");

    let protocol = capture_snapshot_protocol(&paths, Some(&before), Duration::from_secs(15))
        .expect("fresh snapshot");
    let snapshot = parse_snapshot_protocol(&protocol).expect("strict protocol");

    assert_ne!(snapshot.source.inode, before.inode);
    assert_eq!(snapshot.source.sha256, sha256(&fresh));
    assert_eq!(snapshot.published.sha256, snapshot.source.sha256);
    assert_eq!(snapshot.rechecked, snapshot.published);
    assert_eq!(snapshot.bytes, fresh);
    assert_eq!(
        fs::read(paths.published_frame()).expect("published capture"),
        snapshot.bytes
    );
    let published = fs::symlink_metadata(paths.published_frame()).expect("published metadata");
    assert!(published.is_file());
    assert_eq!(published.nlink(), 1);
    assert_eq!(published.mode() & 0o777, 0o600);
}

#[test]
fn privileged_snapshot_rejects_stale_and_symlink_sources() {
    let (_directory, paths) = fixture();
    let frame = rgba_png(0);
    fs::write(paths.debug_frame(), &frame).expect("frame");
    fs::set_permissions(paths.debug_frame(), fs::Permissions::from_mode(0o600))
        .expect("frame mode");
    let before = capture_metadata(paths.debug_frame())
        .expect("metadata")
        .expect("frame");
    assert_eq!(
        capture_snapshot_protocol(&paths, Some(&before), Duration::from_millis(20))
            .expect_err("stale frame"),
        CaptureError::TimedOut
    );

    fs::remove_file(paths.debug_frame()).expect("remove frame");
    let victim = paths.debug_frame().with_extension("victim");
    fs::write(&victim, frame).expect("victim");
    symlink(&victim, paths.debug_frame()).expect("source symlink");
    assert_eq!(
        capture_metadata(paths.debug_frame()).expect_err("source symlink"),
        CaptureError::UnsafeSource
    );
}
