-- The JWT/session implementation was reverted. Keep migration 0004 immutable
-- for databases that already applied it, then remove its unused schema here.
DROP TABLE IF EXISTS identity.auth_sessions;
