use std::fs;
use std::path::Path;

use planeradar::render::Frame;

pub trait FrameAssertions {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4];
    fn dark_square_count(&self, left: u32, top: u32, size: u32) -> usize;
    fn region_is_white(&self, left: u32, top: u32, width: u32, height: u32) -> bool;
    fn color_count(&self, color: [u8; 4], left: u32, top: u32, width: u32, height: u32) -> usize;
    fn assert_matches_golden(&self, name: &str);
}

impl FrameAssertions for Frame {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let (width, height) = self.dimensions();
        assert!(x < width && y < height, "pixel ({x}, {y}) outside frame");
        let offset = usize::try_from((y * width + x) * 4).expect("pixel offset");
        self.pixels()[offset..offset + 4]
            .try_into()
            .expect("RGBA pixel")
    }

    fn dark_square_count(&self, left: u32, top: u32, size: u32) -> usize {
        self.color_count(planeradar::render::theme::BACKGROUND, left, top, size, size)
    }

    fn region_is_white(&self, left: u32, top: u32, width: u32, height: u32) -> bool {
        self.color_count(planeradar::render::theme::LABEL, left, top, width, height) > 0
    }

    fn color_count(&self, color: [u8; 4], left: u32, top: u32, width: u32, height: u32) -> usize {
        let (frame_width, frame_height) = self.dimensions();
        let right = left.saturating_add(width).min(frame_width);
        let bottom = top.saturating_add(height).min(frame_height);
        let mut count = 0;
        for y in top.min(frame_height)..bottom {
            for x in left.min(frame_width)..right {
                if self.pixel(x, y) == color {
                    count += 1;
                }
            }
        }
        count
    }

    fn assert_matches_golden(&self, name: &str) {
        let expected_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("goldens")
            .join(format!("{name}.png"));
        let expected = decode_png(&expected_path);
        let (width, height) = self.dimensions();
        if expected.0 == width && expected.1 == height && expected.2 == self.pixels() {
            return;
        }

        let failure_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("golden-failures")
            .join(format!("{name}.actual.png"));
        fs::create_dir_all(
            failure_path
                .parent()
                .expect("golden failure path has parent"),
        )
        .expect("create golden failure directory");
        self.save_png(&failure_path)
            .expect("write actual golden failure image");
        panic!(
            "rendered frame did not match {}; actual written to {}",
            expected_path.display(),
            failure_path.display()
        );
    }
}

fn decode_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let file = fs::File::open(path)
        .unwrap_or_else(|error| panic!("open golden {}: {error}", path.display()));
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder
        .read_info()
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    let output_size = reader
        .output_buffer_size()
        .expect("golden output buffer size");
    let mut buffer = vec![0; output_size];
    let info = reader
        .next_frame(&mut buffer)
        .unwrap_or_else(|error| panic!("decode golden {}: {error}", path.display()));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    (
        info.width,
        info.height,
        buffer[..info.buffer_size()].to_vec(),
    )
}
