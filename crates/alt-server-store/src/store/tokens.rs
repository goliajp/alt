//! The tokens of credentials: what they look like, and how one is written down.

use super::*;

pub(crate) fn token_digest(token: &str) -> String {
    alt_hash::blake3::derive_key("jp.golia.alt.server-token.v1", token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn insert_token(
    tx: &mut Transaction<'_>,
    actor: &str,
    attribution: &Attribution,
    source: DeploymentMode,
    expires: u64,
    administrator: bool,
) -> Result<String> {
    let mut secret = [0u8; 32];
    alt_os::random::fill(&mut secret);
    let token = format!(
        "alt1_{}",
        secret
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    insert_known_token(
        tx,
        &token,
        actor,
        attribution,
        source,
        expires,
        administrator,
    )?;
    Ok(token)
}
pub(crate) fn validate_token(token: &str) -> Result<()> {
    if token.len() != 69
        || !token.starts_with("alt1_")
        || !token.as_bytes()[5..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_known_token(
    tx: &mut Transaction<'_>,
    token: &str,
    actor: &str,
    attribution: &Attribution,
    source: DeploymentMode,
    expires: u64,
    administrator: bool,
) -> Result<()> {
    validate_token(token)?;
    Scope {
        installation_id: actor.into(),
        repository_instance_id: "actor".into(),
        recovery_epoch: 1,
    }
    .validate()?;
    attribution.validate()?;
    if expires <= now(tx)? {
        return Err(Error::Precondition("credential expiry"));
    }
    let digest = token_digest(token);
    let attribution = Jsonb::from(attribution)?;
    tx.execute("INSERT INTO alt_credentials(token_digest,actor_id,attribution,identity_source,expires_at,administrator) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(token_digest) DO NOTHING",&[&digest,&actor,&attribution,&mode(source),&integer(expires)?,&administrator])?;
    let row=tx.query_one("SELECT actor_id,attribution,identity_source,expires_at,administrator,revoked FROM alt_credentials WHERE token_digest=$1 FOR SHARE",&[&digest])?;
    if row.get::<_, String>(0) != actor
        || row.get::<_, Jsonb>(1) != attribution
        || row.get::<_, String>(2) != mode(source)
        || row.get::<_, i64>(3) != integer(expires)?
        || row.get::<_, bool>(4) != administrator
        || row.get::<_, bool>(5)
    {
        return Err(Error::Precondition(
            "credential secret already has different or revoked claims",
        ));
    }
    Ok(())
}
