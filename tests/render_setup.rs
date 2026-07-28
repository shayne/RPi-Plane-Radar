mod support;

use fontdue::{Font, FontSettings};
use qrcode::QrCode;
use qrcode::types::{Color, EcLevel};
use tiny_skia::Pixmap;

use planeradar::render::setup::{
    CANONICAL_LOCAL_URL, SetupRenderer, fixture_required, fixture_settings,
};
use planeradar::render::text::{HorizontalAnchor, TextRasterizer, TextStyle, VerticalAnchor};
use planeradar::render::{FontAsset, Frame};
use support::FrameAssertions;

const SIZE: u32 = 480;
const SAFE_CENTER: (i32, i32) = (240, 240);
const SAFE_RADIUS: i32 = 232;
const QR_LEFT: u32 = 108;
const QR_TOP: u32 = 50;
const QR_MODULE_PIXELS: u32 = 8;
const QR_QUIET_MODULES: u32 = 4;
const BACKGROUND: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const INK: [u8; 4] = [0, 0, 0, 255];
const LIGHT_TEXT: [u8; 4] = [255, 255, 255, 255];

fn test_setup_renderer() -> SetupRenderer {
    SetupRenderer::new(FontAsset::embedded().expect("embedded DejaVu font"))
}

fn render(local_url: &str, ip_url: Option<&str>, configured: bool, message: &str) -> Frame {
    test_setup_renderer()
        .render(local_url, ip_url, configured, message)
        .expect("render setup frame")
}

fn expected_code(local_url: &str) -> QrCode {
    QrCode::with_error_correction_level(local_url.as_bytes(), EcLevel::M).expect("QR payload")
}

fn assert_qr(frame: &Frame, local_url: &str) {
    let code = expected_code(local_url);
    let code_width = u32::try_from(code.width()).expect("QR width");
    let colors = code.into_colors();
    let total_modules = code_width + QR_QUIET_MODULES * 2;

    for row in 0..total_modules {
        for column in 0..total_modules {
            let expected = if row < QR_QUIET_MODULES
                || column < QR_QUIET_MODULES
                || row >= QR_QUIET_MODULES + code_width
                || column >= QR_QUIET_MODULES + code_width
            {
                WHITE
            } else {
                match colors[usize::try_from(
                    (row - QR_QUIET_MODULES) * code_width + (column - QR_QUIET_MODULES),
                )
                .unwrap()]
                {
                    Color::Dark => INK,
                    Color::Light => WHITE,
                }
            };
            let module_left = QR_LEFT + column * QR_MODULE_PIXELS;
            let module_top = QR_TOP + row * QR_MODULE_PIXELS;
            for y in module_top..module_top + QR_MODULE_PIXELS {
                for x in module_left..module_left + QR_MODULE_PIXELS {
                    assert_eq!(
                        frame.pixel(x, y),
                        expected,
                        "QR tile module ({column}, {row}) must be an opaque integer-aligned square"
                    );
                }
            }
        }
    }
}

fn assert_opaque_and_content_stays_inside_the_circular_safe_region(frame: &Frame) {
    for y in 0..SIZE {
        for x in 0..SIZE {
            let pixel = frame.pixel(x, y);
            assert_eq!(pixel[3], 255, "pixel ({x}, {y}) must be opaque");
            if pixel != BACKGROUND {
                assert!(
                    inside_safe_circle(x as i32, y as i32),
                    "visible pixel ({x}, {y}) falls outside the circular safe region"
                );
            }
        }
    }
}

fn reference_text_frame(lines: &[(&str, f32, f32)]) -> Frame {
    let font = Font::from_bytes(
        include_bytes!("../src/assets/DejaVuSans-Bold.ttf").as_slice(),
        FontSettings {
            collection_index: 0,
            scale: 40.0,
            load_substitutions: true,
        },
    )
    .expect("reference font");
    let text = TextRasterizer::new(&font);
    let mut pixmap = Pixmap::new(SIZE, SIZE).expect("reference pixmap");
    pixmap.fill(tiny_skia::Color::from_rgba8(
        BACKGROUND[0],
        BACKGROUND[1],
        BACKGROUND[2],
        BACKGROUND[3],
    ));
    for &(line, top, cap_height) in lines {
        text.draw(
            &mut pixmap,
            line,
            SIZE as f32 / 2.0,
            top,
            TextStyle {
                cap_height,
                color: LIGHT_TEXT,
                horizontal: HorizontalAnchor::Center,
                vertical: VerticalAnchor::Top,
            },
        );
    }
    Frame::new(SIZE, SIZE, pixmap.take()).expect("reference frame")
}

fn assert_region_matches_reference(
    frame: &Frame,
    reference: &Frame,
    top: u32,
    height: u32,
    label: &str,
) {
    for y in top..top + height {
        for x in 0..SIZE {
            assert_eq!(
                frame.pixel(x, y),
                reference.pixel(x, y),
                "{label} mismatch at ({x}, {y})"
            );
        }
    }
}

