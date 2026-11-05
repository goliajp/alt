CREATE TABLE alt_browser_binding (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  public_origin text NOT NULL
);
CREATE TABLE alt_web_accounts (
  actor_id text PRIMARY KEY,
  login_name text UNIQUE NOT NULL,
  display_name text NOT NULL,
  password_hash text NOT NULL,
  administrator boolean NOT NULL,
  disabled boolean NOT NULL DEFAULT false,
  generation bigint NOT NULL DEFAULT 1 CHECK (generation > 0)
);
CREATE TABLE alt_web_sessions (
  token_digest text PRIMARY KEY REFERENCES alt_credentials(token_digest),
  actor_id text NOT NULL REFERENCES alt_web_accounts(actor_id),
  account_generation bigint NOT NULL CHECK (account_generation > 0),
  expires_at bigint NOT NULL,
  created_at bigint NOT NULL
);
CREATE INDEX alt_web_sessions_actor ON alt_web_sessions(actor_id, expires_at);
CREATE TABLE alt_login_budgets (
  key text PRIMARY KEY,
  window_start bigint NOT NULL,
  attempts integer NOT NULL CHECK (attempts > 0)
);
CREATE TABLE alt_instance_audit (
  sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  actor_id text NOT NULL,
  action text NOT NULL,
  subject_id text NOT NULL,
  details jsonb NOT NULL,
  occurred_at bigint NOT NULL
);
