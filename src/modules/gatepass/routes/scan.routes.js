import express from 'express';
import * as ctrl from '../controllers/scan.controller.js';
import { requireSecurity, requireAdmin } from '../middleware/gatepass.auth.js';

const router = express.Router();

// POST /api/gatepass/scan/validate  — scan & validate a QR, log entry/exit
router.post('/validate', requireSecurity, ctrl.validateQR);

// GET  /api/gatepass/scan/logs      — paginated gate log list
router.get('/logs', requireSecurity, ctrl.getGateLogs);

// GET  /api/gatepass/scan/logs/:id  — single gate log detail
router.get('/logs/:id', requireAdmin, ctrl.getGateLogById);

export default router;
