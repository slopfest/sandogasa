// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Running a registry lookup (crates.io, Hackage, PyPI) for the "latest
//! version is packaged" item: one `curl` invocation, its exit status
//! checked, its output handed to the registry's parser.

/// Run `argv` (a `curl` command line for `registry`), and parse what it
/// prints with `parse`. `Ok(None)` when the response carried no version;
/// `Err` when the command is missing or fails (network down, HTTP
/// error, unknown project).
pub fn fetch(
    argv: &[String],
    registry: &str,
    name: &str,
    parse: fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("running {}: {e}", argv[0]))?;
    if !out.status.success() {
        return Err(format!(
            "{registry} lookup for {name} failed ({} exit {:?})",
            argv[0],
            out.status.code()
        ));
    }
    Ok(parse(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_line(s: &str) -> Option<String> {
        s.lines().next().map(str::to_string)
    }

    #[test]
    fn a_lookup_reads_the_output_or_reports_the_failure() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ok = argv(&["sh", "-c", "echo 1.2"]);
        assert_eq!(fetch(&ok, "PyPI", "x", first_line), Ok(Some("1.2".into())));
        let err = fetch(&argv(&["false"]), "PyPI", "x", first_line).unwrap_err();
        assert_eq!(err, "PyPI lookup for x failed (false exit Some(1))");
        let err = fetch(&argv(&["/nonexistent/curl"]), "PyPI", "x", first_line).unwrap_err();
        assert!(err.starts_with("running /nonexistent/curl"), "{err}");
    }
}
