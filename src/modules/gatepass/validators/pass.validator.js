'use strict';

/**
 * Pass input validators.
 * Plain validation helpers — swap for Joi/Zod when added as a dependency.
 */

/**
 * Validate pass creation payload.
 * @param {object} body
 * @returns {{ valid: boolean, errors: string[] }}
 */
const validateCreatePass = (body) => {
  const errors = [];
  const validActorTypes   = ['STUDENT', 'STAFF', 'VISITOR'];
  const validOutpassTypes = ['DAY_OUT', 'HOME_VISIT', 'MEDICAL', 'EMERGENCY'];
  const validVisitorTypes = ['INVITED', 'WALK_IN'];

  if (!body.actorType || !validActorTypes.includes(body.actorType)) {
    errors.push(`actorType must be one of: ${validActorTypes.join(', ')}`);
  }

  if (body.actorType === 'STUDENT' && body.studentType === 'HOSTELLER') {
    if (!body.outpassType || !validOutpassTypes.includes(body.outpassType)) {
      errors.push(`outpassType must be one of: ${validOutpassTypes.join(', ')} for hosteller outpass`);
    }
    if (!body.fromTime) errors.push('fromTime is required for hosteller outpass');
    if (!body.backTime) errors.push('backTime is required for hosteller outpass');
  }

  if (body.actorType === 'VISITOR') {
    if (!body.visitorName) errors.push('visitorName is required for visitor passes');
    if (!body.visitorType || !validVisitorTypes.includes(body.visitorType)) {
      errors.push(`visitorType must be one of: ${validVisitorTypes.join(', ')}`);
    }
    if (!body.purpose) errors.push('purpose is required for visitor passes');
    if (!body.whomToMeet) errors.push('whomToMeet is required for visitor passes');
  }

  return { valid: errors.length === 0, errors };
};

/**
 * Validate cancel / delete payload — only needs a valid UUID passId (from params).
 */
const validatePassId = (id) => {
  const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  return uuidRegex.test(id);
};

module.exports = { validateCreatePass, validatePassId };
