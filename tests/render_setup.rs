mod support;

use qrcode::QrCode;
use qrcode::types::{Color, EcLevel};

use planeradar::render::setup::{
    CANONICAL_LOCAL_URL, SetupRenderer, fixture_required, fixture_settings,
};
use planeradar::render::{FontAsset, Frame};
use support::FrameAssertions;

const SIZE: u32 = 480;
const SAFE_CENTER: (i32, i32) = (240, 240);
const SAFE_RADIUS: i32 = 232;
const QR_LEFT: u32 = 108;
const QR_TOP: u32 = 50;
const QR_MODULE_PIXELS: u32 = 8;
const QR_QUIET_MODULES: u32 = 4;
const WHITE: [u8; 4] = [255, 255, 255, 255];
const INK: [u8; 4] = [4, 10, 28, 255];

fn test_setup_renderer() -> SetupRenderer {
    SetupRenderer::new(FontAsset::embedded().expect("embedded DejaVu font"))
}

fn render(local_url: &str, ip_url: Option<&str>, configured: bool, message: &str) -> Frame {
    test_setup_renderer()
        .render(local_url, ip_url, configured, message)
        .expect("render setup frame")
}

fn expected_code() -> QrCode {
    QrCode::with_error_correction_level(b"http://planeradar.local", EcLevel::M)
        .expect("canonical QR payload")
}

fn assert_canonical_qr(frame: &Frame) {
    let code = expected_code();
    let code_width = u32::try_from(code.width()).expect("QR width");
    let colors = code.into_colors();

    for row in 0..code_width {
        for column in 0..code_width {
            let expected = match colors[usize::try_from(row * code_width + column).unwrap()] {
                Color::Dark => INK,
                Color::Light => WHITE,
            };
            let module_left = QR_LEFT + (QR_QUIET_MODULES + column) * QR_MODULE_PIXELS;
            let module_top = QR_TOP + (QR_QUIET_MODULES + row) * QR_MODULE_PIXELS;
            for y in module_top..module_top + QR_MODULE_PIXELS {
                for x in module_left..module_left + QR_MODULE_PIXELS {
                    assert_eq!(
                        frame.pixel(x, y),
                        expected,
                        "QR module ({column}, {row}) must be an opaque integer-aligned square"
                    );
                }
            }
        }
    }
}

#[test]
fn setup_frame_encodes_only_the_stable_medium_ec_local_url() {
    let cases = [
        (
            "http://planeradar.local",
            Some("http://10.0.4.74"),
            false,
            "Open this page to set the radar location",
        ),
        (
            "https://evil.example/\0?payload=changed",
            Some("http://[::1]/not-a-numeric-ip-url"),
            true,
            "A different message must not alter the code",
        ),
        (
            "not a URL",
            None,
            false,
            "Tap to dismiss and return to the radar",
        ),
    ];

    for (local_url, ip_url, configured, message) in cases {
        let frame = render(local_url, ip_url, configured, message);
        assert_eq!(frame.dimensions(), (SIZE, SIZE));
        assert_canonical_qr(&frame);
    }
}

