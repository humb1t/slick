-- Proposals sent from the landing's #feature-requests DM.
-- A row starts 'pending' and becomes 'confirmed' when the sender clicks the link in the confirmation email.
CREATE TABLE feature_requests (
  id            TEXT PRIMARY KEY,
  text          TEXT NOT NULL,
  email         TEXT NOT NULL,
  lang          TEXT NOT NULL DEFAULT 'en',
  status        TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed')),
  confirm_token TEXT NOT NULL UNIQUE,
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  confirmed_at  TEXT
);

CREATE INDEX feature_requests_status ON feature_requests (status, created_at);
