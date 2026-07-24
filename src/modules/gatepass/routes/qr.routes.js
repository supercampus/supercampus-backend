'use strict';

const express    = require('express');
const router     = express.Router();
const ctrl       = require('../controllers/qr.controller');
const { requireRoles, requireAdmin } = require('../middleware/gatepass.auth');

// GET  /api/gatepass/qr/:passId            — get QR image / token for a pass
router.get('/:passId', requireRoles(['STUDENT', 'STAFF', 'SECURITY', 'ADMIN']), ctrl.getQR);

// POST /api/gatepass/qr/:passId/regenerate — force regenerate QR (admin only)
router.post('/:passId/regenerate', requireAdmin, ctrl.regenerateQR);

module.exports = router;
