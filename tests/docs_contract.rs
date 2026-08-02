use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const ACCEPTED_SCREENSHOT_SHA256: &str =
    "824eed1c2b4ca92e6992412ad564261733709819665599d616bf5dffe7a32697";

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn fenced_shell_blocks(markdown: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in markdown.lines() {
        if let Some(contents) = current.as_mut() {
            if line.trim() == "```" {
                blocks.push(std::mem::take(contents));
                current = None;
            } else {
                contents.push_str(line);
                contents.push('\n');
            }
        } else if matches!(line.trim(), "```sh" | "```bash" | "```shell") {
            current = Some(String::new());
        }
    }

    assert!(current.is_none(), "README has an unterminated shell fence");
    assert!(
        blocks.iter().all(|block| !block
            .lines()
            .any(|line| line.trim_start().starts_with("```"))),
        "public documentation has a nested shell fence"
    );
    blocks
}

fn mise_tasks(mise: &str) -> BTreeSet<String> {
    mise.lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("[tasks.")
                .and_then(|name| name.strip_suffix(']'))
                .map(|name| name.trim_matches('"').to_owned())
        })
        .collect()
}

fn invoked_mise_tasks(blocks: &[String]) -> BTreeSet<String> {
    let mut tasks = BTreeSet::new();
    for block in blocks {
        for line in block.lines() {
            let words: Vec<_> = line.split_ascii_whitespace().collect();
            for index in 0..words.len().saturating_sub(2) {
                if words[index] == "mise" && words[index + 1] == "run" {
                    tasks.insert(
                        words[index + 2]
                            .trim_matches(|character: char| matches!(character, '\'' | '"' | '`'))
                            .to_owned(),
                    );
                }
            }
        }
    }
    tasks
}

#[test]
fn readme_has_the_exact_public_install_path_and_only_real_mise_tasks() {
    let root = repository_root();
    let readme = read(root.join("README.md"));
    let mise = read(root.join("mise.toml"));
    let blocks = fenced_shell_blocks(&readme);
    let mut public_blocks = blocks.clone();
    for path in [
        "CONTRIBUTING.md",
        "SECURITY.md",
        "docs/architecture.md",
        "docs/install.md",
        "docs/upgrading.md",
        "docs/recovery.md",
        "docs/development.md",
        "docs/troubleshooting.md",
        "docs/hardware/hyperpixel2r-driver.md",
    ] {
        public_blocks.extend(fenced_shell_blocks(&read(root.join(path))));
    }
    let expected = concat!(
        "git clone https://github.com/shayne/RPi-Plane-Radar.git\n",
        "cd RPi-Plane-Radar\n",
        "mise install\n",
        "mise run install -- user@host\n",
    );
    assert!(
        blocks.iter().any(|block| block == expected),
        "README must contain the exact four-command source install block"
    );

    let declared = mise_tasks(&mise);
    let invoked = invoked_mise_tasks(&public_blocks);
    assert!(!invoked.is_empty(), "public docs do not invoke a mise task");
    for task in invoked {
        assert!(
            declared.contains(&task),
            "README invokes missing mise task {task:?}"
        );
    }
}

