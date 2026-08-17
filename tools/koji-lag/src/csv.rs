// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writing CSV, shared by everything here that emits it.
//!
//! Small on purpose. A dependency would do this too, but the whole of it is
//! quoting a field that would otherwise break a row, and getting that wrong
//! is the only real hazard: a CSV that parses *wrongly* is worse than one
//! that fails, because nothing downstream notices.

/// A field, quoted only when it has to be.
pub fn quote(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// One line, fields quoted as needed.
pub fn row<S: AsRef<str>>(fields: &[S]) -> String {
    fields
        .iter()
        .map(|f| quote(f.as_ref()))
        .collect::<Vec<_>>()
        .join(",")
}

/// A whole file: a header line, then a line per row.
pub fn table<S: AsRef<str>>(header: &[&str], rows: &[Vec<S>]) -> String {
    let mut out = row(header);
    out.push('\n');
    for r in rows {
        out.push_str(&row(r));
        out.push('\n');
    }
    out
}

/// A duration in seconds, or an empty field when absent.
///
/// Plain seconds, never "2.6m": that form is for a person reading a table,
/// and a column mixing minutes and hours cannot be summed.
///
/// To the millisecond. Koji's timestamps carry microseconds and a build
/// queue is not measured that finely, so the extra digits are noise in a
/// file people read — `53.94598317146301` says nothing `53.946` does not.
/// Trailing zeros go, and so does negative zero, which is what summing an
/// empty set of delays produces and which reads as a mistake.
pub fn secs(value: Option<f64>) -> String {
    value
        .map(|v| {
            let v = if v == 0.0 { 0.0 } else { v };
            let text = format!("{v:.3}");
            match text.contains('.') {
                true => text.trim_end_matches('0').trim_end_matches('.').to_string(),
                false => text,
            }
        })
        .unwrap_or_default()
}

/// A UTC date for a period bound, or an empty field.
pub fn date(ts: Option<f64>) -> String {
    ts.and_then(|ts| chrono::DateTime::from_timestamp(ts as i64, 0))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_that_would_break_a_row_are_quoted() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote("bar, baz"), "\"bar, baz\"");
        assert_eq!(quote("say \"what\""), "\"say \"\"what\"\"\"");
        assert_eq!(quote("two\nlines"), "\"two\nlines\"");
        // A carriage return counts too: a bare CR inside a field splits the
        // row for some readers and not others.
        assert_eq!(quote("cr\rhere"), "\"cr\rhere\"");
    }

    #[test]
    fn a_table_is_a_header_and_its_rows() {
        let rows = vec![vec!["x86_64", "12"], vec!["s390x", "3"]];
        assert_eq!(table(&["arch", "n"], &rows), "arch,n\nx86_64,12\ns390x,3\n");
    }

    #[test]
    fn seconds_stay_seconds_and_absent_stays_empty() {
        // Unformatted, because a column mixing "2.6m" and "1.1h" cannot be
        // summed by whoever receives it.
        assert_eq!(secs(Some(156.5)), "156.5");
        assert_eq!(secs(None), "");
        // Milliseconds are as fine as this gets, and a whole number stays
        // whole rather than gaining ".000".
        assert_eq!(secs(Some(53.94598317146301)), "53.946");
        assert_eq!(secs(Some(100.0)), "100");
        // Summing no delays gives negative zero, which reads as an error.
        assert_eq!(secs(Some(-0.0)), "0");
        assert_eq!(date(Some(1783036800.0)), "2026-07-03");
        assert_eq!(date(None), "");
    }
}
