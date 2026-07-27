import express from 'express';
import * as ctrl from '../controllers/override.controller.js';
import { requireSecurity, requireAdmin } from '../middleware/gatepass.auth.js';

const router = express.Router();

// POST  /api/gatepass/override          — log manual entry/exit (system down)
router.post('/', requireSecurity, ctrl.createOverride);

// GET   /api/gatepass/override          — list unreviewed overrides (admin)
router.get('/', requireAdmin, ctrl.listOverrides);

// PATCH /api/gatepass/override/:id/review — mark override as reviewed (admin)
router.patch('/:id/review', requireAdmin, ctrl.reviewOverride);

export default router;
