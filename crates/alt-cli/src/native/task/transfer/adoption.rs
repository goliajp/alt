use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct Predecessor {
    pub digest: String,
    pub request: String,
}
#[derive(alt_json::ToJson, alt_json::FromJson)]
#[json(deny_unknown_fields)]
pub(super) struct Adoption {
    pub version: u32,
    pub task: String,
    pub publication: String,
    pub destination: Scope,
    pub creator_request: String,
    #[json(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Predecessor>,
}
const MARKER_BYTES: usize = 8192;
impl NativeRepo<'_> {
    pub(super) fn adoption_path(&self, id: &str) -> PathBuf {
        self.index_path
            .with_file_name("task-adoptions")
            .join(format!("{id}.json"))
    }
    /// Historical markers supply attribution only. The current whole-command
    /// completion is the sole authority for the current workspace adoption.
    pub(super) fn read_adoption(&self, id: &str) -> Res<(Adoption, Vec<u8>, usize)> {
        alt_task::check_id(id)?;
        let path = self.adoption_path(id);
        let original = bounded(&path, MARKER_BYTES)?;
        let current: Adoption = alt_json::from_slice(&original)?;
        let scope = self.task_scope()?;
        let history = path.parent().expect("marker parent").join("history");
        let mut raw = original.clone();
        let mut requests = BTreeSet::new();
        let mut digests = BTreeSet::new();
        for depth in 1..=64 {
            let marker: Adoption = alt_json::from_slice(&raw)?;
            crate::request::parse_id(&marker.creator_request)?;
            alt_task::check_id(&marker.publication)?;
            if !matches!(
                (marker.version, &marker.supersedes),
                (1, None) | (2, Some(_))
            ) || marker.task != id
                || marker.destination != scope
                || marker.publication != current.publication
                || !requests.insert(marker.creator_request.clone())
                || !digests.insert(alt_hash::blake3::hash(&raw).to_hex().to_string())
            {
                return Err(refused("task adoption history or destination mismatch"));
            }
            let Some(prior) = marker.supersedes else {
                return Ok((current, original, depth));
            };
            alt_task::check_id(&prior.digest)?;
            crate::request::parse_id(&prior.request)?;
            raw = bounded(
                &history.join(format!("{}.json", prior.digest)),
                MARKER_BYTES,
            )?;
            if alt_hash::blake3::hash(&raw).to_hex().as_str() != prior.digest {
                return Err(refused("task adoption predecessor bytes changed"));
            }
            let previous: Adoption = alt_json::from_slice(&raw)?;
            if previous.creator_request != prior.request {
                return Err(refused("task adoption predecessor request mismatch"));
            }
        }
        Err(refused(
            "task adoption history exceeds the supported chain limit",
        ))
    }
    pub(super) fn adoption_applied(&self, adoption: &Adoption) -> Res<bool> {
        let directory = self
            .store
            .alt_dir
            .join("requests")
            .join(&adoption.creator_request);
        let intent: crate::request::Intent = json_file(&directory.join("intent.json"))?;
        alt_task::check_id(&intent.binding_digest)?;
        if intent.version != 1
            || intent.hash_algorithm != "blake3"
            || intent.repository != adoption.destination.repository
            || intent.workspace != adoption.destination.workspace
            || intent.request_id != adoption.creator_request
        {
            return Err(refused("task adoption request binding mismatch"));
        }
        let Some((completion, response)) = crate::request::read_completion(&directory, &intent)?
        else {
            return Ok(false);
        };
        if completion.outcome != "applied" || response.exit_code != 0 {
            return Ok(false);
        }
        let result: alt_json::Value = alt_json::from_slice(&response.stdout)?;
        if result["adopted_task_id"] != adoption.task
            || result["publication_digest"] != adoption.publication
            || result["destination"] != alt_json::to_value(&adoption.destination)?
            || result["supersedes"] != alt_json::to_value(&adoption.supersedes)?
        {
            return Err(refused("task adoption completion binding mismatch"));
        }
        Ok(true)
    }
    pub(super) fn preserve_adoption(&self, id: &str, raw: &[u8], prior: &Predecessor) -> Res<()> {
        let directory = self
            .adoption_path(id)
            .parent()
            .expect("marker parent")
            .join("history");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.json", prior.digest));
        let mut temporary = alt_os::temp::NamedTempFile::new_in(&directory)?;
        temporary.write_all(raw)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => (),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if bounded(&path, MARKER_BYTES)? != raw {
                    return Err(refused("task adoption history collision"));
                }
            }
            Err(error) => return Err(error.error.into()),
        }
        for parent in directory.canonicalize()?.ancestors() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod json_tests {
    use super::*;

    alt_json_testkit::module_fixture_tests!("native-task-transfer-adoption"; Predecessor, Adoption);
}
