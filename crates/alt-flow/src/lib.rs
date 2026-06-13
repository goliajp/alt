//! The git-flow branch model as a domain object: the long-lived branch names
//! (`main`, `develop`), the topic-branch prefixes (`feature/`, `release/`,
//! `hotfix/`), and the source/target rules each flow obeys — a feature starts
//! off `develop` and finishes back into it, and so on.
//!
//! Steel: it knows the git-flow domain but performs no I/O and touches no
//! store. It hands the executing layer short branch names and which branch a
//! flow integrates into; the caller turns those into ref transactions.

/// The configurable git-flow branch model. `Default` is the conventional
/// layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchModel {
    pub main: String,
    pub develop: String,
    pub feature_prefix: String,
    pub release_prefix: String,
    pub hotfix_prefix: String,
}

impl Default for BranchModel {
    fn default() -> Self {
        BranchModel {
            main: "main".to_owned(),
            develop: "develop".to_owned(),
            feature_prefix: "feature/".to_owned(),
            release_prefix: "release/".to_owned(),
            hotfix_prefix: "hotfix/".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowError {
    /// The topic name was empty or malformed.
    BadName(String),
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::BadName(n) => write!(f, "invalid flow topic name '{n}'"),
        }
    }
}

impl std::error::Error for FlowError {}

/// One flow's branch layout: the topic branch to create/finish, the branch it
/// starts from, and the branch a finish integrates back into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// The topic branch short name, e.g. `feature/login`.
    pub branch: String,
    /// The branch a start is based on (and, for features, the finish target).
    pub base: String,
    /// The branch a finish merges into.
    pub target: String,
}

impl BranchModel {
    /// A `feature/<name>` flow: starts from and finishes into `develop`.
    pub fn feature(&self, name: &str) -> Result<Flow, FlowError> {
        let name = check_name(name)?;
        Ok(Flow {
            branch: format!("{}{name}", self.feature_prefix),
            base: self.develop.clone(),
            target: self.develop.clone(),
        })
    }

    /// A `release/<name>` flow: starts from `develop`, finishes into `main`
    /// (the caller also back-merges into `develop`; that policy lives above).
    pub fn release(&self, name: &str) -> Result<Flow, FlowError> {
        let name = check_name(name)?;
        Ok(Flow {
            branch: format!("{}{name}", self.release_prefix),
            base: self.develop.clone(),
            target: self.main.clone(),
        })
    }

    /// A `hotfix/<name>` flow: starts from `main`, finishes into `main`.
    pub fn hotfix(&self, name: &str) -> Result<Flow, FlowError> {
        let name = check_name(name)?;
        Ok(Flow {
            branch: format!("{}{name}", self.hotfix_prefix),
            base: self.main.clone(),
            target: self.main.clone(),
        })
    }
}

/// Validates a topic name: non-empty, no slashes (the prefix supplies the
/// hierarchy), no leading dot or whitespace/control characters.
fn check_name(name: &str) -> Result<&str, FlowError> {
    let bad = name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains("..")
        || name.bytes().any(|b| b <= b' ' || b == 0x7f);
    if bad {
        return Err(FlowError::BadName(name.to_owned()));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_starts_and_finishes_on_develop() {
        let m = BranchModel::default();
        let f = m.feature("login").unwrap();
        assert_eq!(f.branch, "feature/login");
        assert_eq!(f.base, "develop");
        assert_eq!(f.target, "develop");
    }

    #[test]
    fn release_starts_develop_targets_main() {
        let m = BranchModel::default();
        let r = m.release("1.0").unwrap();
        assert_eq!(r.branch, "release/1.0");
        assert_eq!(r.base, "develop");
        assert_eq!(r.target, "main");
    }

    #[test]
    fn hotfix_is_main_to_main() {
        let m = BranchModel::default();
        let h = m.hotfix("urgent").unwrap();
        assert_eq!(h.branch, "hotfix/urgent");
        assert_eq!(h.base, "main");
        assert_eq!(h.target, "main");
    }

    #[test]
    fn bad_names_are_rejected() {
        let m = BranchModel::default();
        for n in ["", ".", "a/b", "a..b", "a b", "\tx"] {
            assert!(m.feature(n).is_err(), "{n:?} should be rejected");
        }
    }

    #[test]
    fn a_custom_model_changes_the_names() {
        let m = BranchModel {
            develop: "dev".to_owned(),
            feature_prefix: "f/".to_owned(),
            ..BranchModel::default()
        };
        let f = m.feature("x").unwrap();
        assert_eq!(f.branch, "f/x");
        assert_eq!(f.base, "dev");
    }
}