#[test]
fn readme_states_support_maturity_credit_and_disclosure() {
    let readme = read(repository_root().join("README.md"));
    for required in [
        "Raspberry Pi Zero 2 W",
        "Pimoroni HyperPixel 2.1 Round",
        "64-bit Raspberry Pi OS Lite Trixie",
        "macOS",
        "Wi-Fi and SSH must already work",
        "Plane Radar does not configure Wi-Fi",
        "docs/images/planeradar-radar.gif",
        "![Plane Radar running on a Raspberry Pi Zero 2 W](docs/images/planeradar-radar.gif)",
        "https://github.com/MatixYo/ESP32-Plane-Radar",
        "independent Raspberry Pi implementation",
        "not a GitHub fork",
        "https://github.com/shayne/hyperpixel2r-kms",
        "OpenAI Codex",
        "commit trailers",
        "maintainers remain responsible",
        "[MIT License](LICENSE)",
        "docs/recovery.md",
        "immutable stable release",
    ] {
        assert!(
            readme.contains(required),
            "README is missing required public contract text {required:?}"
        );
    }

    for stale in [
        "There is no stable release yet",
        "0.1.0-rc.N",
        "stable release remains gated",
    ] {
        assert!(
            !readme.contains(stale),
            "README still contains pre-release text {stale:?}"
        );
    }

    let install = read(repository_root().join("docs/install.md"));
    for stale in ["There is no stable release yet", "0.1.0-rc.N"] {
        assert!(
            !install.contains(stale),
            "installation guide still contains pre-release text {stale:?}"
        );
    }

    for path in [
        "docs/architecture.md",
        "docs/install.md",
        "docs/upgrading.md",
        "docs/recovery.md",
        "docs/development.md",
        "docs/troubleshooting.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
    ] {
        assert!(
            readme.contains(path),
            "README does not link required public document {path}"
        );
        assert!(
            repository_root().join(path).is_file(),
            "required public document {path} does not exist"
        );
    }
}

#[test]
fn public_docs_describe_release_prompt_uninstall_and_capture_boundaries() {
    let root = repository_root();
    let readme = read(root.join("README.md"));
    for required in [
        "GitHub CLI (`gh`)",
        "authenticated",
        "latest stable release",
        "normal reboot",
        "identity-bound reconnect",
        "retry the exact same uninstall command",
    ] {
        assert!(
            readme.contains(required),
            "README is missing operational fact {required:?}"
        );
    }

    let install = read(root.join("docs/install.md"));
    for required in [
        "only when standard input is an interactive terminal",
        "`--non-interactive`",
        "never prompts for the hostname",
        "release candidates",
        "manifests, checksums, and release identity",
        "stable releases",
        "attestation",
        "bootstrap",
    ] {
        assert!(
            install.contains(required),
            "installation guide is missing operational fact {required:?}"
        );
    }
    assert!(
        install.contains("the discovered numeric `http://` URL"),
        "installation guide must describe the one runtime-selected numeric URL"
    );

    let architecture = read(root.join("docs/architecture.md"));
    for required in [
        "service-owned",
        "`/var/lib/planeradar/debug.png`",
        "root-private",
        "`/var/lib/planeradar-installer/captures/current.png`",
    ] {
        assert!(
            architecture.contains(required),
            "architecture guide is missing capture boundary {required:?}"
        );
    }

    let upgrading = read(root.join("docs/upgrading.md"));
    let recovery = read(root.join("docs/recovery.md"));
    for document in [&upgrading, &recovery] {
        let normalized = document.split_whitespace().collect::<Vec<_>>().join(" ");
        for required in [
            "normal reboot",
            "identity-bound reconnect",
            "exact same uninstall command",
        ] {
            assert!(
                normalized.contains(required),
                "lifecycle guide is missing uninstall fact {required:?}"
            );
        }
    }
    assert!(
        recovery.contains("resume the exact install command")
            && recovery.contains("prior accepted pair"),
        "tryboot recovery must distinguish initial install from upgrade rollback"
    );
}

#[test]
fn development_guide_distinguishes_every_release_verification_path() {
    let development = read(repository_root().join("docs/development.md"));
    let normalized = development.split_whitespace().collect::<Vec<_>>().join(" ");
    for required in [
        "local `--release-dir` verifies its local manifest, checksums, and release identity",
        "If it is a stable draft, it also verifies every runnable artifact attestation without requiring the not-yet-created public tag",
        "explicit source-controller release candidate selected with `--version` verifies its release-candidate manifest, checksums, and release identity",
        "skips the stable-only GitHub release and attestation policy",
        "Published stable source-controller versions add `gh release verify` as well as the runnable artifact attestations",
        "separate release bootstrap verifies release-candidate attestations before executing the downloaded controller",
    ] {
        assert!(
            normalized.contains(required),
            "development guide is missing verification-path fact {required:?}"
        );
    }
}