#[test]
fn setup_frame_encodes_the_provided_local_url() {
    let local_url = "http://hangar-2.local";
    let frame = render(
        local_url,
        Some("http://10.0.4.74"),
        false,
        "Open this page to set the radar location",
    );

    assert_eq!(frame.dimensions(), (SIZE, SIZE));
    assert_qr(&frame, local_url);
}

#[test]
fn setup_frame_uses_a_black_canvas_white_qr_tile_and_light_text() {
    let frame = render(
        CANONICAL_LOCAL_URL,
        Some("http://10.0.4.74"),
        false,
        "Open this page to set the radar location",
    );

    for (x, y) in [(0, 0), (479, 0), (0, 479), (479, 479), (240, 470)] {
        assert_eq!(frame.pixel(x, y), BACKGROUND, "canvas pixel ({x}, {y})");
    }
    assert_qr(&frame, CANONICAL_LOCAL_URL);
    assert!(
        frame.color_count(LIGHT_TEXT, 0, 318, SIZE, 145) > 0,
        "surrounding setup text must remain light and readable"
    );
    assert_opaque_and_content_stays_inside_the_circular_safe_region(&frame);
}

#[test]
fn qr_has_exactly_four_quiet_modules_and_the_largest_safe_integer_scale() {
    let frame = render(
        CANONICAL_LOCAL_URL,
        Some("http://10.0.4.74"),
        false,
        "Open this page to set the radar location",
    );
    let code_modules = u32::try_from(expected_code(CANONICAL_LOCAL_URL).width()).expect("QR width");
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

    assert_opaque_and_content_stays_inside_the_circular_safe_region(&frame);
}

#[test]
fn longest_valid_ipv6_urls_are_measured_to_fit_the_round_safe_region() {
    for ip_url in [
        "http://[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        "http://[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]:65535",
    ] {
        let frame = render(
            CANONICAL_LOCAL_URL,
            Some(ip_url),
            true,
            "Settings are available on this page",
        );
        assert_qr(&frame, CANONICAL_LOCAL_URL);
        assert_opaque_and_content_stays_inside_the_circular_safe_region(&frame);
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

    let waiting_reference = reference_text_frame(&[
        ("http://planeradar.local", 322.0, 20.0),
        ("WAITING FOR NETWORK", 351.0, 17.0),
    ]);
    assert_region_matches_reference(
        &missing_ip,
        &waiting_reference,
        346,
        34,
        "WAITING FOR NETWORK",
    );
    assert_region_matches_reference(
        &invalid_ip,
        &waiting_reference,
        346,
        34,
        "invalid URL network status",
    );
    let required_reference = reference_text_frame(&[
        ("Open this page to set the", 381.0, 14.0),
        ("radar location", 401.0, 14.0),
    ]);
    assert_region_matches_reference(
        &required_safe_fallback,
        &required_reference,
        378,
        46,
        "fixed required instruction",
    );
    let configured_control_reference = reference_text_frame(&[("TAP TO RETURN", 428.0, 12.0)]);
    assert_region_matches_reference(
        &configured,
        &configured_control_reference,
        424,
        32,
        "TAP TO RETURN",
    );
}

#[test]
fn required_instruction_ignores_disguised_dismissal_messages() {
    let expected = render(
        CANONICAL_LOCAL_URL,
        None,
        false,
        "Open this page to set the radar location",
    );
    for disguised in [
        "T\u{200b}AP TO DIS\u{200b}MISS AND RE\u{200b}TURN",
        "T\u{2060}AP TO DIS\u{2060}MISS AND RE\u{2060}TURN",
        "T\u{200c}AP TO DIS\u{200d}MISS AND RE\u{feff}TURN",
        "T\0AP TO DIS\u{1f}MISS AND RE\tTURN",
    ] {
        assert!(
            render(CANONICAL_LOCAL_URL, None, false, disguised) == expected,
            "required setup must ignore caller message {disguised:?}"
        );
    }
}

#[test]
fn oversized_raw_whitespace_url_is_rejected_before_trim_can_reveal_a_valid_url() {
    let huge_candidate = format!(
        "{}http://10.0.4.74{}",
        " ".repeat(1_000_000),
        " ".repeat(1_000_000)
    );
    let frame = render(
        CANONICAL_LOCAL_URL,
        Some(&huge_candidate),
        true,
        "Settings are available on this page",
    );
    let waiting_reference = reference_text_frame(&[
        ("http://planeradar.local", 322.0, 20.0),
        ("WAITING FOR NETWORK", 351.0, 17.0),
    ]);
    assert_region_matches_reference(&frame, &waiting_reference, 346, 34, "oversized raw URL");
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
