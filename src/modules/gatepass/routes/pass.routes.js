import express from 'express';
import * as ctrl from '../controllers/pass.controller.js';
import { requireRoles } from '../middleware/gatepass.auth.js';

const router = express.Router();

// POST   /api/gatepass/passes          — submit a new pass request
router.post('/', requireRoles(['STUDENT', 'STAFF', 'SECURITY']), ctrl.createPass);

// GET    /api/gatepass/passes          — list own passes (role-filtered)
router.get('/', requireRoles(['STUDENT', 'STAFF', 'ADMIN', 'SECURITY']), ctrl.listPasses);

// GET    /api/gatepass/passes/:id      — get single pass with approval chain
router.get('/:id', requireRoles(['STUDENT', 'STAFF', 'ADMIN', 'SECURITY']), ctrl.getPass);

// DELETE /api/gatepass/passes/:id      — cancel a pending pass (owner only)
router.delete('/:id', requireRoles(['STUDENT', 'STAFF']), ctrl.cancelPass);

export default router;
