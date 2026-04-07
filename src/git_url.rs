/// Normalize a Git repository URL into a canonical hosted form.
///
/// This attempts to parse common Git URL forms (SCP-style and standard URLs)
/// for known hosts (GitHub, GitLab, Bitbucket, Sourcehut) and reconstructs a
/// normalized repository reference (including a `.git` suffix and optional
/// committish fragment when applicable). If the input is not recognized or
/// cannot be normalized, the original input is returned unchanged as an owned
/// `String`.
///
/// # Returns
///
/// A `String` containing the normalized repository reference, or the original
/// input when no normalization is performed.
///
/// # Examples
///
/// ```
/// let a = normalize("git@github.com:owner/repo#main");
/// assert_eq!(a, "git@github.com:owner/repo.git#main");
///
/// let b = normalize("https://gitlab.com/group/project");
/// assert_eq!(b, "git+https://gitlab.com/group/project.git");
///
/// // unknown hosts are returned unchanged
/// let c = normalize("https://example.com/some/repo");
/// assert_eq!(c, "https://example.com/some/repo");
/// ```
pub fn normalize(input: &str) -> String {
    try_normalize(input).unwrap_or_else(|| input.to_owned())
}

/// Attempts to canonicalize a Git repository URL into the module's normalized hosted-git-info form.
///
/// Supports SCP-style SSH inputs like `git@host:owner/repo[#fragment]` and standard URLs
/// `scheme://[user@]host/path[?query][#fragment]`. Query strings are removed and an existing
/// fragment (`#...`) is treated as the committish to preserve. Only recognized hosts are normalized;
/// unrecognized hosts, unsupported schemes, or repository-path shapes that do not match host-specific
/// rules cause the function to return `None`.
///
/// # Returns
///
/// `Some(String)` containing the normalized repository string when parsing and normalization succeed,
/// `None` otherwise.
fn try_normalize(input: &str) -> Option<String> {
    // SCP-style SSH: git@host:path[#fragment]
    if let Some(rest) = input.strip_prefix("git@") {
        let (domain, path) = rest.split_once(':')?;
        let host = Host::from_domain(domain)?;
        let (path, frag) = split_or(path, '#');
        let (user, project, committish) = host.extract(path, frag)?;
        return host.render("scp", user, project, committish);
    }

    // Standard URL: scheme://[user@]host/path[?query][#fragment]
    let (no_frag, frag) = split_or(input, '#');
    let (no_query, _) = split_or(no_frag, '?');
    let (scheme, rest) = no_query.split_once("://")?;
    let (authority, path) = split_or(rest, '/');
    let host_str = authority.split_once('@').map_or(authority, |(_, h)| h);
    let host = Host::from_domain(host_str)?;
    let (user, project, committish) = host.extract(path, frag)?;
    host.render(scheme, user, project, committish)
}

/// Splits a string once on the first occurrence of a character, returning the parts before and after it.
///
/// If the character is not found, the original string is returned as the left part and the right part is empty.
fn split_or(s: &str, pat: char) -> (&str, &str) {
    s.split_once(pat).unwrap_or((s, ""))
}

use strum::{EnumString, IntoStaticStr};

#[derive(Clone, Copy, PartialEq, EnumString, IntoStaticStr)]
enum Host {
    #[strum(serialize = "github.com")]
    GitHub,
    #[strum(serialize = "gitlab.com")]
    GitLab,
    #[strum(serialize = "bitbucket.org")]
    Bitbucket,
    #[strum(serialize = "git.sr.ht")]
    Sourcehut,
}

impl Host {
    /// Parse a repository host domain into a `Host`, accepting an optional leading `www.`.
    ///
    /// Removes a leading `"www."` from `domain` if present and attempts to convert the resulting domain string into a `Host` variant.
    fn from_domain(domain: &str) -> Option<Self> {
        domain.strip_prefix("www.").unwrap_or(domain).parse().ok()
    }

    /// Canonical domain string for the host.
    fn domain(self) -> &'static str {
        self.into()
    }

    /// Render a canonical repository URL for this host using the given scheme and components.
    ///
    /// Returns `Some(String)` containing the normalized URL when the host and scheme combination is supported;
    /// returns `None` for unsupported combinations. The `committish` fragment is appended as `#committish` only when non-empty.
    fn render(self, scheme: &str, user: &str, project: &str, committish: &str) -> Option<String> {
        let domain = self.domain();
        let hash = if committish.is_empty() { "" } else { "#" };
        Some(match scheme {
            "scp" => format!("git@{domain}:{user}/{project}.git{hash}{committish}"),
            "https" | "git+https" => match self {
                Self::Sourcehut => {
                    format!("https://{domain}/{user}/{project}.git{hash}{committish}")
                }
                _ => format!("git+https://{domain}/{user}/{project}.git{hash}{committish}"),
            },
            "git+ssh" | "ssh" => {
                format!("git+ssh://git@{domain}/{user}/{project}.git{hash}{committish}")
            }
            "git" if matches!(self, Self::GitHub) => {
                format!("git://{domain}/{user}/{project}.git{hash}{committish}")
            }
            _ => return None,
        })
    }

    /// Extracts the repository owner (user/namespace), project name, and committish from a host-specific repository path.
    ///
    /// Returns `Some((user, project, committish))` when `path` matches the host's expected repository layout and both
    /// `user` and `project` are non-empty; returns `None` for non-repository shapes or host-specific rejected patterns
    /// (e.g., GitLab `/-/`, Bitbucket `/get`, Sourcehut `/archive`).
    fn extract<'a>(self, path: &'a str, fragment: &'a str) -> Option<(&'a str, &'a str, &'a str)> {
        match self {
            Self::GitHub => {
                let (user, rest) = path.split_once('/')?;
                let mut parts = rest.splitn(3, '/');
                let (project_raw, committish) = match (parts.next(), parts.next(), parts.next()) {
                    (Some(p), None, _) => (p, fragment),
                    (Some(p), Some("tree"), None) => (p, ""),
                    (Some(p), Some("tree"), Some(c)) => (p, c),
                    _ => return None,
                };
                nonempty(user, project_raw.trim_end_matches(".git"), committish)
            }
            Self::GitLab => {
                if path.contains("/-/") || path.contains("/archive.tar.gz") {
                    return None;
                }
                let (user, project) = path.rsplit_once('/')?;
                nonempty(user, project.trim_end_matches(".git"), fragment)
            }
            Self::Bitbucket => {
                let (user, rest) = path.split_once('/')?;
                let (project, aux) = rest.split_once('/').unwrap_or((rest, ""));
                if aux == "get" || aux.starts_with("get/") {
                    return None;
                }
                nonempty(user, project.trim_end_matches(".git"), fragment)
            }
            Self::Sourcehut => {
                let (user, rest) = path.split_once('/')?;
                let (project_raw, aux) = rest.split_once('/').unwrap_or((rest, ""));
                if aux == "archive" || aux.starts_with("archive/") {
                    return None;
                }
                nonempty(user, project_raw.trim_end_matches(".git"), fragment)
            }
        }
    }
}

