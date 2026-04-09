/// Normalizes a git repository URL to the format expected by npm's package.json,
/// matching the normalization performed by the `hosted-git-info` npm package.
///
/// For recognized hosts (GitHub, GitLab, Bitbucket, Sourcehut), the URL is
/// reconstructed in canonical form. All other URLs are returned unchanged.
pub fn normalize(input: &str) -> String {
    try_normalize(input).unwrap_or_else(|| input.to_owned())
}

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
    fn from_domain(domain: &str) -> Option<Self> {
        domain.strip_prefix("www.").unwrap_or(domain).parse().ok()
    }

    fn domain(self) -> &'static str {
        self.into()
    }

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
