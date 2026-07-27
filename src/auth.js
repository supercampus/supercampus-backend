import jwt from 'jsonwebtoken';
import { config } from './config.js';
import { query } from './db.js';

export const sessionCookieName = 'supercampus_session';
export const sessionCookieOptions = {
  httpOnly: true,
  secure: config.nodeEnv === 'production',
  sameSite: config.nodeEnv === 'production' ? 'none' : 'lax',
  maxAge: 8 * 60 * 60 * 1000,
  path: '/',
};

export function createSessionToken(student) {
  return jwt.sign(
    { sub: student.id, tenantId: student.tenantId, role: 'student' },
    config.jwtSecret,
    { expiresIn: '8h', issuer: 'supercampus-api', audience: 'supercampus-web' },
  );
}

export async function requireAuth(request, response, next) {
  try {
    const token = request.cookies?.[sessionCookieName];
    if (!token) return response.status(401).json({ error: 'Authentication required' });
    const payload = jwt.verify(token, config.jwtSecret, {
      issuer: 'supercampus-api', audience: 'supercampus-web',
    });
    if (typeof payload !== 'object' || !payload.sub || !payload.tenantId) {
      return response.status(401).json({ error: 'Invalid session' });
    }
    const result = await query(
      `SELECT s.id, s.tenant_id AS "tenantId"
       FROM students s JOIN tenants t ON t.id = s.tenant_id
       WHERE s.id = $1 AND s.tenant_id = $2 AND s.is_active AND t.is_active`,
      [payload.sub, payload.tenantId],
    );
    if (!result.rows[0]) return response.status(401).json({ error: 'Session no longer valid' });
    request.auth = result.rows[0];
    next();
  } catch (error) {
    if (error?.name === 'JsonWebTokenError' || error?.name === 'TokenExpiredError') {
      return response.status(401).json({ error: 'Session expired or invalid' });
    }
    next(error);
  }
}
