use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::NamedTempFile;

use crate::util::command_output_allow_failure;

const ATOS_LOAD_ADDRESS: u64 = 0x1000000;

#[derive(Debug, Clone)]
pub(super) struct SymbolicationImage {
    pub(super) path: String,
    pub(super) uuid: Option<String>,
    pub(super) load_address: u64,
    pub(super) size: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct SymbolicatedAddress {
    pub(super) display: String,
    pub(super) is_missing: bool,
    is_user_code: bool,
    is_hidden: bool,
}

#[derive(Debug, Default)]
pub(super) struct TraceSymbolication {
    symbols: HashMap<u64, SymbolicatedAddress>,
    pub(super) total_unique_addresses: usize,
    pub(super) symbolicated_addresses: usize,
}

#[derive(Debug)]
pub(super) struct SymbolicationRequest<'a> {
    pub(super) trace_path: &'a Path,
    pub(super) arch: Option<&'a str>,
    pub(super) dsym_dirs: &'a [PathBuf],
    pub(super) images: &'a [SymbolicationImage],
    pub(super) addresses: BTreeSet<u64>,
}

impl TraceSymbolication {
    pub(super) fn symbolicate(request: SymbolicationRequest<'_>) -> Self {
        let total_unique_addresses = request.addresses.len();
        let mut symbols = HashMap::new();
        if request.images.is_empty() || request.addresses.is_empty() {
            return Self {
                symbols,
                total_unique_addresses,
                symbolicated_addresses: 0,
            };
        }

        let sorted_images = sorted_images(request.images);
        let mut image_offsets: BTreeMap<usize, BTreeSet<u64>> = BTreeMap::new();
        for address in request.addresses {
            if let Some((image_index, offset)) = image_for_address(address, &sorted_images) {
                image_offsets.entry(image_index).or_default().insert(offset);
            }
        }

        let search_dirs = dsym_search_dirs(request.trace_path, request.dsym_dirs);
        for (image_index, offsets) in image_offsets {
            let image = &sorted_images[image_index];
            let binary = binary_for_image(image, &search_dirs);
            let resolved = binary
                .as_deref()
                .map(|binary| atos_symbols(binary, request.arch, &offsets))
                .unwrap_or_default();

            for offset in offsets {
                let address = image.load_address.saturating_add(offset);
                if let Some(symbol) = resolved.get(&offset) {
                    symbols.insert(
                        address,
                        SymbolicatedAddress {
                            display: symbol.clone(),
                            is_missing: false,
                            is_user_code: is_user_image(image),
                            is_hidden: is_hidden_image(image),
                        },
                    );
                } else {
                    symbols.insert(
                        address,
                        SymbolicatedAddress {
                            display: fallback_symbol(image, offset),
                            is_missing: true,
                            is_user_code: is_user_image(image),
                            is_hidden: is_hidden_image(image),
                        },
                    );
                }
            }
        }

        let symbolicated_addresses = symbols.values().filter(|symbol| !symbol.is_missing).count();
        Self {
            symbols,
            total_unique_addresses,
            symbolicated_addresses,
        }
    }

    pub(super) fn display_address(&self, raw_address: &str) -> String {
        parse_address(raw_address)
            .and_then(|address| self.symbols.get(&address))
            .map(|symbol| symbol.display.clone())
            .unwrap_or_else(|| raw_address.to_owned())
    }

    fn frame_for_address(&self, raw_address: &str) -> RenderedFrame {
        parse_address(raw_address)
            .and_then(|address| self.symbols.get(&address))
            .map(|symbol| RenderedFrame {
                display: symbol.display.clone(),
                is_user_code: symbol.is_user_code,
                is_hidden: symbol.is_hidden || is_hidden_symbol(&symbol.display),
            })
            .unwrap_or_else(|| RenderedFrame {
                display: raw_address.to_owned(),
                is_user_code: false,
                is_hidden: false,
            })
    }

