import * as geofenceService from '../services/geofence.service.js';
import { validateGeofenceEvent } from '../validators/geofence.validator.js';

/**
 * POST /api/gatepass/geofence/entry
 * Mobile app posts this when a student/staff enters the campus geofence boundary.
 *
 * - DAY_SCHOLAR → creates a 30-min QR, returns it to the app for display at gate.
 * - HOSTELLER / STAFF → logs the geofence event; no QR needed here.
 */
export const handleEntry = async (req, res) => {
  const payload = { ...req.body, userId: req.user.id };
  const { valid, errors } = validateGeofenceEvent(payload);
  if (!valid) {
    return res.status(400).json({ error: 'Validation failed', details: errors });
  }

  try {
    const result = await geofenceService.handleEntry({
      userId: req.user.id,
      studentType: req.user.studentType,
      latitude: req.body.latitude,
      longitude: req.body.longitude,
    });
    return res.status(200).json(result);
  } catch (err) {
    console.error('[GeofenceController.handleEntry]', err);
    return res.status(500).json({ error: 'Failed to process geofence entry event' });
  }
};

/**
 * POST /api/gatepass/geofence/exit
 * Mobile app posts this when a student/staff exits the campus geofence boundary.
 *
 * - HOSTELLER with an approved outpass → sends WhatsApp notification to parent.
 * - Others → logs event only.
 */
export const handleExit = async (req, res) => {
  const payload = { ...req.body, userId: req.user.id };
  const { valid, errors } = validateGeofenceEvent(payload);
  if (!valid) {
    return res.status(400).json({ error: 'Validation failed', details: errors });
  }

  try {
    const result = await geofenceService.handleExit({
      userId: req.user.id,
      studentType: req.user.studentType,
      parentPhone: req.user.parentPhone,
      latitude: req.body.latitude,
      longitude: req.body.longitude,
    });
    return res.status(200).json(result);
  } catch (err) {
    console.error('[GeofenceController.handleExit]', err);
    return res.status(500).json({ error: 'Failed to process geofence exit event' });
  }
};
