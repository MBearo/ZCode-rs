//! Remote workspace identities (Node `packages/shared/src/remote-workspace-identity.ts`):
//! `remote:ssh:<host>:<port>:<user>:<path>`, `remote:wsl:<distro>[:<user>]:<path>`,
//! `remote:docker:<container>:<path>`. The path always starts with `/`.

/// A parsed remote identity: its kind and the remote workspace path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remote<'a> {
    pub kind: &'static str,
    pub path: &'a str,
}

/// Node `parseRemoteWorkspaceIdentity`; `None` for local or malformed identities.
pub fn parse_remote(identity: &str) -> Option<Remote<'_>> {
    let rest = identity.strip_prefix("remote:")?;
    let kind_end = rest.find(':').filter(|end| *end > 0)?;
    let (kind, segments) = match &rest[..kind_end] {
        "ssh" => ("ssh", 3),
        "wsl" => ("wsl", 1),
        "docker" => ("docker", 1),
        _ => return None,
    };
    // authority 逐段消费：路径段本身可能含 ":"，不能整体 split。
    let mut cursor = kind_end + 1;
    for _ in 0..segments {
        let next = cursor + rest[cursor..].find(':').filter(|offset| *offset > 0)?;
        cursor = next + 1;
    }
    if kind == "wsl" && !rest[cursor..].starts_with('/') {
        let user_end = cursor + rest[cursor..].find(':').filter(|offset| *offset > 0)?;
        cursor = user_end + 1;
    }
    let path = &rest[cursor..];
    path.starts_with('/').then_some(Remote { kind, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_identity_formats() {
        let parse = |s| parse_remote(s).map(|r| (r.kind, r.path));
        assert_eq!(
            parse("remote:ssh:host:22:me:/srv/app"),
            Some(("ssh", "/srv/app"))
        );
        assert_eq!(parse("remote:ssh:host:22:me:/a:b"), Some(("ssh", "/a:b")));
        assert_eq!(parse("remote:wsl:Ubuntu:/home/x"), Some(("wsl", "/home/x")));
        assert_eq!(
            parse("remote:wsl:Ubuntu:me:/home/x"),
            Some(("wsl", "/home/x"))
        );
        assert_eq!(parse("remote:docker:box:/app"), Some(("docker", "/app")));
        for bad in [
            "/local/path",
            "remote:",
            "remote::x:/p",
            "remote:ftp:h:/p",
            "remote:ssh:host::me:/p",
            "remote:ssh:host:22:me:relative",
            "remote:wsl:Ubuntu:me",
            "remote:docker:box:",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }
}