#[test]
fn owner_guides_describe_optional_radar_controls_and_provider_privacy() {
    let root = repository_root();
    for (name, document) in [
        ("README", read(root.join("README.md"))),
        ("installation guide", read(root.join("docs/install.md"))),
    ] {
        let normalized = document.split_whitespace().collect::<Vec<_>>().join(" ");
        for required in [
            "Aircraft labels",
            "Show callsign",
            "Show origin and destination",
            "Show expanded aircraft model",
            "Footer",
            "Weather condition",
            "Celsius",
            "Fahrenheit",
            "Radar location",
            "Zulu",
            "12-hour",
            "24-hour",
            "Radar text size",
            "Traffic filter",
            "Minimum altitude",
            "Maximum altitude",
            "feet",
            "Blank bounds are open",
            "unknown-altitude aircraft",
            "ADSBDB",
            "Open-Meteo",
            "aircraft callsign",
            "aircraft identifier",
            "configured coordinates",
            "all new provider features are optional",
            "off by default",
            "may share one request",
            "Zulu-only time and date send nothing to Open-Meteo",
        ] {
            assert!(
                normalized.contains(required),
                "{name} is missing optional-radar guidance {required:?}"
            );
        }
    }
}

#[test]
fn architecture_describes_independent_optional_workers_and_snapshot_flow() {
    let architecture = read(repository_root().join("docs/architecture.md"));
    let normalized = architecture
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for required in [
        "three independent workers",
        "ADS-B worker",
        "ADSBDB enrichment worker",
        "Open-Meteo environment worker",
        "three settings wake channels",
        "failure isolation",
        "six hours",
        "ten minutes",
        "15 minutes",
        "schema version 1",
        "immutable enrichment and environment fields",
        "minute-based clock redraws",
        "service errors do not set `DATA STALE`",
        "`WX --`",
        "`WX STALE`",
        "silently falls back",
        "`tests/goldens/settings.png`",
        "physical QR settings screen",
        "must remain unchanged",
        "HTML contracts and viewport inspection",
    ] {
        assert!(
            normalized.contains(required),
            "architecture guide is missing optional-runtime fact {required:?}"
        );
    }
}

#[test]
fn readme_does_not_teach_a_manual_ssh_install() {
    let readme = read(repository_root().join("README.md"));
    for block in fenced_shell_blocks(&readme) {
        let lower = block.to_ascii_lowercase();
        assert!(
            !(lower.contains("ssh ") && lower.contains("install")),
            "README contains a raw manual SSH install command:\n{block}"
        );
        assert!(
            !(lower.contains("sudo ") && lower.contains("planeradar")),
            "README contains a manual sudo Plane Radar operation:\n{block}"
        );
    }
}

#[test]
fn public_markdown_has_no_broken_local_links() {
    let root = repository_root();
    let mut markdown = vec![
        root.join("README.md"),
        root.join("CONTRIBUTING.md"),
        root.join("SECURITY.md"),
    ];
    collect_markdown(&root.join("docs"), &mut markdown);

    for source in markdown.into_iter().filter(|path| path.is_file()) {
        let contents = read(&source);
        for candidate in markdown_destinations(&contents) {
            if candidate.starts_with('#')
                || candidate.starts_with("http://")
                || candidate.starts_with("https://")
                || candidate.starts_with("mailto:")
            {
                continue;
            }
            let target = candidate
                .split('#')
                .next()
                .expect("split always returns one item");
            if target.is_empty() {
                continue;
            }
            let resolved = source
                .parent()
                .expect("Markdown file has a parent")
                .join(target);
            assert!(
                resolved.exists(),
                "{} links to missing local path {candidate:?}",
                source.strip_prefix(&root).unwrap_or(&source).display()
            );
        }
    }
}

