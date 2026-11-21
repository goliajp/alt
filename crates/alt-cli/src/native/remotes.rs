//! `alt remote list`.

use super::*;

impl NativeRepo<'_> {
    /// `alt remote list`: emit configured remotes (alphabetical). Empty
    /// when no `remotes/` dir exists — a fresh repo has no remotes.
    pub fn remote_list(&self, json: bool, out: &mut impl Write) -> Res<()> {
        let remotes = self.read_remotes()?;
        if json {
            use crate::json::Json;
            let arr = remotes
                .iter()
                .map(|r| {
                    Json::Object(vec![
                        ("name", Json::str(&r.name)),
                        ("url", Json::str(&r.url)),
                        ("fetch", Json::str(&r.fetch)),
                    ])
                })
                .collect();
            crate::json::emit(out, vec![("remotes", Json::Array(arr))])?;
        } else {
            for r in &remotes {
                writeln!(out, "{}\t{}", r.name, r.url)?;
            }
        }
        Ok(())
    }
}
