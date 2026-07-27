import express from 'express';
import cors from 'cors';
import helmet from 'helmet';
import cookieParser from 'cookie-parser';
import bcrypt from 'bcryptjs';
import { rateLimit } from 'express-rate-limit';
import { ZodError } from 'zod';
import { config } from './config.js';
import { pool, query, withTransaction } from './db.js';
import { createSessionToken, requireAuth, sessionCookieName, sessionCookieOptions } from './auth.js';
import { loginSchema, stateUpdateSchema } from './schemas.js';
import gatepassRouter from './modules/gatepass/index.js';

const app = express();

app.disable('x-powered-by');
if (config.nodeEnv === 'production') app.set('trust proxy', ['loopback', 'linklocal', 'uniquelocal']);
app.use(helmet());
app.use(cors({ origin: config.frontendOrigin, credentials: true }));
app.use(express.json({ limit: '128kb' }));
app.use(cookieParser());
app.use('/api', (_request, response, next) => {
  response.set('Cache-Control', 'no-store');
  response.set('Pragma', 'no-cache');
  next();
});

// ─── Rate Limiting ────────────────────────────────────────────────────────────

const loginLimiter = rateLimit({
  windowMs: 15 * 60 * 1000,
  limit: 10,
  standardHeaders: 'draft-8',
  legacyHeaders: false,
  message: { error: 'Too many login attempts. Please try again later.' },
});

// ─── Helpers ──────────────────────────────────────────────────────────────────

function serializeStudent(row) {
  return {
    id: row.id, tenantId: row.tenantId, roll: row.roll, name: row.name,
    initials: row.initials, email: row.email, college: row.college,
    fullCollege: row.fullCollege, dept: row.dept, year: row.year,
    tenant: { id: row.tenantId, code: row.college, name: row.fullCollege, city: row.city },
  };
}

const studentSelect = `
  SELECT s.id, s.tenant_id AS "tenantId", s.roll_number AS roll, s.name, s.initials,
         s.email, s.password_hash AS "passwordHash", s.department AS dept, s.study_year AS year,
         t.code AS college, t.name AS "fullCollege", t.city
  FROM students s JOIN tenants t ON t.id = s.tenant_id`;

// ─── Public Routes ────────────────────────────────────────────────────────────

app.get('/api/health', async (_request, response, next) => {
  try {
    await query('SELECT 1');
    response.json({ status: 'ok', database: 'connected', timestamp: new Date().toISOString() });
  } catch (error) { next(error); }
});

app.get('/api/auth/tenants', async (_request, response, next) => {
  try {
    const result = await query('SELECT id, code, name, city FROM tenants WHERE is_active ORDER BY name');
    response.json({ data: result.rows });
  } catch (error) { next(error); }
});

app.post('/api/auth/login', loginLimiter, async (request, response, next) => {
  try {
    const credentials = loginSchema.parse(request.body);
    const result = await query(
      `${studentSelect}
       WHERE lower(s.email) = $1 AND s.is_active AND t.is_active`,
      [credentials.email],
    );
    const student = result.rows[0];
    const comparisonHash = student?.passwordHash ?? '$2b$12$MJVUf5IF0ZvPPw3HkZxWhuat1fdlyiPr6VlHQAeTLnp4mktZXHWqa';
    const matches = await bcrypt.compare(credentials.password, comparisonHash);
    if (!student || !matches) return response.status(401).json({ error: 'Invalid email or password' });
    const token = createSessionToken(student);
    response.cookie(sessionCookieName, token, sessionCookieOptions);
    await query(
      `INSERT INTO activity_events (student_id, event_type, metadata)
       VALUES ($1, 'AUTH_LOGIN', jsonb_build_object('tenantId', $2::text))`,
      [student.id, student.tenantId],
    );
    response.json({ data: { student: serializeStudent(student) } });
  } catch (error) { next(error); }
});

app.get('/api/auth/me', requireAuth, async (request, response, next) => {
  try {
    const result = await query(
      `${studentSelect} WHERE s.id = $1 AND s.tenant_id = $2 AND s.is_active AND t.is_active`,
      [request.auth.id, request.auth.tenantId],
    );
    if (!result.rows[0]) return response.status(401).json({ error: 'Session no longer valid' });
    response.json({ data: { student: serializeStudent(result.rows[0]) } });
  } catch (error) { next(error); }
});

