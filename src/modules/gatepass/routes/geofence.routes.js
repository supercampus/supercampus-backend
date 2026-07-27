import express from 'express';
import * as ctrl from '../controllers/geofence.controller.js';
import { requireRoles } from '../middleware/gatepass.auth.js';

const router = express.Router();

/**
 * Geofence Events — called by the mobile app when a student/staff
 * crosses the campus boundary polygon.
 *
 * POST /api/gatepass/geofence/entry
 *   → For DAY_SCHOLAR: auto-generates a short-TTL QR and returns it.
 *   → For HOSTELLER:   no QR generated (they need an approved outpass).
 *
 * POST /api/gatepass/geofence/exit
 *   → For HOSTELLER with an APPROVED outpass: triggers WhatsApp
 *     notification to parents ("Left Campus").
 *   → For others: logs the event only.
 */

// POST /api/gatepass/geofence/entry
router.post('/entry', requireRoles(['STUDENT', 'STAFF']), ctrl.handleEntry);

// POST /api/gatepass/geofence/exit
router.post('/exit', requireRoles(['STUDENT', 'STAFF']), ctrl.handleExit);

export default router;