    #[cfg(test)]
    pub(super) fn for_test(symbols: impl IntoIterator<Item = (u64, &'static str)>) -> Self {
        let symbols = symbols
            .into_iter()
            .map(|(address, display)| {
                (
                    address,
                    SymbolicatedAddress {
                        display: display.to_owned(),
                        is_missing: false,
                        is_user_code: true,
                        is_hidden: false,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        Self {
            total_unique_addresses: symbols.len(),
            symbolicated_addresses: symbols.len(),
            symbols,
        }
    }
}

#[derive(Debug)]
struct RenderedFrame {
    display: String,
    is_user_code: bool,
    is_hidden: bool,
}

#[derive(Debug)]
pub(super) struct RenderedStack {
    pub(super) display: String,
    pub(super) has_user_code: bool,
}

pub(super) fn parse_address(raw_address: &str) -> Option<u64> {
    let trimmed = raw_address.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u64::from_str_radix(hex, 16).ok()
}

fn sorted_images(images: &[SymbolicationImage]) -> Vec<SymbolicationImage> {
    let mut sorted = images.to_vec();
    sorted.sort_by(|left, right| {
        left.load_address
            .cmp(&right.load_address)
            .then_with(|| left.path.cmp(&right.path))
    });
    sorted
}

fn image_for_address(address: u64, images: &[SymbolicationImage]) -> Option<(usize, u64)> {
    for (index, image) in images.iter().enumerate() {
        if address < image.load_address {
            continue;
        }
        if let Some(size) = image.size {
            if address >= image.load_address.saturating_add(size) {
                continue;
            }
        } else if let Some(next_image) = images.get(index + 1)
            && address >= next_image.load_address
        {
            continue;
        }
        return Some((index, address - image.load_address));
    }
    None
}

fn dsym_search_dirs(trace_path: &Path, explicit_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = explicit_dirs.to_vec();
    if let Some(parent) = trace_path.parent() {
        dirs.push(parent.to_path_buf());
        if let Some(grandparent) = parent.parent() {
            dirs.push(grandparent.to_path_buf());
        }
    }
    dirs
}

fn binary_for_image(image: &SymbolicationImage, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(dsym) = dsym_for_image(image, search_dirs) {
        return Some(dsym);
    }
    if let Some(dsym) = spotlight_dsym_for_image(image) {
        return Some(dsym);
    }
    let binary = PathBuf::from(&image.path);
    if binary.is_file() {
        return Some(binary);
    }
    device_support_binary_for_image(image)
}

fn dsym_for_image(image: &SymbolicationImage, search_dirs: &[PathBuf]) -> Option<PathBuf> {
    let path = Path::new(&image.path);
    let binary_name = path.file_name()?.to_string_lossy();
    let folder_extension = if image.path.contains(".framework/") {
        "framework"
    } else if image.path.contains(".app/") {
        "app"
    } else {
        "dSYM"
    };

    for dir in search_dirs {
        let candidates = [
            dir.join(format!("{binary_name}.{folder_extension}.dSYM")),
            dir.join(format!("{binary_name}.dSYM")),
        ];
        for candidate in candidates {
            if let Some(dwarf) = first_dwarf_file(&candidate) {
                return Some(dwarf);
            }
        }
    }
    None
}

fn spotlight_dsym_for_image(image: &SymbolicationImage) -> Option<PathBuf> {
    let uuid = image.uuid.as_ref()?;
    let mut command = Command::new("/usr/bin/mdfind");
    command.arg(format!("com_apple_xcode_dsym_uuids == {uuid}"));
    let (success, stdout, _) = command_output_allow_failure(&mut command).ok()?;
    if !success {
        return None;
    }
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .find_map(|path| first_dwarf_file(&path))
}

fn device_support_binary_for_image(image: &SymbolicationImage) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let roots = [
        home.join("Library/Developer/Xcode/iOS DeviceSupport"),
        home.join("Library/Developer/Xcode/watchOS DeviceSupport"),
        home.join("Library/Developer/Xcode/tvOS DeviceSupport"),
    ];
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry
                .path()
                .join("Symbols")
                .join(image.path.trim_start_matches('/'));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn first_dwarf_file(dsym: &Path) -> Option<PathBuf> {
    let dwarf_dir = dsym.join("Contents/Resources/DWARF");
    fs::read_dir(dwarf_dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_file())
}

fn atos_symbols(
    binary: &Path,
    requested_arch: Option<&str>,
    offsets: &BTreeSet<u64>,
) -> HashMap<u64, String> {
    if offsets.is_empty() {
        return HashMap::new();
    }
    let arch = arch_for_binary(binary, requested_arch);
    let mut addresses_file = match NamedTempFile::new() {
        Ok(file) => file,
        Err(_) => return HashMap::new(),
    };
    let mut input_addresses = Vec::with_capacity(offsets.len());
    for offset in offsets {
        let address = offset.saturating_add(ATOS_LOAD_ADDRESS);
        input_addresses.push(address);
        if writeln!(addresses_file, "{address:x}").is_err() {
            return HashMap::new();
        }
    }

    let mut command = Command::new("/usr/bin/atos");
    command
        .arg("-l")
        .arg(format!("{ATOS_LOAD_ADDRESS:x}"))
        .arg("-o")
        .arg(binary)
        .arg("-f")
        .arg(addresses_file.path());
    if let Some(arch) = arch.as_deref() {
        command.arg("-arch").arg(arch);
    }
    let (success, stdout, _) = match command_output_allow_failure(&mut command) {
        Ok(output) => output,
        Err(_) => return HashMap::new(),
    };
    if !success {
        return HashMap::new();
    }

    let mut result = HashMap::new();
    for ((offset, input_address), symbol) in offsets.iter().zip(input_addresses).zip(stdout.lines())
    {
        let trimmed = symbol.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("0x")
            || trimmed.eq_ignore_ascii_case(&format!("{input_address:x}"))
        {
            continue;
        }
        result.insert(*offset, format_symbol(trimmed));
    }
    result
}

fn arch_for_binary(binary: &Path, requested_arch: Option<&str>) -> Option<String> {
    let requested = requested_arch
        .map(str::trim)
        .filter(|arch| !arch.is_empty());
    let mut command = Command::new("/usr/bin/lipo");
    command.arg("-archs").arg(binary);
    let (success, stdout, _) = command_output_allow_failure(&mut command).ok()?;
    if !success {
        return requested.map(ToOwned::to_owned);
    }
    let archs = stdout
        .split_whitespace()
        .map(str::trim)
        .filter(|arch| !arch.is_empty())
        .collect::<Vec<_>>();
    if let Some(requested) = requested {
        if archs.contains(&requested) {
            return Some(requested.to_owned());
        }
        if requested == "arm64e" && archs.contains(&"arm64") {
            return Some("arm64".to_owned());
        }
    }
    archs.first().map(|arch| (*arch).to_owned())
}

fn fallback_symbol(image: &SymbolicationImage, offset: u64) -> String {
    let name = image_library_name(image);
    format!("{name}+0x{offset:x}")
}

fn image_library_name(image: &SymbolicationImage) -> String {
    cleaned_up_path(&image.path)
        .file_name()
        .unwrap_or_else(|| OsStr::new(&image.path))
        .to_string_lossy()
        .into_owned()
}

fn is_user_image(image: &SymbolicationImage) -> bool {
    image.path.contains(".app/") && !image.path.contains("/Xcode.app/") && !is_hidden_image(image)
}

fn is_hidden_image(image: &SymbolicationImage) -> bool {
    image_library_name(image) == "libOrbiTrace.dylib"
}

fn cleaned_up_path(path: &str) -> PathBuf {
    if path.contains(".app/") && !path.contains("/Xcode.app/") {
        let mut keep = false;
        let mut cleaned = PathBuf::new();
        for component in Path::new(path).components() {
            let text = component.as_os_str().to_string_lossy();
            if keep {
                cleaned.push(component.as_os_str());
            } else if text.ends_with(".app") {
                keep = true;
            }
        }
        if cleaned.as_os_str().is_empty() {
            PathBuf::from(path)
        } else {
            cleaned
        }
    } else if let Some(index) = path.find("/RuntimeRoot/") {
        PathBuf::from(&path[index + "/RuntimeRoot/".len()..])
    } else {
        PathBuf::from(path)
    }
}

fn format_symbol(symbol: &str) -> String {
    let mut formatted = symbol.trim().to_owned();
    formatted = strip_source_line_numbers(&formatted);
    formatted = strip_trailing_plus_offset(&formatted);
    formatted = strip_trailing_empty_parens(&formatted);
    formatted = strip_trailing_source_file(&formatted);
    formatted = strip_in_binary_marker(&formatted);
    formatted = strip_swift_private_prefix(&formatted);
    formatted.trim().to_owned()
}

fn strip_source_line_numbers(symbol: &str) -> String {
    let mut result = String::with_capacity(symbol.len());
    let mut cursor = 0;
    while let Some(relative_end) = symbol[cursor..].find(')') {
        let end = cursor + relative_end;
        if let Some(colon) = symbol[cursor..end].rfind(':') {
            let colon = cursor + colon;
            if symbol[colon + 1..end]
                .chars()
                .all(|character| character.is_ascii_digit())
            {
                result.push_str(&symbol[cursor..colon]);
                result.push(')');
                cursor = end + 1;
                continue;
            }
        }
        result.push_str(&symbol[cursor..=end]);
        cursor = end + 1;
    }
    result.push_str(&symbol[cursor..]);
    result
}

fn strip_trailing_plus_offset(symbol: &str) -> String {
    let trimmed = symbol.trim_end();
    if let Some(index) = trimmed.rfind(" + ")
        && trimmed[index + 3..]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return trimmed[..index].to_owned();
    }
    trimmed.to_owned()
}

fn strip_trailing_empty_parens(symbol: &str) -> String {
    symbol
        .trim_end()
        .strip_suffix(" ()")
        .unwrap_or(symbol)
        .to_owned()
}

fn strip_trailing_source_file(symbol: &str) -> String {
    let trimmed = symbol.trim_end();
    let Some(open) = trimmed.rfind(" (") else {
        return trimmed.to_owned();
    };
    if !trimmed.ends_with(')') {
        return trimmed.to_owned();
    }
    let body = &trimmed[open + 2..trimmed.len() - 1];
    if body.contains('.') && !body.contains(' ') {
        return trimmed[..open].to_owned();
    }
    trimmed.to_owned()
}

fn strip_in_binary_marker(symbol: &str) -> String {
    let mut result = symbol.to_owned();
    while let Some(start) = result.find(" (in ") {
        let Some(relative_end) = result[start..].find(')') else {
            break;
        };
        let end = start + relative_end + 1;
        result.replace_range(start..end, "");
    }
    result
}

fn strip_swift_private_prefix(symbol: &str) -> String {
    let Some(rest) = symbol.strip_prefix("__") else {
        return symbol.to_owned();
    };
    let digits = rest
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digits == 0 {
        return symbol.to_owned();
    }
    let after_digits = &rest[digits..];
    if let Some(stripped) = after_digits
        .strip_prefix('+')
        .or_else(|| after_digits.strip_prefix('-'))
    {
        return stripped.to_owned();
    }
    symbol.to_owned()
}

pub(super) fn render_stack(stack: &[String], symbols: Option<&TraceSymbolication>) -> String {
    render_stack_detail(stack, symbols).display
}

pub(super) fn render_stack_detail(
    stack: &[String],
    symbols: Option<&TraceSymbolication>,
) -> RenderedStack {
    if let Some(symbols) = symbols {
        render_symbolicated_stack(stack, symbols)
    } else {
        RenderedStack {
            display: stack.join(" <- "),
            has_user_code: !stack.is_empty(),
        }
    }
}

fn render_symbolicated_stack(stack: &[String], symbols: &TraceSymbolication) -> RenderedStack {
    let frames = stack
        .iter()
        .map(|raw_frame| symbols.frame_for_address(raw_frame))
        .filter(|frame| !frame.is_hidden)
        .collect::<Vec<_>>();
    let Some(first_user_index) = frames.iter().position(|frame| frame.is_user_code) else {
        return RenderedStack {
            display: render_system_only_stack(stack, symbols, &frames),
            has_user_code: false,
        };
    };
    let last_user_index = frames
        .iter()
        .rposition(|frame| frame.is_user_code)
        .unwrap_or(first_user_index);

    let mut rendered = Vec::new();
    if first_user_index > 0
        && let Some(display) = collapsed_system_frame(&frames[..first_user_index], false)
    {
        push_distinct_frame(&mut rendered, display);
    }

    let mut index = first_user_index;
    let mut collapsed_inner_system_runs = 0usize;
    while index <= last_user_index {
        let frame = &frames[index];
        if frame.is_user_code {
            push_distinct_frame(&mut rendered, frame.display.clone());
            index += 1;
            continue;
        }

        let run_start = index;
        while index <= last_user_index && !frames[index].is_user_code {
            index += 1;
        }
        if collapsed_inner_system_runs < 2
            && let Some(display) = collapsed_system_frame(&frames[run_start..index], false)
        {
            push_distinct_frame(&mut rendered, display);
            collapsed_inner_system_runs += 1;
        }
    }

    RenderedStack {
        display: rendered.join(" <- "),
        has_user_code: true,
    }
}

fn render_system_only_stack(
    stack: &[String],
    symbols: &TraceSymbolication,
    frames: &[RenderedFrame],
) -> String {
    if frames.is_empty() {
        stack
            .iter()
            .map(|address| symbols.display_address(address))
            .collect::<Vec<_>>()
            .join(" <- ")
    } else {
        collapsed_system_frame(frames, true).unwrap_or_else(|| "<system>".to_owned())
    }
}

fn collapsed_system_frame(frames: &[RenderedFrame], allow_noisy_fallback: bool) -> Option<String> {
    let frame = frames
        .iter()
        .find(|frame| is_meaningful_system_frame(&frame.display))
        .or_else(|| allow_noisy_fallback.then(|| frames.first()).flatten())?;
    let mut display = frame.display.clone();
    if frames.len() > 1 {
        display.push_str(" (+ collapsed system calls)");
    }
    Some(display)
}

fn push_distinct_frame(frames: &mut Vec<String>, display: String) {
    if frames.last().is_some_and(|last| last == &display) {
        return;
    }
    frames.push(display);
}

fn is_meaningful_system_frame(display: &str) -> bool {
    let name = display.trim();
    if name.starts_with("0x") {
        return false;
    }
    ![
        "libswiftCore.dylib+0x",
        "SwiftUI+0x",
        "SwiftUICore+0x",
        "CoreFoundation+0x",
        "Foundation+0x",
        "libobjc.A.dylib+0x",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

fn is_hidden_symbol(display: &str) -> bool {
    let name = display.trim();
    name.starts_with("thunk for @escaping")
        || name.starts_with("thunk for @callee")
        || name.starts_with("partial apply for")
        || name.starts_with("implicit closure #")
}

pub(super) fn resolve_dsym_dir(cwd: &Path, dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        cwd.join(dir)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        SymbolicatedAddress, SymbolicationImage, TraceSymbolication, format_symbol,
        image_for_address, parse_address, render_stack, sorted_images,
    };

    #[test]
    fn maps_addresses_to_loaded_image_ranges() {
        let images = sorted_images(&[
            SymbolicationImage {
                path: "/A".to_owned(),
                uuid: None,
                load_address: 0x1000,
                size: Some(0x100),
            },
            SymbolicationImage {
                path: "/B".to_owned(),
                uuid: None,
                load_address: 0x2000,
                size: Some(0x200),
            },
        ]);

        assert_eq!(image_for_address(0x1080, &images), Some((0, 0x80)));
        assert_eq!(image_for_address(0x2100, &images), Some((1, 0x100)));
        assert_eq!(image_for_address(0x2200, &images), None);
    }

    #[test]
    fn formats_atos_symbols_for_trace_output() {
        assert_eq!(
            format_symbol("static AppDelegate.$main() (in emergeTest) (AppDelegate.swift:10)"),
            "static AppDelegate.$main()"
        );
        assert_eq!(format_symbol("_dyld_start (in dyld) + 0"), "_dyld_start");
        assert_eq!(
            format_symbol("__12+closure #1 in CPUHotLoop.run()"),
            "closure #1 in CPUHotLoop.run()"
        );
    }

    #[test]
    fn parses_hex_addresses() {
        assert_eq!(parse_address("0x10"), Some(16));
        assert_eq!(parse_address("10"), Some(16));
        assert_eq!(parse_address("not-hex"), None);
    }

    #[test]
    fn render_stack_collapses_system_frames_and_hides_trace_runtime() {
        let symbols = TraceSymbolication {
            symbols: HashMap::from([
                (
                    0x10,
                    SymbolicatedAddress {
                        display: "orbi_trace_malloc".to_owned(),
                        is_missing: false,
                        is_user_code: false,
                        is_hidden: true,
                    },
                ),
                (
                    0x20,
                    SymbolicatedAddress {
                        display: "malloc".to_owned(),
                        is_missing: false,
                        is_user_code: false,
                        is_hidden: false,
                    },
                ),
                (
                    0x30,
                    SymbolicatedAddress {
                        display: "swift_allocObject".to_owned(),
                        is_missing: false,
                        is_user_code: false,
                        is_hidden: false,
                    },
                ),
                (
                    0x40,
                    SymbolicatedAddress {
                        display: "MemoryWorkload.churn(rounds:blockSize)".to_owned(),
                        is_missing: false,
                        is_user_code: true,
                        is_hidden: false,
                    },
                ),
                (
                    0x50,
                    SymbolicatedAddress {
                        display: "SwiftUI.body.getter".to_owned(),
                        is_missing: false,
                        is_user_code: false,
                        is_hidden: false,
                    },
                ),
            ]),
            total_unique_addresses: 5,
            symbolicated_addresses: 5,
        };

        let rendered = render_stack(
            &[
                "0x10".to_owned(),
                "0x20".to_owned(),
                "0x30".to_owned(),
                "0x40".to_owned(),
                "0x50".to_owned(),
            ],
            Some(&symbols),
        );

        assert_eq!(
            rendered,
            "malloc (+ collapsed system calls) <- MemoryWorkload.churn(rounds:blockSize)"
        );
    }

    #[test]
    fn render_stack_drops_noisy_system_fallbacks_around_user_code() {
        let symbols = TraceSymbolication {
            symbols: HashMap::from([
                (
                    0x10,
                    SymbolicatedAddress {
                        display: "libswiftCore.dylib+0x13a0d0".to_owned(),
                        is_missing: true,
                        is_user_code: false,
                        is_hidden: false,
                    },
                ),
                (
                    0x20,
                    SymbolicatedAddress {
                        display: "CPUHotLoop.run(iterations)".to_owned(),
                        is_missing: false,
                        is_user_code: true,
                        is_hidden: false,
                    },
                ),
            ]),
            total_unique_addresses: 2,
            symbolicated_addresses: 1,
        };

        let rendered = render_stack(
            &["0x10".to_owned(), "0x20".to_owned(), "0x30".to_owned()],
            Some(&symbols),
        );

        assert_eq!(rendered, "CPUHotLoop.run(iterations)");
    }
}