app.post('/api/auth/logout', requireAuth, async (request, response, next) => {
  try {
    await query(`INSERT INTO activity_events (student_id, event_type) VALUES ($1, 'AUTH_LOGOUT')`, [request.auth.id]);
    response.clearCookie(sessionCookieName, { ...sessionCookieOptions, maxAge: undefined });
    response.status(204).end();
  } catch (error) { next(error); }
});

// ─── Protected Routes (require auth) ─────────────────────────────────────────

app.use('/api', requireAuth);

app.get('/api/student', async (request, response, next) => {
  try {
    const result = await query(
      `${studentSelect} WHERE s.id = $1 AND s.tenant_id = $2`,
      [request.auth.id, request.auth.tenantId],
    );
    if (!result.rows[0]) return response.status(404).json({ error: 'Student not found' });
    response.json({ data: serializeStudent(result.rows[0]) });
  } catch (error) { next(error); }
});

app.get('/api/state', async (request, response, next) => {
  try {
    const result = await query(
      `SELECT sas.state, sas.version, sas.updated_at AS "updatedAt"
       FROM student_app_state sas JOIN students s ON s.id = sas.student_id
       WHERE sas.student_id = $1 AND s.tenant_id = $2`,
      [request.auth.id, request.auth.tenantId],
    );
    if (!result.rows[0]) return response.status(404).json({ error: 'State not found' });
    response.json({ data: result.rows[0] });
  } catch (error) { next(error); }
});

app.put('/api/state', async (request, response, next) => {
  try {
    const payload = stateUpdateSchema.parse(request.body);
    const saved = await withTransaction(async (client) => {
      const result = await client.query(
        `UPDATE student_app_state sas SET state = $3::jsonb, version = version + 1, updated_at = now()
         FROM students s
         WHERE sas.student_id = s.id AND sas.student_id = $1 AND s.tenant_id = $2
         RETURNING sas.state, sas.version, sas.updated_at AS "updatedAt"`,
        [request.auth.id, request.auth.tenantId, JSON.stringify(payload.state)],
      );
      if (!result.rows[0]) { const error = new Error('State not found'); error.status = 404; throw error; }
      if (payload.action) {
        await client.query(
          `INSERT INTO activity_events (student_id, event_type, metadata)
           VALUES ($1, $2, jsonb_build_object('tenantId', $3::text))`,
          [request.auth.id, payload.action, request.auth.tenantId],
        );
      }
      return result.rows[0];
    });
    response.json({ data: saved });
  } catch (error) { next(error); }
});

app.get('/api/activity', async (request, response, next) => {
  try {
    const requestedLimit = Number.parseInt(String(request.query.limit ?? '20'), 10);
    const limit = Number.isInteger(requestedLimit) ? Math.min(Math.max(requestedLimit, 1), 100) : 20;
    const result = await query(
      `SELECT ae.id, ae.event_type AS "eventType", ae.metadata, ae.created_at AS "createdAt"
       FROM activity_events ae JOIN students s ON s.id = ae.student_id
       WHERE ae.student_id = $1 AND s.tenant_id = $2
       ORDER BY ae.created_at DESC LIMIT $3`,
      [request.auth.id, request.auth.tenantId, limit],
    );
    response.json({ data: result.rows });
  } catch (error) { next(error); }
});

// ─── Gatepass Module ──────────────────────────────────────────────────────────

app.use('/api/gatepass', gatepassRouter);

// ─── Error Handling ───────────────────────────────────────────────────────────

app.use((_request, response) => response.status(404).json({ error: 'Route not found' }));

// eslint-disable-next-line no-unused-vars
app.use((error, _request, response, _next) => {
  if (error instanceof ZodError) return response.status(400).json({ error: 'Invalid request', details: error.issues });
  const status = Number.isInteger(error.status) ? error.status : 500;
  console.error(error);
  response.status(status).json({ error: status === 500 ? 'Internal server error' : error.message });
});

// ─── Start Server ─────────────────────────────────────────────────────────────

const server = app.listen(config.port, config.apiHost, () => {
  console.log(`SuperCampus API listening on http://${config.apiHost}:${config.port}`);
});

async function shutdown(signal) {
  console.log(`${signal} received, shutting down`);
  server.close(async () => { await pool.end(); process.exit(0); });
}
process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
