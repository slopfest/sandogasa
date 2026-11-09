// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The SIGs' lookaside cache, where CBS fetches the files a package's
//! `sources` file names. It is not CentOS Stream's: a SIG build of a
//! package fetches from `git.centos.org/sources/<package>/<file>/sha512/
//! <sum>/<file>`, so each file has to be uploaded there, with
//! `centos-lookaside-upload-sig`, before the package's first build.

/// The SIGs' lookaside cache.
pub const SIG_LOOKASIDE: &str = "https://git.centos.org";

/// One file a `sources` file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    /// The hash, lower case: `sha512`.
    pub hash: String,
    pub sum: String,
}

/// The files a `sources` file names, from its
/// `SHA512 (rsync-3.5.0.tar.gz) = <sum>` lines. Lines in any other form
/// are skipped.
pub fn parse_sources(text: &str) -> Vec<Source> {
    text.lines()
        .filter_map(|line| {
            let (hash, rest) = line.trim().split_once(" (")?;
            let (name, sum) = rest.split_once(") = ")?;
            let (name, sum) = (name.trim(), sum.trim());
            (!name.is_empty() && !sum.is_empty() && !name.contains('/')).then(|| Source {
                name: name.to_string(),
                hash: hash.trim().to_ascii_lowercase(),
                sum: sum.to_string(),
            })
        })
        .collect()
}

/// Where the SIG lookaside at `base` keeps `source` of `package`.
pub fn url(base: &str, package: &str, source: &Source) -> String {
    format!(
        "{}/sources/{package}/{name}/{hash}/{sum}/{name}",
        base.trim_end_matches('/'),
        name = source.name,
        hash = source.hash,
        sum = source.sum,
    )
}

/// The command that uploads `file` (a name in the current directory)
/// for `package`, as a person would type it.
pub fn upload_argv(file: &str, package: &str) -> Vec<String> {
    ["centos-lookaside-upload-sig", "-f", file, "-n", package]
        .map(String::from)
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sources_file_names_its_files_and_where_the_sig_lookaside_has_them() {
        let text = "SHA512 (rsync-patches-3.2.7.tar.gz) = e675f6\n\
                    SHA512 (rsync-3.2.7.tar.gz) = c2afba\n\
                    \n\
                    0123abcd  old-style-md5.tar.gz\n";
        let sources = parse_sources(text);
        assert_eq!(
            sources.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["rsync-patches-3.2.7.tar.gz", "rsync-3.2.7.tar.gz"]
        );
        assert_eq!(sources[1].hash, "sha512");
        // As CBS's get_sources.sh fetched it in task 6077804.
        assert_eq!(
            url(SIG_LOOKASIDE, "rsync", &sources[1]),
            "https://git.centos.org/sources/rsync/rsync-3.2.7.tar.gz/sha512/c2afba/rsync-3.2.7.tar.gz"
        );
        assert_eq!(
            upload_argv("rsync-3.2.7.tar.gz", "rsync").join(" "),
            "centos-lookaside-upload-sig -f rsync-3.2.7.tar.gz -n rsync"
        );
        assert!(parse_sources("SHA512 (../evil) = x\n").is_empty());
    }
}
