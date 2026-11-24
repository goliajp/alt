//! The tag a flow finish leaves on the commit it puts its target at, and
//! the tag object an annotated tag is.

use super::super::*;
use crate::flow_args::Finish;

/// A tag a finish is to make: named and worded before the finish stores
/// anything, so that a name it cannot take ends the finish untouched.
pub(in crate::native) struct FinishTag {
    pub(in crate::native) name: String,
    message: String,
}

/// Says what a finish did: its fields as JSON, or the tag it made and
/// its line.
pub(super) fn finished(
    out: &mut impl Write,
    json: bool,
    mut fields: Vec<(&'static str, crate::json::Json)>,
    tag: Option<FinishTag>,
    said: &str,
) -> Res<()> {
    if json {
        fields.extend(tag.map(|tag| ("tag", crate::json::Json::str(tag.name))));
        return Ok(crate::json::emit(out, fields)?);
    }
    if let Some(tag) = tag {
        writeln!(out, "Created tag '{}'", tag.name)?;
    }
    writeln!(out, "{said}")?;
    Ok(())
}

impl NativeRepo<'_> {
    /// The tag the finish of a topic of `kind` is to make, where it makes
    /// one: a release and a hotfix are tagged unless
    /// `gitflow.branch.<kind>.tag` or the command says otherwise.
    pub(in crate::native) fn finish_tag(
        &self,
        kind: &str,
        asked: &Finish,
    ) -> Res<Option<FinishTag>> {
        let settings = self.settings()?;
        let about = format!("branch.{kind}");
        let about = Some(about.as_bytes());
        let set = settings.get_bool("gitflow", about, "tag").transpose()?;
        if !asked.tag_made && (asked.no_tag || !set.unwrap_or(kind != "feature")) {
            return Ok(None);
        }
        let name = match &asked.tag_name {
            Some(name) => name.clone(),
            None => {
                let prefix = settings.get_str("gitflow", about, "tagprefix");
                let prefix = prefix.map(|p| p.to_string()).unwrap_or_default();
                format!("{prefix}{}", asked.name)
            }
        };
        if check_tag_name(&name).is_err() {
            let said = format!("'{name}' is not a valid tag name.");
            return Err(crate::git_settings::fatal(said));
        }
        if self.store.refs.get(&format!("refs/tags/{name}")).is_some() {
            let said = format!("tag '{name}' already exists");
            return Err(crate::git_settings::fatal(said));
        }
        let message = match (&asked.tag_message, &asked.tag_message_file) {
            (Some(message), _) => message.clone(),
            (None, Some(file)) => {
                let read = std::fs::read_to_string(self.root.join(&self.prefix).join(file));
                let unread = |e| format!("could not open or read '{}': {e}", file.display());
                read.map_err(|e| crate::git_settings::fatal(unread(e)))?
            }
            (None, None) => format!("Tagging version {}", asked.name),
        };
        Ok(Some(FinishTag { name, message }))
    }

    /// Stores the tag object of `tag` on `commit`; the change of refs that
    /// names it, for the transaction of the finish.
    pub(in crate::native) fn finish_tagged(
        &mut self,
        tag: Option<&FinishTag>,
        commit: ObjectId,
    ) -> Res<Option<RefChange>> {
        let Some(tag) = tag else { return Ok(None) };
        let object = self.store_tag(&tag.name, commit, &tag.message)?;
        Ok(Some(RefChange {
            name: format!("refs/tags/{}", tag.name),
            old: None,
            new: Some(RefTarget::Oid(object)),
        }))
    }

    /// Stores the object of an annotated tag `name` on `target`, by the
    /// caller as committer, as git writes it: the message without the
    /// lines that begin with `#`, the space at the end of a line and the
    /// blank lines git takes out of the message of a tag.
    pub(in crate::native) fn store_tag(
        &mut self,
        name: &str,
        target: ObjectId,
        message: &str,
    ) -> Res<ObjectId> {
        let kind = self.store.odb.lookup(&target)?.map(|e| e.kind);
        let kind = kind.ok_or_else(|| format!("target {target} not in the object store"))?;
        let kind = match kind {
            ObjectKind::Commit => "commit",
            ObjectKind::Tree => "tree",
            ObjectKind::Blob => "blob",
            ObjectKind::Tag => "tag",
        };
        let message = &sequence_editor::cleanup(message);
        let tagger = self.committer()?;
        let (who, mail) = (tagger.name(), tagger.email());
        let (when, tz) = (tagger.when(), tagger.tz());
        let bytes = format!(
            "object {target}\ntype {kind}\ntag {name}\ntagger {who} <{mail}> {when} {tz}\n\n{message}"
        )
        .into_bytes();
        let object = ObjectId::hash_object(self.store.algo()?, ObjectKind::Tag, &bytes);
        self.store.odb.put(object, ObjectKind::Tag, &bytes)?;
        self.store.odb.flush()?;
        Ok(object)
    }
}
