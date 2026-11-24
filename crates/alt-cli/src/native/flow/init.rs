//! `alt flow init`.

use super::super::*;

impl NativeRepo<'_> {
    /// `alt flow init`: create `develop` off `main` (or the current branch's
    /// commit) and switch to it, in one ref transaction.
    pub fn flow_init(&mut self, json: bool, out: &mut impl Write) -> Res<()> {
        use crate::json::Json;
        let model = self.flow_model();
        let dev_ref = format!("refs/heads/{}", model.develop);
        if self.store.refs.get(&dev_ref).is_some() {
            if json {
                crate::json::emit(
                    out,
                    vec![
                        ("develop", Json::str(model.develop)),
                        ("already_initialized", Json::Bool(true)),
                    ],
                )?;
            } else {
                writeln!(out, "flow already initialized (branch '{}')", model.develop)?;
            }
            return Ok(());
        }
        let main_ref = format!("refs/heads/{}", model.main);
        let start = match self.store.refs.resolve(&main_ref)? {
            Some(c) => c,
            None => self
                .store
                .refs
                .resolve(&self.head_branch()?)?
                .ok_or("create an initial commit before 'alt flow init'")?,
        };
        let head_old = self.store.refs.get(&self.head_ref).cloned();
        let head_ref = self.head_ref.clone();
        self.commit_refs_only(
            "flow",
            &[
                RefChange {
                    name: dev_ref.clone(),
                    old: None,
                    new: Some(RefTarget::Oid(start)),
                },
                RefChange {
                    name: head_ref,
                    old: head_old,
                    new: Some(RefTarget::Symbolic(dev_ref.clone())),
                },
            ],
        )?;
        if json {
            crate::json::emit(
                out,
                vec![
                    ("develop", Json::str(model.develop)),
                    ("already_initialized", Json::Bool(false)),
                    ("commit", Json::str(start.to_string())),
                ],
            )?;
        } else {
            writeln!(
                out,
                "Initialized flow: created '{}' at {start}",
                model.develop
            )?;
        }
        Ok(())
    }
}
