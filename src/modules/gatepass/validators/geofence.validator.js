/**
 * Geofence event validator.
 * Mobile app sends lat/lng when the student crosses the campus boundary.
 */

/**
 * Validate geofence entry/exit payload.
 * @param {object} body - Expected: { userId: string, latitude?: number, longitude?: number }
 * @returns {{ valid: boolean, errors: string[] }}
 */
export const validateGeofenceEvent = (body) => {
  const errors = [];
  const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

  if (!body.userId || !uuidRegex.test(body.userId)) {
    errors.push('userId must be a valid UUID');
  }

  if (body.latitude !== undefined && typeof body.latitude !== 'number') {
    errors.push('latitude must be a number');
  }

  if (body.longitude !== undefined && typeof body.longitude !== 'number') {
    errors.push('longitude must be a number');
  }

  return { valid: errors.length === 0, errors };
};
