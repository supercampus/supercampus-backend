import express from 'express';
import * as ctrl from '../controllers/qr.controller.js';
import { requireRoles, requireAdmin } from '../middleware/gatepass.auth.js';

const router = express.Router();

// GET  /api/gatepass/qr/:passId            — get QR image / token for a pass
router.get('/:passId', requireRoles(['STUDENT', 'STAFF', 'SECURITY', 'ADMIN']), ctrl.getQR);

// POST /api/gatepass/qr/:passId/regenerate — force regenerate QR (admin only)
router.post('/:passId/regenerate', requireAdmin, ctrl.regenerateQR);

export default router;
