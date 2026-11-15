// Shared by the build script and the compile-time metadata consumer. No dependencies.
pub fn revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Strict RFC3339 calendar timestamp, including an explicit UTC offset.
pub fn commit_date(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return false;
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        let part = b.get(start..end)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        part.iter()
            .try_fold(0, |n, digit| Some(n * 10 + u32::from(digit - b'0')))
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut end = 19;
    if b.get(end) == Some(&b'.') {
        end += 1;
        let start = end;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return false;
        }
    }
    if matches!(b.get(end), Some(b'Z' | b'z')) {
        return end + 1 == b.len();
    }
    matches!(b.get(end), Some(b'+' | b'-'))
        && end + 6 == b.len()
        && b[end + 3] == b':'
        && number(end + 1, end + 3).is_some_and(|h| h <= 23)
        && number(end + 4, end + 6).is_some_and(|m| m <= 59)
}

/// Version tags are exact HEAD tags, with an optional conventional v prefix.
pub fn release_tag(value: &str) -> Option<&str> {
    let value = value.strip_prefix('v').unwrap_or(value);
    if value.len() > 128 || !value.is_ascii() {
        return None;
    }
    let core = value.split(['-', '+']).next()?;
    let parts: Vec<_> = core.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
        })
    {
        return None;
    }
    let mut tail = &value[core.len()..];
    if let Some(rest) = tail.strip_prefix('-') {
        let pre = rest.split('+').next()?;
        if !identifiers(pre, true) {
            return None;
        }
        tail = &rest[pre.len()..];
    }
    if let Some(build) = tail.strip_prefix('+') {
        if !identifiers(build, false) {
            return None;
        }
    } else if !tail.is_empty() {
        return None;
    }
    Some(value)
}
fn identifiers(value: &str, prerelease: bool) -> bool {
    value.split('.').all(|p| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !(prerelease
                && p.len() > 1
                && p.starts_with('0')
                && p.bytes().all(|b| b.is_ascii_digit()))
    })
}
pub fn display_version(
    revision: Option<&str>,
    date: Option<&str>,
    tag: Option<&str>,
) -> Option<String> {
    if let Some(tag) = tag.and_then(release_tag) {
        return Some(tag.to_owned());
    }
    let revision = revision.filter(|s| self::revision(s))?;
    let date = date.filter(|s| commit_date(s))?;
    let zone = if date.ends_with(['Z', 'z']) {
        "+0000".to_owned()
    } else {
        date[date.len() - 6..].replace(':', "")
    };
    Some(format!(
        "{}{}{}{}{}{}{}-{}",
        &date[2..4],
        &date[5..7],
        &date[8..10],
        &date[11..13],
        &date[14..16],
        &date[17..19],
        zone,
        &revision[..7].to_ascii_lowercase()
    ))
}
