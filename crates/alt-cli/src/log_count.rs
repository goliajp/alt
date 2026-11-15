//! The `-<n>` shorthand of `alt log`.

/// `alt log -3` means `alt log -n 3`, as in git.
pub fn with_count_option(mut words: Vec<String>) -> Vec<String> {
    let mut at = 1;
    // the subcommand is the first word that is neither a global option nor
    // the value of one
    while let Some(word) = words.get(at) {
        match word.as_str() {
            "--workspace" | "--request-id" => at += 2,
            option if option.starts_with('-') => at += 1,
            _ => break,
        }
    }
    if words.get(at).map(String::as_str) != Some("log") {
        return words;
    }
    at += 1;
    while let Some(word) = words.get(at) {
        if word == "--" {
            break;
        }
        let count = word
            .strip_prefix('-')
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        if let Some(count) = count.map(str::to_owned) {
            words[at] = "-n".to_owned();
            words.insert(at + 1, count);
            at += 1;
        }
        at += 1;
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dash_and_a_number_after_log_is_a_commit_count() {
        let expand = |args: &[&str]| {
            with_count_option(args.iter().map(|w| (*w).to_owned()).collect()).join(" ")
        };
        assert_eq!(expand(&["alt", "log", "-1"]), "alt log -n 1");
        assert_eq!(expand(&["alt", "log", "-25", "main"]), "alt log -n 25 main");
        assert_eq!(
            expand(&["alt", "--workspace", "log", "log", "-p", "-3"]),
            "alt --workspace log log -p -n 3"
        );
        // only for log, only before `--`, and only whole numbers
        assert_eq!(expand(&["alt", "show", "-1"]), "alt show -1");
        assert_eq!(expand(&["alt", "log", "--", "-1"]), "alt log -- -1");
        assert_eq!(expand(&["alt", "log", "-1x", "-"]), "alt log -1x -");
        assert_eq!(expand(&["alt", "branch", "log", "-1"]), "alt branch log -1");
    }
}
