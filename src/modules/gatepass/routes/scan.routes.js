'use strict';

const express    = require('express');
const router     = express.Router();
const ctrl       = require('../controllers/scan.controller');
const { requireSecurity, requireAdmin } = require('../middleware/gatepass.auth');

// POST /api/gatepass/scan/validate  — scan & validate a QR, log entry/exit
router.post('/validate', requireSecurity, ctrl.validateQR);

// GET  /api/gatepass/scan/logs      — paginated gate log list
router.get('/logs', requireSecurity, ctrl.getGateLogs);

// GET  /api/gatepass/scan/logs/:id  — single gate log detail
router.get('/logs/:id', requireAdmin, ctrl.getGateLogById);

module.exports = router;
