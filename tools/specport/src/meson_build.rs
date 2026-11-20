// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a new source's `meson.build` files say beyond its options: the
//! dependencies they look for, whether they define tests, and what they
//! install where. Read from the text, not by running Meson, so it sees
//! calls, not their results: enough to point a person at what the spec
//! has to cover.

/// One `meson.build`, by its path in the source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MesonFile {
    /// `meson.build`, `tests/meson.build`.
    pub path: String,
    pub text: String,
}

/// Whether a `dependency()` call requires what it looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Required {
    /// `required: true`, or no `required:` at all.
    Yes,
    /// `required: false`.
    No,
    /// `required: get_option('name')`: as the feature option is set.
    Option(String),
}

/// A `dependency()` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The names it looks for, the first preferred.
    pub names: Vec<String>,
    pub required: Required,
    /// The `meson.build` calling it.
    pub file: String,
}

/// Something a `meson.build` installs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    /// The directory, with Meson's directory options as RPM macros
    /// (`%{_sysconfdir}/ras/triggers`), or the expression as written
    /// when it is not one of those.
    pub dir: String,
    /// What is installed: a file name, or the variable holding it.
    pub what: String,
    /// The `meson.build` installing it.
    pub file: String,
}

/// Dependencies Meson finds by other means than pkg-config (its own
/// lookups, CMake, config tools), which no `pkgconfig(…)` BuildRequires
/// provides.
const NOT_PKGCONFIG: &[&str] = &[
    "appleframeworks",
    "atomic",
    "blocks",
    "boost",
    "cuda",
    "curses",
    "dl",
    "gmock",
    "gtest",
    "hdf5",
    "iconv",
    "intl",
    "jni",
    "llvm",
    "mpi",
    "netcdf",
    "openmp",
    "pcap",
    "python3",
    "qt4",
    "qt5",
    "qt6",
    "sdl2",
    "threads",
    "valgrind",
    "wxwidgets",
];

/// Every `dependency()` call in `files` that pkg-config can satisfy.
pub fn dependencies(files: &[MesonFile]) -> Vec<Dependency> {
    let mut out = Vec::new();
    for f in files {
        for args in calls(&f.text, "dependency") {
            let names: Vec<String> = args
                .iter()
                .map_while(|a| quoted(a).map(str::to_string))
                .collect();
            // `dependency('')` is Meson's empty, not-found dependency.
            if names.first().is_none_or(|n| n.is_empty())
                || names.iter().any(|n| NOT_PKGCONFIG.contains(&n.as_str()))
            {
                continue;
            }
            // Found another way than pkg-config: no pkgconfig() for it.
            if keyword(&args, "method")
                .and_then(quoted)
                .is_some_and(|m| m != "pkg-config" && m != "auto")
            {
                continue;
            }
            let required = match keyword(&args, "required").map(str::trim) {
                None | Some("true") => Required::Yes,
                Some("false") => Required::No,
                Some(v) => match v
                    .strip_prefix("get_option(")
                    .and_then(|r| r.strip_suffix(')'))
                    .and_then(quoted)
                {
                    Some(o) => Required::Option(o.to_string()),
                    // A variable: whether it is required depends on the build.
                    None => Required::No,
                },
            };
            out.push(Dependency {
                names,
                required,
                file: f.path.clone(),
            });
        }
    }
    out
}

/// Whether `files` use Meson's python module, which needs the Python
/// headers and the RPM macros of `python3-devel`.
pub fn uses_python(files: &[MesonFile]) -> bool {
    files
        .iter()
        .any(|f| f.text.contains("import('python')") || f.text.contains("import(\"python\")"))
}

/// The `meson.build` files defining tests (`test()` or `benchmark()`).
pub fn test_files(files: &[MesonFile]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !calls(&f.text, "test").is_empty() || !calls(&f.text, "benchmark").is_empty())
        .map(|f| f.path.clone())
        .collect()
}

/// Meson's directory options as the RPM macros `%meson` sets them to.
const DIR_MACROS: &[(&str, &str)] = &[
    ("bindir", "%{_bindir}"),
    ("datadir", "%{_datadir}"),
    ("includedir", "%{_includedir}"),
    ("infodir", "%{_infodir}"),
    ("libdir", "%{_libdir}"),
    ("libexecdir", "%{_libexecdir}"),
    ("localedir", "%{_datadir}/locale"),
    ("localstatedir", "%{_localstatedir}"),
    ("mandir", "%{_mandir}"),
    ("prefix", "%{_prefix}"),
    ("sbindir", "%{_sbindir}"),
    ("sharedstatedir", "%{_sharedstatedir}"),
    ("sysconfdir", "%{_sysconfdir}"),
];

