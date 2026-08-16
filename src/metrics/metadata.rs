//! Parsers for device and operating-system metadata files.

use super::ParseError;

const OS_RELEASE: &str = "/etc/os-release";

/// Raspberry Pi identifiers extracted from `/proc/cpuinfo`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CpuInfoMetadata {
    /// Board revision code, when reported by the kernel.
    pub revision: Option<String>,
    /// Board serial number, when reported by the kernel.
    pub serial: Option<String>,
}

/// Selected fields from the operating system release file.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OsRelease {
    /// Human-friendly operating system name and version.
    pub pretty_name: Option<String>,
    /// Operating system name without a version.
    pub name: Option<String>,
    /// Human-friendly version string.
    pub version: Option<String>,
    /// Machine-readable version identifier.
    pub version_id: Option<String>,
    /// Machine-readable operating system identifier.
    pub id: Option<String>,
}

/// Extracts the first non-empty `Revision` and `Serial` fields from
/// `/proc/cpuinfo`.
#[must_use]
pub fn parse_cpuinfo(input: &str) -> CpuInfoMetadata {
    let mut metadata = CpuInfoMetadata::default();
    for line in input.lines() {
        let Some((raw_key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        match raw_key.trim() {
            "Revision" if metadata.revision.is_none() => {
                metadata.revision = Some(value.to_owned());
            }
            "Serial" if metadata.serial.is_none() => {
                metadata.serial = Some(value.to_owned());
            }
            _ => {}
        }
    }
    metadata
}

/// Parses selected fields from an `os-release` file without evaluating shell
/// syntax or expanding variables.
pub fn parse_os_release(input: &str) -> Result<OsRelease, ParseError> {
    let mut release = OsRelease::default();
    for (line_index, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, raw_value) = line.split_once('=').ok_or_else(|| {
            ParseError::new(
                OS_RELEASE,
                format!("line {} is not an assignment", line_index + 1),
            )
        })?;
        if !valid_key(key) {
            return Err(ParseError::new(
                OS_RELEASE,
                format!("line {} has an invalid key", line_index + 1),
            ));
        }
        let value = parse_assignment_value(raw_value, line_index + 1)?;
        let destination = match key {
            "PRETTY_NAME" => Some(&mut release.pretty_name),
            "NAME" => Some(&mut release.name),
            "VERSION" => Some(&mut release.version),
            "VERSION_ID" => Some(&mut release.version_id),
            "ID" => Some(&mut release.id),
            _ => None,
        };
        if let Some(destination) = destination {
            *destination = Some(value);
        }
    }
    Ok(release)
}

/// Parses a device-tree model value, removing its trailing NUL terminator.
#[must_use]
pub fn parse_device_tree_model(input: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(input).ok()?;
    non_empty(text.trim_matches(['\0', ' ', '\t', '\r', '\n']))
}

/// Parses a hostname file as a single non-empty line.
#[must_use]
pub fn parse_hostname(input: &str) -> Option<String> {
    input.lines().find_map(|line| non_empty(line.trim()))
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_assignment_value(raw: &str, line_number: usize) -> Result<String, ParseError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(String::new());
    }

    let first = raw.as_bytes()[0];
    if first == b'\'' || first == b'"' {
        if raw.len() < 2 || raw.as_bytes()[raw.len() - 1] != first {
            return Err(ParseError::new(
                OS_RELEASE,
                format!("line {line_number} has an unterminated quoted value"),
            ));
        }
        let inner = &raw[1..raw.len() - 1];
        if first == b'\'' {
            return Ok(inner.to_owned());
        }
        return unescape(inner, line_number);
    }

    unescape(raw, line_number)
}

fn unescape(input: &str, line_number: usize) -> Result<String, ParseError> {
    let mut output = String::with_capacity(input.len());
    let mut characters = input.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters.next().ok_or_else(|| {
                ParseError::new(
                    OS_RELEASE,
                    format!("line {line_number} ends with an incomplete escape"),
                )
            })?;
            output.push(escaped);
        } else {
            output.push(character);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_cpuinfo, parse_device_tree_model, parse_hostname, parse_os_release, OsRelease,
    };

    const CPUINFO: &str = include_str!("../../tests/fixtures/proc_cpuinfo.txt");

    #[test]
    fn extracts_board_identifiers_from_cpuinfo() {
        let metadata = parse_cpuinfo(CPUINFO);

        assert_eq!(metadata.revision.as_deref(), Some("a02082"));
        assert_eq!(metadata.serial.as_deref(), Some("0000000000000001"));
    }

    #[test]
    fn missing_cpuinfo_fields_remain_absent() {
        let metadata = parse_cpuinfo("processor : 0\nmodel name : Example CPU\n");

        assert_eq!(metadata.revision, None);
        assert_eq!(metadata.serial, None);
    }

    #[test]
    fn parses_quoted_and_unquoted_os_release_fields() {
        let input = "\
# Example release\n\
PRETTY_NAME=\"Example Linux 1\"\n\
NAME='Example Linux'\n\
VERSION=1\\ stable\n\
VERSION_ID=\"1\"\n\
ID=example\n\
UNRELATED=value\n";

        assert_eq!(
            parse_os_release(input),
            Ok(OsRelease {
                pretty_name: Some("Example Linux 1".to_owned()),
                name: Some("Example Linux".to_owned()),
                version: Some("1 stable".to_owned()),
                version_id: Some("1".to_owned()),
                id: Some("example".to_owned()),
            })
        );
    }

    #[test]
    fn later_os_release_assignments_take_precedence() {
        let release = parse_os_release("NAME=First\nNAME=Second\n")
            .expect("duplicate assignments should use the last value");

        assert_eq!(release.name.as_deref(), Some("Second"));
    }

    #[test]
    fn rejects_malformed_os_release_lines() {
        assert!(parse_os_release("NAME\n").is_err());
        assert!(parse_os_release("lowercase=value\n").is_err());
        assert!(parse_os_release("NAME=\"unterminated\n").is_err());
        assert!(parse_os_release("NAME=value\\\n").is_err());
    }

    #[test]
    fn parses_device_tree_model_and_hostname() {
        assert_eq!(
            parse_device_tree_model(b"Example ARM Board\0"),
            Some("Example ARM Board".to_owned())
        );
        assert_eq!(parse_device_tree_model(b"\0"), None);
        assert_eq!(parse_device_tree_model(&[0xff, 0]), None);
        assert_eq!(
            parse_hostname("\nexample-node\n"),
            Some("example-node".to_owned())
        );
        assert_eq!(parse_hostname(" \n"), None);
    }
}
