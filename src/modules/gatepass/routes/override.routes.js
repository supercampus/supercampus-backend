'use strict';

const express    = require('express');
const router     = express.Router();
const ctrl       = require('../controllers/override.controller');
const { requireSecurity, requireAdmin } = require('../middleware/gatepass.auth');

// POST  /api/gatepass/override          — log manual entry/exit (system down)
router.post('/', requireSecurity, ctrl.createOverride);

// GET   /api/gatepass/override          — list unreviewed overrides (admin)
router.get('/', requireAdmin, ctrl.listOverrides);

// PATCH /api/gatepass/override/:id/review — mark override as reviewed (admin)
router.patch('/:id/review', requireAdmin, ctrl.reviewOverride);

module.exports = router;
