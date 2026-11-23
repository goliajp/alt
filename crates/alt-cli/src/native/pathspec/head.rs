//! `HEAD` as a workspace means it.

/// `spec` with `head` in place of the `HEAD` or `@` it begins with, for a
/// workspace whose HEAD is the ref `head`.
pub fn at_head(head: &str, spec: &str) -> String {
    let follows = |rest: &str| rest.is_empty() || rest.starts_with(['~', '^', ':']);
    match (spec.strip_prefix("HEAD"), spec.strip_prefix('@')) {
        (Some(rest), _) if follows(rest) || rest.starts_with("@{") => format!("{head}{rest}"),
        (_, Some(rest)) if follows(rest) => format!("{head}{rest}"),
        (_, Some(rest)) if rest.starts_with('{') => format!("{head}{spec}"),
        _ => spec.to_owned(),
    }
}
