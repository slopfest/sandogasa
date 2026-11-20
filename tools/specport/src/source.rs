// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading a new source tree's build files, from a directory or a
//! tarball (through `tar`, without unpacking it).

use std::path::Path;
use std::process::Command;

use crate::meson_build::MesonFile;

/// For each list of names in `wanted`, the text of the first one at the
/// top of the tree in `path`, a directory or a tarball, or `None` when
/// the tree has none of them. A tarball is listed once and the files
/// extracted together, so it is decompressed twice at most.
pub fn read_top(path: &Path, wanted: &[&[&str]]) -> Result<Vec<Option<String>>, String> {
    if path.is_dir() {
        return wanted
            .iter()
            .map(|names| {
                for name in *names {
                    let file = path.join(name);
                    if file.is_file() {
                        return std::fs::read_to_string(&file)
                            .map(Some)
                            .map_err(|e| format!("{}: {e}", file.display()));
                    }
                }
                Ok(None)
            })
            .collect();
    }
    let tarball = path.display().to_string();
    let listing = tar(&["-tf", &tarball])?;
    let entries: Vec<&str> = listing.lines().filter(|e| !e.is_empty()).collect();
    let top = top_dir(&entries);
    let chosen: Vec<Option<&str>> = wanted
        .iter()
        .map(|names| {
            names.iter().find_map(|name| {
                entries.iter().copied().find(|e| {
                    let e = e.trim_start_matches("./");
                    match &top {
                        Some(t) => e == format!("{t}/{name}"),
                        None => e == *name,
                    }
                })
            })
        })
        .collect();
    let to_extract: Vec<&str> = chosen.iter().flatten().copied().collect();
    if to_extract.is_empty() {
        return Ok(vec![None; wanted.len()]);
    }
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut args = vec!["-xf", &tarball, "-C"];
    let into = dir.path().display().to_string();
    args.push(&into);
    args.extend(&to_extract);
    tar(&args)?;
    chosen
        .iter()
        .map(|c| match c {
            Some(entry) => std::fs::read_to_string(dir.path().join(entry))
                .map(Some)
                .map_err(|e| format!("{entry}: {e}")),
            None => Ok(None),
        })
        .collect()
}

/// The tree in `path` (a directory or a tarball): every `meson.build`
/// in it, by its path from the top, outside `subprojects/` (bundled
/// projects, not the package's own build), and the names at its top, for
/// checking what the spec's `%doc` lines name.
pub fn read_tree(path: &Path) -> Result<(Vec<MesonFile>, Vec<String>), String> {
    if path.is_dir() {
        let mut files = Vec::new();
        walk(path, path, &mut files)?;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut top: Vec<String> = std::fs::read_dir(path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        top.sort();
        return Ok((files, top));
    }
    let tarball = path.display().to_string();
    let listing = tar(&["-tf", &tarball])?;
    let entries: Vec<&str> = listing.lines().filter(|e| !e.is_empty()).collect();
    let top = top_dir(&entries);
    // Each entry's path inside the tree.
    let inside = |e: &str| -> Option<String> {
        let e = e.trim_start_matches("./");
        match &top {
            Some(t) => e.strip_prefix(&format!("{t}/")).map(str::to_string),
            None => Some(e.to_string()),
        }
    };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| inside(e))
        .filter_map(|p| p.split('/').next().map(str::to_string))
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    let builds: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|e| {
            inside(e).is_some_and(|p| {
                (p == "meson.build" || p.ends_with("/meson.build"))
                    && !p.starts_with("subprojects/")
            })
        })
        .collect();
    if builds.is_empty() {
        return Ok((Vec::new(), names));
    }
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let into = dir.path().display().to_string();
    let mut args = vec!["-xf", tarball.as_str(), "-C", into.as_str()];
    args.extend(&builds);
    tar(&args)?;
    let mut files = builds
        .iter()
        .map(|e| {
            let text =
                std::fs::read_to_string(dir.path().join(e)).map_err(|err| format!("{e}: {err}"))?;
            Ok(MesonFile {
                path: inside(e).unwrap_or_default(),
                text,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((files, names))
}

/// Every `meson.build` under `dir`, by its path from `root`, outside
/// `subprojects/` and hidden directories, symlinks not followed; one that
/// cannot be read is warned about and skipped.
fn walk(root: &Path, dir: &Path, out: &mut Vec<MesonFile>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        // Not following symlinks: one pointing back up would loop.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if name.starts_with('.') || (dir == root && name == "subprojects") {
                continue;
            }
            walk(root, &path, out)?;
        } else if kind.is_file() && name == "meson.build" {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.display().to_string())
                .unwrap_or(name);
            // One unreadable file leaves the rest to read.
            match std::fs::read_to_string(&path) {
                Ok(text) => out.push(MesonFile { path: rel, text }),
                Err(e) => eprintln!("warning: skipping {}: {e}", path.display()),
            }
        }
    }
    Ok(())
}