/// An `install_dir:` expression as a path: `get_option('sysconfdir') /
/// 'ras/triggers'` is `%{_sysconfdir}/ras/triggers`; anything else is
/// kept as written.
pub fn render_dir(expr: &str) -> String {
    let parts: Vec<String> = split_top(expr, '/')
        .iter()
        .map(|p| {
            let p = p.trim();
            if let Some(q) = quoted(p) {
                return q.to_string();
            }
            if let Some(opt) = p
                .strip_prefix("get_option(")
                .and_then(|r| r.strip_suffix(')'))
                .and_then(quoted)
                && let Some((_, m)) = DIR_MACROS.iter().find(|(o, _)| *o == opt)
            {
                return m.to_string();
            }
            p.to_string()
        })
        .collect();
    parts.join("/")
}

/// What `files` install where: `install_data`, `install_man`,
/// `install_subdir`, `install_headers`, a `configure_file` or
/// `custom_target` with an `install_dir:`, and an `executable` or
/// library with `install: true`, in Meson's default directory when it
/// names none (`install_data`'s is `%{_datadir}/<project>`). One with
/// `install: false` installs nothing and is left out.
pub fn installs(files: &[MesonFile]) -> Vec<Install> {
    let project = files
        .iter()
        .find_map(|f| calls(&f.text, "project").into_iter().next())
        .and_then(|args| args.first().and_then(|a| quoted(a).map(str::to_string)))
        .unwrap_or_else(|| "<project>".to_string());
    let mut out = Vec::new();
    for f in files {
        for name in [
            "install_data",
            "install_man",
            "install_subdir",
            "install_headers",
            "configure_file",
            "custom_target",
            "executable",
            "library",
            "shared_library",
            "static_library",
        ] {
            for args in calls(&f.text, name) {
                let install = keyword(&args, "install").map(str::trim);
                if install == Some("false") {
                    continue;
                }
                let built = matches!(
                    name,
                    "executable" | "library" | "shared_library" | "static_library"
                );
                let dir = match (name, keyword(&args, "install_dir")) {
                    (_, Some(d)) => render_dir(d),
                    _ if built && install != Some("true") => continue,
                    ("executable", None) => "%{_bindir}".to_string(),
                    (_, None) if built => "%{_libdir}".to_string(),
                    ("install_data", None) => format!("%{{_datadir}}/{project}"),
                    ("install_man", None) => "%{_mandir}".to_string(),
                    ("install_headers", None) => "%{_includedir}".to_string(),
                    _ => continue,
                };
                // configure_file names its output, the others their first
                // argument.
                let what = match name {
                    "configure_file" => keyword(&args, "output").map(|o| o.trim().to_string()),
                    _ => args.first().map(|a| a.trim().to_string()),
                };
                let what = what.unwrap_or_default();
                let what = quoted(&what).map(str::to_string).unwrap_or(what);
                out.push(Install {
                    dir,
                    what,
                    file: f.path.clone(),
                });
            }
        }
    }
    out
}

/// The argument lists of every call of `name` in `text`, each split into
/// its top-level arguments; comments and strings are skipped when
/// matching parentheses.
fn calls(text: &str, name: &str) -> Vec<Vec<String>> {
    let text = strip_comments(text);
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = text[from..].find(name) {
        let start = from + pos;
        from = start + name.len();
        let before_ok = start == 0 || {
            let c = bytes[start - 1] as char;
            !(c.is_alphanumeric() || c == '_' || c == '.')
        };
        let rest = &text[from..];
        let open = rest.len() - rest.trim_start().len();
        if !before_ok || !rest[open..].starts_with('(') {
            continue;
        }
        let body_start = from + open + 1;
        let Some(len) = balanced(&text[body_start..]) else {
            continue;
        };
        out.push(split_top(&text[body_start..body_start + len], ','));
        from = body_start + len;
    }
    out
}

