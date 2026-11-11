use alt_remote_protocol::{RequestKey, Scope};

#[derive(Clone, Debug, PartialEq, Eq)]

pub enum ExportAction {
    Prepare,
    Query,
    Archive,
    Content(String),
    Freeze,
    Reopen,
    Retire,
}
#[derive(Clone, Debug, PartialEq, Eq)]

pub enum ImportAction {
    Begin,
    Query,
    Upload(String),
    Activate,
    Abandon,
}
#[derive(Clone, Debug, PartialEq, Eq)]

pub enum PortableRoute {
    Capabilities,
    Inspect(Scope),
    Export(Scope, String, ExportAction),
    Import(RequestKey, ImportAction),
}
impl PortableRoute {
    pub fn has_json_body(&self) -> bool {
        matches!(
            self,
            Self::Inspect(_) | Self::Import(_, ImportAction::Begin | ImportAction::Activate)
        )
    }
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::Capabilities
                | Self::Inspect(_)
                | Self::Export(
                    _,
                    _,
                    ExportAction::Query | ExportAction::Archive | ExportAction::Content(_)
                )
                | Self::Import(_, ImportAction::Query)
        )
    }
}
/// Caller already validates the canonical scope and rejects all query strings.
pub fn parse(method: &str, scope: Scope, tail: &[&str]) -> Option<PortableRoute> {
    use crate::route::{digest, id};
    match tail {
        ["inspect"] if method == "POST" => Some(PortableRoute::Inspect(scope)),
        ["exports", export, rest @ ..] if id(export) => {
            let action = match (method, rest) {
                ("POST", []) => ExportAction::Prepare,
                ("GET", []) => ExportAction::Query,
                ("GET", ["archive"]) => ExportAction::Archive,
                ("GET", ["content", content]) if digest(content) => {
                    ExportAction::Content((*content).into())
                }
                ("POST", ["freeze"]) => ExportAction::Freeze,
                ("POST", ["reopen"]) => ExportAction::Reopen,
                ("POST", ["retire"]) => ExportAction::Retire,
                _ => return None,
            };
            Some(PortableRoute::Export(scope, (*export).into(), action))
        }
        ["imports", actor, import, rest @ ..] if id(actor) && id(import) => {
            let action = match (method, rest) {
                ("POST", []) => ImportAction::Begin,
                ("GET", []) => ImportAction::Query,
                ("PUT", ["content", content]) if digest(content) => {
                    ImportAction::Upload((*content).into())
                }
                ("POST", ["activate"]) => ImportAction::Activate,
                ("POST", ["abandon"]) => ImportAction::Abandon,
                _ => return None,
            };
            Some(PortableRoute::Import(
                RequestKey {
                    scope,
                    actor_id: (*actor).into(),
                    request_id: (*import).into(),
                },
                action,
            ))
        }
        _ => None,
    }
}