/// The one directory every entry of a tarball is in, if there is one;
/// otherwise its files are at the archive's root.
fn top_dir(entries: &[&str]) -> Option<String> {
    let mut top: Option<&str> = None;
    for e in entries {
        let e = e.trim_start_matches("./");
        if e.is_empty() {
            continue;
        }
        let (first, _) = e.split_once('/')?;
        match top {
            None => top = Some(first),
            Some(t) if t == first => {}
            Some(_) => return None,
        }
    }
    top.map(str::to_string)
}

fn tar(args: &[&str]) -> Result<String, String> {
    let out = Command::new("tar")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tar {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const WANTED: &[&[&str]] = &[&["meson.build"], &["meson.options", "meson_options.txt"]];

    fn tarball(dir: &Path, name: &str, from: &Path, what: &str) -> std::path::PathBuf {
        let out = dir.join(name);
        let ok = Command::new("tar")
            .args(["-cf", &out.display().to_string(), "-C"])
            .arg(from)
            .arg(what)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        out
    }

    #[test]
    fn a_top_level_file_of_a_directory_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("meson.options"), "new\n").unwrap();
        let read = read_top(dir.path(), WANTED).unwrap();
        assert_eq!(read, [None, Some("new\n".to_string())]);
    }

    #[test]
    fn a_top_level_file_of_a_tarball_is_read_not_one_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("pkg-1.0");
        std::fs::create_dir_all(tree.join("subprojects/x")).unwrap();
        std::fs::write(tree.join("subprojects/x/meson_options.txt"), "sub\n").unwrap();
        std::fs::write(tree.join("subprojects/x/meson.build"), "sub\n").unwrap();
        std::fs::write(tree.join("meson_options.txt"), "top\n").unwrap();
        // One directory at the top.
        let t = tarball(dir.path(), "a.tar", dir.path(), "pkg-1.0");
        let read = read_top(&t, WANTED).unwrap();
        assert_eq!(read, [None, Some("top\n".to_string())]);
        // Files at the archive's root, `./` before each.
        let t = tarball(dir.path(), "b.tar", &tree, ".");
        let read = read_top(&t, WANTED).unwrap();
        assert_eq!(read, [None, Some("top\n".to_string())]);
        assert!(read_top(&dir.path().join("missing.tar"), WANTED).is_err());
    }

    #[test]
    fn every_meson_build_of_a_tree_is_read_but_subprojects() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("pkg-1.0");
        std::fs::create_dir_all(tree.join("tests")).unwrap();
        std::fs::create_dir_all(tree.join("subprojects/x")).unwrap();
        std::fs::write(tree.join("meson.build"), "top\n").unwrap();
        std::fs::write(tree.join("tests/meson.build"), "tests\n").unwrap();
        std::fs::write(tree.join("subprojects/x/meson.build"), "sub\n").unwrap();
        std::fs::write(tree.join("README.rst"), "").unwrap();
        // A link back up is not followed.
        std::os::unix::fs::symlink(".", tree.join("tests/self")).unwrap();
        let paths = |files: &[MesonFile]| files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();
        let (files, top) = read_tree(&tree).unwrap();
        assert_eq!(paths(&files), ["meson.build", "tests/meson.build"]);
        assert_eq!(files[1].text, "tests\n");
        assert_eq!(top, ["README.rst", "meson.build", "subprojects", "tests"]);
        std::fs::remove_file(tree.join("tests/self")).unwrap();
        // The same from a tarball, one directory at its top.
        let t = tarball(dir.path(), "a.tar", dir.path(), "pkg-1.0");
        let (files, top) = read_tree(&t).unwrap();
        assert_eq!(paths(&files), ["meson.build", "tests/meson.build"]);
        assert_eq!(top, ["README.rst", "meson.build", "subprojects", "tests"]);
    }

    #[test]
    fn the_top_directory_is_the_one_every_entry_shares() {
        assert_eq!(top_dir(&["p/", "p/a", "p/b/c"]).as_deref(), Some("p"));
        assert_eq!(top_dir(&["./a", "./b/c"]), None);
        assert_eq!(top_dir(&["p/a", "q/b"]), None);
    }
}