/// `text` with `#` comments outside strings removed.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|line| {
            let mut in_str = false;
            for (i, c) in line.char_indices() {
                match c {
                    '\'' => in_str = !in_str,
                    '#' if !in_str => return &line[..i],
                    _ => {}
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The length of `text` up to the `)` closing a call already opened.
fn balanced(text: &str) -> Option<usize> {
    let mut depth = 1;
    let mut in_str: Option<char> = None;
    for (i, c) in text.char_indices() {
        match (in_str, c) {
            (Some(q), c) if c == q => in_str = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => in_str = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` split at `sep` outside strings and brackets.
fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut in_str: Option<char> = None;
    let mut cur = String::new();
    for c in text.chars() {
        match (in_str, c) {
            (Some(q), c) if c == q => in_str = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => in_str = Some(c),
            (None, '(' | '[' | '{') => depth += 1,
            (None, ')' | ']' | '}') => depth -= 1,
            (None, c) if c == sep && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// The value of keyword argument `key` among `args`.
fn keyword<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter().find_map(|a| {
        let (k, v) = a.split_once(':')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// The contents of a single-quoted (or double-quoted) string literal.
fn quoted(s: &str) -> Option<&str> {
    let s = s.trim();
    s.strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|r| r.strip_suffix('"')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> MesonFile {
        MesonFile {
            path: path.into(),
            text: text.into(),
        }
    }

    /// From rasdaemon 1.0.0's meson.build files.
    fn rasdaemon() -> Vec<MesonFile> {
        vec![
            file(
                "meson.build",
                "project('rasdaemon', 'c')\n\
                 cmocka_dep = dependency('cmocka', version: '>= 2.0', required: false)\n\
                 py = import('python').find_installation('python3')\n\
                 libtraceevent_dep = dependency('libtraceevent', required: true)\n\
                 systemd_dep = dependency('systemd', required: false)\n\
                 deps = [dependency('threads'), libtraceevent_dep]\n\
                 # dependency('commented', required: true)\n\
                 mysql_dep = dependency('mysqlclient', 'mariadb',\n    required: get_option('mysql'))\n\
                 sqlite3_dep = dependency('sqlite3', required: get_option('sqlite3'))\n\
                 libpci_dep = dependency('libpci')\n\
                 none = dependency('', required: false)\n\
                 b = dependency('boost', modules: ['system'])\n\
                 z = dependency('zstd', method: 'cmake')\n\
                 executable('rasdaemon', sources, install: true,\n    install_dir: get_option('sbindir'))\n\
                 executable('helper', 'h.c', install: true)\n\
                 executable('unit', 'u.c')\n\
                 shared_library('ras', 'r.c', install: true)\n\
                 configure_file(output: 'x.h', install: false, install_dir: get_option('includedir'))\n\
                 install_data('README.data')\n\
                 install_data('completions/ras-mc-ctl.bash',\n    install_dir: bash_compdir, rename: 'ras-mc-ctl')\n",
            ),
            file(
                "contrib/meson.build",
                "install_data('mc_event_trigger',\n    install_dir: get_option('sysconfdir') / 'ras/triggers')\n",
            ),
            file(
                "misc/meson.build",
                "configure_file(input: 'x.in', output: 'rasdaemon.service', configuration: c,\n    install_dir: systemd_unitdir)\n",
            ),
            file(
                "tests/meson.build",
                "foreach g : groups\n  test(g, unittest_exe, args: ['--group', g])\nendforeach\n",
            ),
            file("man/meson.build", "install_man('rasdaemon.8')\n"),
        ]
    }

    #[test]
    fn dependencies_are_read_with_whether_they_are_required() {
        let deps = dependencies(&rasdaemon());
        let got: Vec<(Vec<&str>, &Required)> = deps
            .iter()
            .map(|d| (d.names.iter().map(String::as_str).collect(), &d.required))
            .collect();
        assert_eq!(
            got,
            [
                (vec!["cmocka"], &Required::No),
                (vec!["libtraceevent"], &Required::Yes),
                (vec!["systemd"], &Required::No),
                (
                    vec!["mysqlclient", "mariadb"],
                    &Required::Option("mysql".into())
                ),
                (vec!["sqlite3"], &Required::Option("sqlite3".into())),
                (vec!["libpci"], &Required::Yes),
            ]
        );
        assert!(uses_python(&rasdaemon()));
        assert!(!uses_python(&[file("meson.build", "project('x')")]));
    }

    #[test]
    fn tests_are_found_in_any_meson_build() {
        assert_eq!(test_files(&rasdaemon()), ["tests/meson.build"]);
        // get_test_data() is not a test.
        assert!(test_files(&[file("meson.build", "x = get_test_data()")]).is_empty());
    }

    #[test]
    fn installs_name_what_goes_where() {
        let got: Vec<(String, String, String)> = installs(&rasdaemon())
            .into_iter()
            .map(|i| (i.dir, i.what, i.file))
            .collect();
        let row = |d: &str, w: &str, f: &str| (d.to_string(), w.to_string(), f.to_string());
        assert_eq!(
            got,
            [
                row("%{_datadir}/rasdaemon", "README.data", "meson.build"),
                row("bash_compdir", "completions/ras-mc-ctl.bash", "meson.build"),
                row("%{_sbindir}", "rasdaemon", "meson.build"),
                row("%{_bindir}", "helper", "meson.build"),
                row("%{_libdir}", "ras", "meson.build"),
                row(
                    "%{_sysconfdir}/ras/triggers",
                    "mc_event_trigger",
                    "contrib/meson.build"
                ),
                row("systemd_unitdir", "rasdaemon.service", "misc/meson.build"),
                row("%{_mandir}", "rasdaemon.8", "man/meson.build"),
            ]
        );
    }

    #[test]
    fn an_install_dir_becomes_a_path() {
        assert_eq!(
            render_dir("get_option('mandir') / 'man8'"),
            "%{_mandir}/man8"
        );
        assert_eq!(render_dir("pydir"), "pydir");
        assert_eq!(
            render_dir("get_option('unknown') / 'x'"),
            "get_option('unknown')/x"
        );
    }
}
