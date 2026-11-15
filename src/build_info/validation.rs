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
    if day == 0 || day > days || hour > 23 || minute > 59 || second > 60 {
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
