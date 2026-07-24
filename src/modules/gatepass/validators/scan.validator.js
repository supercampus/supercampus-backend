'use strict';

/**
 * Scan input validator.
 */

/**
 * Validate QR scan payload.
 * @param {object} body - Expected: { token: string, type?: 'ENTRY'|'EXIT' }
 * @returns {{ valid: boolean, errors: string[] }}
 */
const validateScan = (body) => {
  const errors = [];
  const validTypes = ['ENTRY', 'EXIT'];

  if (!body.token || typeof body.token !== 'string' || body.token.trim().length === 0) {
    errors.push('token is required and must be a non-empty string');
  }

  if (body.type && !validTypes.includes(body.type)) {
    errors.push(`type must be one of: ${validTypes.join(', ')}`);
  }

  return { valid: errors.length === 0, errors };
};

module.exports = { validateScan };