#[test]
fn qr_has_exactly_four_quiet_modules_and_the_largest_safe_integer_scale() {
    let frame = render(
        CANONICAL_LOCAL_URL,
        Some("http://10.0.4.74"),
        false,
        "Open this page to set the radar location",
    );
    let code_modules = u32::try_from(expected_code().width()).expect("QR width");
    let total_modules = code_modules + QR_QUIET_MODULES * 2;
    let total_pixels = total_modules * QR_MODULE_PIXELS;
    assert_eq!(code_modules, 25);
    assert_eq!(total_pixels, 264);
    assert!(frame.dark_square_count(QR_LEFT, QR_TOP, total_pixels) > 100);
    assert!(frame.region_is_white(QR_LEFT, QR_TOP, total_pixels, 32));

    assert_eq!(
        frame.color_count(WHITE, QR_LEFT, QR_TOP, total_pixels, 32),
        usize::try_from(total_pixels * 32).unwrap()
    );
    assert_eq!(
        frame.color_count(WHITE, QR_LEFT, QR_TOP + total_pixels - 32, total_pixels, 32),
        usize::try_from(total_pixels * 32).unwrap()
    );
    assert_eq!(
        frame.color_count(WHITE, QR_LEFT, QR_TOP, 32, total_pixels),
        usize::try_from(32 * total_pixels).unwrap()
    );
    assert_eq!(
        frame.color_count(WHITE, QR_LEFT + total_pixels - 32, QR_TOP, 32, total_pixels),
        usize::try_from(32 * total_pixels).unwrap()
    );
    assert_eq!(frame.pixel(QR_LEFT + 31, QR_TOP + 32), WHITE);
    assert_eq!(frame.pixel(QR_LEFT + 32, QR_TOP + 32), INK);

    let current_corner = (QR_LEFT as i32, QR_TOP as i32);
    assert!(inside_safe_circle(current_corner.0, current_corner.1));
    let larger_pixels = total_modules * (QR_MODULE_PIXELS + 1);
    let larger_left = i32::try_from((SIZE - larger_pixels) / 2).unwrap();
    assert!(
        !inside_safe_circle(larger_left, QR_TOP as i32),
        "the next integer module scale must not fit the circular-safe geometry"
    );
}

#[test]
fn all_ink_and_alpha_stay_inside_the_circular_safe_region_for_hostile_text() {
    let message = format!(
        "{} END",
        "\0\r\n\tCafé 東京 🛩 a-very-long-unbroken-status-token ".repeat(1_000)
    );
    let frame = render(
        "http://this-argument-is-deliberately-invalid.invalid/\0",
        Some("http://999.999.999.999/".repeat(1_000).as_str()),
        true,
        &message,
    );

    for y in 0..SIZE {
        for x in 0..SIZE {
            let pixel = frame.pixel(x, y);
            assert_eq!(pixel[3], 255, "pixel ({x}, {y}) must be opaque");
            if pixel != WHITE {
                assert!(
                    inside_safe_circle(x as i32, y as i32),
                    "visible pixel ({x}, {y}) falls outside the circular safe region"
                );
            }
        }
    }
}

#[test]
fn network_and_dismissal_control_text_cannot_be_overridden_by_messages_or_urls() {
    let missing_ip = render(
        CANONICAL_LOCAL_URL,
        None,
        false,
        "Open this page to set the radar location",
    );
    let invalid_ip = render(
        "not-a-url",
        Some("javascript:alert(1)"),
        false,
        "Open this page to set the radar location",
    );
    let required_with_dismissal = render(
        CANONICAL_LOCAL_URL,
        None,
        false,
        "Tap to dismiss and return",
    );
    let required_safe_fallback = render(
        CANONICAL_LOCAL_URL,
        None,
        false,
        "Open this page to set the radar location",
    );
    let configured = render(
        CANONICAL_LOCAL_URL,
        None,
        true,
        "Settings are available on this page",
    );

    assert_eq!(
        missing_ip, invalid_ip,
        "missing and invalid numeric URLs must show WAITING FOR NETWORK"
    );
    assert_eq!(
        required_with_dismissal, required_safe_fallback,
        "required setup must never render caller-provided dismissal language"
    );
    assert_ne!(
        configured, required_safe_fallback,
        "configured settings must have distinct TAP TO RETURN control text"
    );
}

#[test]
fn required_fixture_matches_golden() {
    fixture_required()
        .expect("required setup fixture")
        .assert_matches_golden("setup-required");
}

#[test]
fn configured_fixture_matches_golden() {
    fixture_settings()
        .expect("configured settings fixture")
        .assert_matches_golden("settings");
}

fn inside_safe_circle(x: i32, y: i32) -> bool {
    let dx = x - SAFE_CENTER.0;
    let dy = y - SAFE_CENTER.1;
    dx * dx + dy * dy <= SAFE_RADIUS * SAFE_RADIUS
}