/// Ensures the repository `user` and `project` are present and returns them with `committish`.
///
/// Returns `Some((user, project, committish))` if both `user` and `project` are non-empty, otherwise `None`.
fn nonempty<'a>(
    user: &'a str,
    project: &'a str,
    committish: &'a str,
) -> Option<(&'a str, &'a str, &'a str)> {
    if user.is_empty() || project.is_empty() {
        None
    } else {
        Some((user, project, committish))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn github_https() {
        assert_eq!(
            normalize("https://github.com/user/repo"),
            "git+https://github.com/user/repo.git"
        );
    }

    #[test]
    fn github_https_already_normalized() {
        assert_eq!(
            normalize("git+https://github.com/user/repo.git"),
            "git+https://github.com/user/repo.git"
        );
    }

    #[test]
    fn github_https_with_committish() {
        assert_eq!(
            normalize("https://github.com/user/repo#main"),
            "git+https://github.com/user/repo.git#main"
        );
    }

    #[test]
    fn gitlab_https() {
        assert_eq!(
            normalize("https://gitlab.com/user/repo"),
            "git+https://gitlab.com/user/repo.git"
        );
    }

    #[test]
    fn bitbucket_https() {
        assert_eq!(
            normalize("https://bitbucket.org/user/repo"),
            "git+https://bitbucket.org/user/repo.git"
        );
    }

    #[test]
    fn sourcehut_https() {
        assert_eq!(
            normalize("https://git.sr.ht/~user/repo"),
            "https://git.sr.ht/~user/repo.git"
        );
    }

    #[test]
    fn github_ssh_scp() {
        assert_eq!(
            normalize("git@github.com:user/repo"),
            "git@github.com:user/repo.git"
        );
    }

    #[test]
    fn github_ssh_scp_already_has_git() {
        assert_eq!(
            normalize("git@github.com:user/repo.git"),
            "git@github.com:user/repo.git"
        );
    }

    #[test]
    fn github_git_plus_ssh() {
        assert_eq!(
            normalize("git+ssh://git@github.com/user/repo"),
            "git+ssh://git@github.com/user/repo.git"
        );
    }

    #[test]
    fn gitlab_nested_namespace() {
        assert_eq!(
            normalize("https://gitlab.com/group/subgroup/repo"),
            "git+https://gitlab.com/group/subgroup/repo.git"
        );
    }

    #[test]
    fn gitlab_ssh_scp_nested_namespace() {
        assert_eq!(
            normalize("git@gitlab.com:group/subgroup/repo"),
            "git@gitlab.com:group/subgroup/repo.git"
        );
    }

    #[test]
    fn query_string_is_stripped() {
        assert_eq!(
            normalize("https://github.com/user/repo?foo=bar"),
            "git+https://github.com/user/repo.git"
        );
    }

    #[test]
    fn unknown_host_unchanged() {
        assert_eq!(
            normalize("https://example.com/user/repo"),
            "https://example.com/user/repo"
        );
    }

    #[test]
    fn unknown_ssh_unchanged() {
        assert_eq!(
            normalize("git@example.com:user/repo"),
            "git@example.com:user/repo"
        );
    }

    #[test]
    fn github_ssh_scp_single_segment_unchanged() {
        assert_eq!(normalize("git@github.com:repo"), "git@github.com:repo");
    }

    #[test]
    fn bitbucket_download_url_unchanged() {
        assert_eq!(
            normalize("https://bitbucket.org/user/repo/get/main.zip"),
            "https://bitbucket.org/user/repo/get/main.zip"
        );
    }

    #[test]
    fn sourcehut_archive_url_unchanged() {
        assert_eq!(
            normalize("https://git.sr.ht/~user/repo/archive/main.tar.gz"),
            "https://git.sr.ht/~user/repo/archive/main.tar.gz"
        );
    }

    #[test]
    fn gitlab_ci_path_unchanged() {
        assert_eq!(
            normalize("https://gitlab.com/group/repo/-/blob/main/README.md"),
            "https://gitlab.com/group/repo/-/blob/main/README.md"
        );
    }
}