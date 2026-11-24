//! A repository that belongs to another user is opened, as by git and
//! by the commands of alt, only where `safe.directory` names it.

use alt_web::{ApiError, MultiRepo};

#[test]
#[ignore = "needs root: gives a repository to another user"]
fn a_repository_of_another_user_is_opened_only_where_it_is_named_safe() {
    let dir = alt_os::temp::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let repo = crate::endpoint_smoke::build_fixture_alt(&root, "held");
    let global = dir.path().join("global");
    std::fs::write(&global, "").unwrap();
    // the test is run alone, by its name: no other thread reads these
    unsafe {
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::set_var("GIT_CONFIG_GLOBAL", &global);
        std::env::remove_var("SUDO_UID");
    }
    let repos = MultiRepo::new(&root);
    assert!(repos.open("held").is_ok());
    for path in [repo.clone(), repo.join(".alt")] {
        std::os::unix::fs::chown(path, Some(65534), None).unwrap();
    }
    match repos.open("held") {
        Err(ApiError::RepoOpen(why)) => {
            assert!(why.contains("safe.directory"), "{why}");
            assert!(!why.contains(root.to_str().unwrap()), "{why}");
        }
        other => panic!("opened, or refused otherwise: {:?}", other.map(|_| ())),
    }
    // named by its working tree, or by its control directory
    let at = repo.canonicalize().unwrap();
    for named in [at.clone(), at.join(".alt")] {
        let safe = format!("[safe]\n\tdirectory = {}\n", named.display());
        std::fs::write(&global, safe).unwrap();
        assert!(repos.open("held").is_ok(), "{}", named.display());
    }
    std::fs::write(&global, "[safe]\n\tdirectory = /elsewhere\n").unwrap();
    assert!(repos.open("held").is_err());
}
