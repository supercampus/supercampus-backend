import 'dotenv/config';

const placeholderSecrets = new Set([
  'replace-with-a-long-random-production-secret',
  'change-me',
]);

function parseFrontendOrigin(value, nodeEnv) {
  const origin = value ?? (nodeEnv === 'production' ? undefined : 'http://localhost:3000');
  if (!origin) throw new Error('FRONTEND_ORIGIN is required in production.');

  let parsed;
  try {
    parsed = new URL(origin);
  } catch {
    throw new Error('FRONTEND_ORIGIN must be a valid http(s) origin.');
  }
  if (!['http:', 'https:'].includes(parsed.protocol) || parsed.origin !== origin) {
    throw new Error('FRONTEND_ORIGIN must contain only an http(s) origin without a path or trailing slash.');
  }
  if (nodeEnv === 'production' && parsed.protocol !== 'https:') {
    throw new Error('FRONTEND_ORIGIN must use HTTPS in production.');
  }
  return parsed.origin;
}

export function loadConfig(env = process.env) {
  const nodeEnv = env.NODE_ENV ?? 'development';
  const port = Number.parseInt(env.PORT ?? '4000', 10);
  const poolMax = Number.parseInt(env.PGPOOL_MAX ?? '10', 10);
  const databaseSsl = env.DATABASE_SSL ?? 'disable';
  const apiHost = env.API_HOST ?? '127.0.0.1';

  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error('PORT must be a valid TCP port.');
  }
  if (!env.DATABASE_URL) {
    throw new Error('DATABASE_URL is required. Copy .env.example to .env and update it.');
  }
  if (!['disable', 'require', 'verify-full'].includes(databaseSsl)) {
    throw new Error('DATABASE_SSL must be disable, require, or verify-full.');
  }
  if (!env.JWT_SECRET || env.JWT_SECRET.length < 32 || placeholderSecrets.has(env.JWT_SECRET)) {
    throw new Error('JWT_SECRET must be a non-placeholder value of at least 32 characters.');
  }

  return {
    nodeEnv,
    port,
    apiHost,
    databaseUrl: env.DATABASE_URL,
    frontendOrigin: parseFrontendOrigin(env.FRONTEND_ORIGIN, nodeEnv),
    poolMax: Number.isInteger(poolMax) && poolMax > 0 ? poolMax : 10,
    jwtSecret: env.JWT_SECRET,
    databaseSsl,
  };
}

export const config = loadConfig();
