//! `HEAD` as a workspace means it.

/// `spec` with `head` in place of the `HEAD` or `@` it begins with, for a
/// workspace whose HEAD is the ref `head`. `@{N}` alone becomes
/// `<head>@{.N}`.
pub fn at_head(head: &str, spec: &str) -> String {
    let follows = |rest: &str| rest.is_empty() || rest.starts_with(['~', '^', ':']);
    match (spec.strip_prefix("HEAD"), spec.strip_prefix('@')) {
        // git has no `HEAD@{-N}`: it is left to name nothing
        (Some(rest), _) if rest.starts_with("@{-") => spec.to_owned(),
        (Some(rest), _) if follows(rest) || rest.starts_with("@{") => format!("{head}{rest}"),
        (_, Some(rest)) if follows(rest) => format!("{head}{rest}"),
        // the log of the branch the workspace is on, not of its head
        (_, Some(rest))
            if rest.starts_with('{') && rest[1..].starts_with(|c: char| c.is_ascii_digit()) =>
        {
            format!("{head}@{{.{}", &rest[1..])
        }
        // the branch left: asked of the head of the default workspace
        // as git writes it, with no name before it
        (_, Some(rest)) if rest.starts_with("{-") && head == "HEAD" => spec.to_owned(),
        (_, Some(rest)) if rest.starts_with('{') => format!("{head}{spec}"),
        _ => spec.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::at_head;

    #[test]
    fn the_head_of_a_workspace_stands_where_head_is_meant() {
        let at = |spec| at_head("workspaces/w/HEAD", spec);
        assert_eq!(at("HEAD"), "workspaces/w/HEAD");
        assert_eq!(at("HEAD~2"), "workspaces/w/HEAD~2");
        assert_eq!(at("@"), "workspaces/w/HEAD");
        assert_eq!(at("HEAD@{1}"), "workspaces/w/HEAD@{1}");
        assert_eq!(at("@{u}"), "workspaces/w/HEAD@{u}");
        assert_eq!(at("@{-1}"), "workspaces/w/HEAD@{-1}");
        assert_eq!(at("HEAD@{-1}"), "HEAD@{-1}");
        assert_eq!(at_head("HEAD", "@{-1}~1"), "@{-1}~1");
        assert_eq!(at_head("HEAD", "@{u}"), "HEAD@{u}");
        // the log of the branch, not of the head
        assert_eq!(at("@{2}"), "workspaces/w/HEAD@{.2}");
        assert_eq!(at("@{2}~1"), "workspaces/w/HEAD@{.2}~1");
        assert_eq!(at("main@{1}"), "main@{1}");
        assert_eq!(at("HEADS"), "HEADS");
    }
}
