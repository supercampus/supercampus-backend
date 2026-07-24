'use strict';

/**
 * Gatepass Role-Guard Middleware
 *
 * Usage:
 *   router.post('/', requireRoles(['STUDENT', 'STAFF']), controller.create);
 *
 * Expects req.user to be populated by the upstream auth middleware with at
 * least { id, role }.  If auth middleware is not yet wired, attach a stub user
 * or use the BYPASS_AUTH env flag during development only.
 */

/**
 * Returns a middleware that allows only the specified roles.
 * @param {string[]} roles - Array of allowed Role enum values.
 */
const requireRoles = (roles) => (req, res, next) => {
  const user = req.user;

  if (!user) {
    return res.status(401).json({ error: 'Unauthorized — no authenticated user found.' });
  }

  if (!roles.includes(user.role)) {
    return res.status(403).json({
      error: `Forbidden — required role(s): ${roles.join(', ')}. Your role: ${user.role}.`,
    });
  }

  return next();
};

/**
 * Convenience guards for common role groups.
 */
const requireAdmin    = requireRoles(['ADMIN']);
const requireSecurity = requireRoles(['SECURITY', 'ADMIN']);
const requireApprover = requireRoles(['TEACHER', 'STAFF', 'ADMIN']); // HOD / Warden / Principal etc.
const requireStudent  = requireRoles(['STUDENT']);
const requireStaff    = requireRoles(['STAFF', 'TEACHER']);

module.exports = {
  requireRoles,
  requireAdmin,
  requireSecurity,
  requireApprover,
  requireStudent,
  requireStaff,
};