fn collect_markdown(directory: &Path, markdown: &mut Vec<PathBuf>) {
    if !directory.is_dir() {
        return;
    }
    for entry in fs::read_dir(directory).expect("read Markdown directory") {
        let path = entry.expect("Markdown directory entry").path();
        if path.is_dir() {
            collect_markdown(&path, markdown);
        } else if path.extension().and_then(|value| value.to_str()) == Some("md") {
            markdown.push(path);
        }
    }
}

fn markdown_destinations(markdown: &str) -> Vec<String> {
    let mut destinations = Vec::new();
    let prose = markdown
        .lines()
        .scan(false, |in_fence, line| {
            if line.trim_start().starts_with("```") {
                *in_fence = !*in_fence;
                return Some("");
            }
            Some(if *in_fence { "" } else { line })
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prose = prose
        .split('`')
        .enumerate()
        .filter_map(|(index, part)| (index % 2 == 0).then_some(part))
        .collect::<Vec<_>>()
        .join("");
    let mut rest = prose.as_str();
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else {
            break;
        };
        let destination = &rest[..end];
        if !destination.contains(char::is_whitespace) {
            destinations.push(destination.to_owned());
        }
        rest = &rest[end + 1..];
    }
    destinations
}

#[test]
fn accepted_device_capture_is_exact_rgba_480_square() {
    let path = repository_root().join("docs/images/planeradar-radar.png");
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        ACCEPTED_SCREENSHOT_SHA256
    );

    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("decode screenshot metadata");
    let info = reader.info();
    assert_eq!((info.width, info.height), (480, 480));
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let mut pixels = vec![0; reader.output_buffer_size().expect("PNG output size")];
    let frame = reader.next_frame(&mut pixels).expect("decode screenshot");
    assert_eq!(frame.buffer_size(), 480 * 480 * 4);
}

#[test]
fn readme_animation_is_native_480_square_and_loops() {
    let path = repository_root().join("docs/images/planeradar-radar.gif");
    let bytes = fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    assert!(
        bytes.len() >= 13,
        "GIF is shorter than its logical screen descriptor"
    );
    assert!(
        &bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a",
        "README animation is not a GIF"
    );
    assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 480);
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 480);
    assert!(
        bytes
            .windows(b"NETSCAPE2.0".len())
            .any(|window| window == b"NETSCAPE2.0"),
        "README animation does not contain an infinite-loop extension"
    );
}

#[test]
fn tracked_files_do_not_contain_maintainer_secrets_or_targets() {
    let root = repository_root();
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(&root)
        .output()
        .expect("list tracked files");
    assert!(output.status.success(), "git ls-files failed");

    for name in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).expect("tracked path is UTF-8");
        let path = root.join(name);
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(contents) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let lower = contents.to_ascii_lowercase();
        let forbidden = [
            ["shayne", "@planeradar"].concat(),
            ["planeradar_pi_target=", "shayne@"].concat(),
            ["shayne", "s!"].concat(),
            ["wifi_", "password="].concat(),
            ["wi-fi ", "password:"].concat(),
        ];
        for forbidden in forbidden {
            assert!(
                !lower.contains(&forbidden),
                "{name} contains forbidden private marker {forbidden:?}"
            );
        }
    }
}

#[test]
fn docs_check_is_real_and_readme_commands_is_its_alias() {
    let mise = read(repository_root().join("mise.toml"));
    assert!(
        mise.contains("[tasks.docs-check]"),
        "mise.toml does not declare docs-check"
    );
    let alias = mise
        .split("[tasks.readme-commands]")
        .nth(1)
        .and_then(|section| section.split("\n[tasks.").next())
        .expect("readme-commands task");
    assert!(
        alias.contains("depends = [\"docs-check\"]"),
        "readme-commands must be an alias of docs-check"
    );
    assert!(
        !alias.contains("grep -q"),
        "readme-commands is still the placeholder grep"
    );
}
