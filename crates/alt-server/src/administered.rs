//! The operator actions that are done as an administrator of the instance.

use super::*;
use alt_server_store::{Credential, Store};

pub(super) fn execute(
    args: &Args,
    config: &Config,
    store: &mut Store,
    (admin, token): (&Credential, &str),
) -> Result<Value> {
    let result = match &args.command {
        Action::LinkInstall {
            repository,
            epoch,
            channel,
        } => {
            let scope = alt_remote_protocol::Scope {
                installation_id: config.instance.installation_id.clone(),
                repository_instance_id: repository.clone(),
                recovery_epoch: *epoch,
            };
            let factory = config.factory()?;
            let runtime = factory
                .change_links
                .iter()
                .find(|c| c.matches(&scope, channel))
                .ok_or(Error::Config("change-link is not configured"))?;
            runtime.install(&factory, token)?;
            json!({"repository":repository,"channel":channel,"installed":true})
        }
        Action::LinkDisable {
            repository,
            epoch,
            channel,
        } => {
            let scope = alt_remote_protocol::Scope {
                installation_id: config.instance.installation_id.clone(),
                repository_instance_id: repository.clone(),
                recovery_epoch: *epoch,
            };
            store.disable_change_channel(admin, &scope, channel)?;
            json!({"repository":repository,"channel":channel,"disabled":true})
        }
        Action::Status => {
            alt_json::to_value(store.instance_status(admin)?).map_err(|_| Error::Store)?
        }
        Action::Audit { before, limit } => {
            alt_json::to_value(store.instance_audit(admin, *before, *limit)?)
                .map_err(|_| Error::Store)?
        }
        Action::RepositoryCreate { id, quota_bytes } => {
            json!({"repository":store.create_repository(admin,id,*quota_bytes)?})
        }
        Action::Grants {
            repository,
            actor,
            file,
        } => {
            let grant: Grants = json_file(file)?;
            store.set_grants(
                admin,
                repository,
                actor,
                grant.read,
                grant.publish_records,
                &grant.grants,
            )?;
            json!({"repository":repository,"actor":actor,"grants_updated":true})
        }
        Action::ReviewGrant {
            repository,
            actor,
            allow,
        } => {
            store.set_review_grant(admin, repository, actor, *allow)?;
            json!({"repository":repository,"actor":actor,"can_review":allow})
        }
        Action::CollectorPolicy { repository, file } => {
            store.set_collector_policy(admin, repository, &json_file(file)?)?;
            json!({"repository":repository,"collector_policy_updated":true})
        }
        Action::BranchPolicy { repository, file } => {
            store.set_policies(
                admin,
                repository,
                &json_file::<Vec<alt_repository_service::RefPolicy>>(file)?,
            )?;
            json!({"repository":repository,"branch_policy_updated":true})
        }
        Action::DefaultBranch { repository, branch } => {
            store.set_default_branch(admin, repository, branch)?;
            json!({"repository":repository,"default_branch":branch})
        }
        Action::TokenIssue {
            actor,
            secret,
            expires_unix_ms,
            administrator,
            attribution,
        } => {
            let attribution = match attribution {
                Some(path) => json_file(path)?,
                None => Attribution {
                    kind: ActorKind::Human,
                    on_behalf_of: None,
                    delegation_id: None,
                    agent_session: None,
                },
            };
            store.issue_local_token_with_secret(
                admin,
                &operator::read_token(secret)?,
                actor,
                &attribution,
                *expires_unix_ms,
                *administrator,
            )?;
            json!({"actor":actor,"credential_registered":true,"expires_unix_ms":expires_unix_ms.to_string()})
        }
        Action::BrowserEnable => {
            let browser = config
                .browser
                .as_ref()
                .ok_or(Error::Config("browser configuration required"))?;
            let origin = alt_browser_auth::PublicOrigin::parse(&browser.public_origin)
                .map_err(|_| Error::Config("browser origin"))?;
            store.enable_browser(admin, &origin)?;
            json!({"browser_enabled":true,"public_origin":origin.as_str()})
        }
        Action::UserCreate {
            actor,
            login,
            display_name,
            password_file: path,
            administrator,
        } => {
            let password = password_file(path)?;
            json!({"user":store.create_local_user(admin, actor, login, display_name, &password, *administrator)?})
        }
        Action::UserResetPassword {
            actor,
            password_file: path,
        } => {
            store.reset_local_password(admin, actor, &password_file(path)?)?;
            json!({"actor":actor,"password_reset":true,"old_credentials_revoked":true})
        }
        Action::TokenRevoke { secret } => {
            store.revoke(admin, &operator::read_token(secret)?)?;
            json!({"revoked":true})
        }
        Action::Collect { repository, limit } => {
            json!({"repository":repository,"removed":store.collect_orphans(admin,repository,*limit)?})
        }
        Action::Scrub { repository, mend } => {
            operator::scrub::read(store, admin, repository.as_deref(), *mend)?
        }
        _ => return Err(Error::Config("unsupported operator action")),
    };
    Ok(result)
}
