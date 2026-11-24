//! `alt init`: an empty store beside the working tree it is made for.

use super::*;

pub fn init(dir: Option<PathBuf>, out: &mut impl Write) -> Res<()> {
    let root = dir.unwrap_or_else(|| PathBuf::from("."));
    let alt_dir = root.join(".alt");
    if alt_dir.exists() {
        return Err(format!("{} already exists", alt_dir.display()).into());
    }
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir(&alt_dir)?;
    std::fs::File::open(&root)?.sync_all()?;
    let mut odb = NativeOdb::open(&alt_dir)?;
    odb.flush()?;
    let mut refs = RefStore::open(&alt_dir)?;
    let context = alt_json::to_vec(&recovery::RefContext {
        version: 2,
        effects: recovery::RefEffects::Unknown,
        workspace: recovery::workspace_identity_at(&alt_dir.join("index"), DEFAULT_WORKSPACE)?,
        inverse_of: None,
        checkout: None,
        request_id: None,
    })?;
    refs.commit_scoped(
        &Identity::from_env().actor("init"),
        now_ms(),
        &[RefChange {
            name: "HEAD".to_owned(),
            old: None,
            new: Some(RefTarget::Symbolic("refs/heads/main".to_owned())),
        }],
        None,
        None,
        Some(&context),
    )?;
    save_index(&alt_dir.join("index"), &empty_index(), HashAlgo::Sha1)?;
    import_config::note_case(&alt_dir)?;
    writeln!(
        out,
        "Initialized empty alt repository in {}",
        alt_dir.display()
    )?;
    Ok(())
}
