use crate::model::TaskId;
use std::path::{Path, PathBuf};
use url::Url;

pub fn suggest_filename(content_disposition: Option<&str>, url: &Url, id: TaskId) -> String {
    let header_name = content_disposition.and_then(|header| {
        parameter(header, "filename*")
            .and_then(|value| {
                value
                    .strip_prefix("UTF-8''")
                    .or_else(|| value.strip_prefix("utf-8''"))
                    .map(percent_decode)
            })
            .or_else(|| parameter(header, "filename"))
    });
    let url_name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .map(percent_decode);
    sanitize_filename(header_name.or(url_name).as_deref().unwrap_or(""), id)
}

fn parameter(header: &str, wanted: &str) -> Option<String> {
    header
        .split(';')
        .skip(1)
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(wanted))
        .map(|(_, value)| value.trim().trim_matches('"').replace("\\\"", "\""))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub fn sanitize_filename(input: &str, id: TaskId) -> String {
    let leaf = input.rsplit(['/', '\\']).next().unwrap_or("");
    let mut name: String = leaf
        .chars()
        .map(|character| {
            if character <= '\u{1f}' || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    name = name.trim().trim_end_matches([' ', '.']).to_owned();
    if name.is_empty() {
        return format!("download-{id}.bin");
    }

    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if reserved {
        name.insert(0, '_');
    }
    name
}

pub fn available_filename(dir: &Path, requested: &str) -> PathBuf {
    let requested_path = dir.join(requested);
    if !requested_path.exists() {
        return requested_path;
    }
    let stem = requested_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = requested_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    (1..)
        .map(|number| dir.join(format!("{stem} ({number}){extension}")))
        .find(|path| !path.exists())
        .expect("infinite filename candidate sequence must produce a free path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_utf8_content_disposition() {
        let url = Url::parse("https://example.test/fallback.bin").unwrap();
        assert_eq!(
            suggest_filename(
                Some("attachment; filename*=UTF-8''%E6%B8%AC%E8%A9%A6.zip"),
                &url,
                1
            ),
            "測試.zip"
        );
    }

    #[test]
    fn blocks_windows_devices_and_traversal() {
        assert_eq!(sanitize_filename("..\\CON.txt", 9), "_CON.txt");
        assert_eq!(sanitize_filename("a<b>:c?.zip", 9), "a_b__c_.zip");
    }

    #[test]
    fn falls_back_when_name_is_empty() {
        assert_eq!(sanitize_filename("...", 42), "download-42.bin");
    }
}
