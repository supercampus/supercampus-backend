import express from 'express';
import * as ctrl from '../controllers/approval.controller.js';
import { requireApprover } from '../middleware/gatepass.auth.js';

const router = express.Router();

// GET  /api/gatepass/approvals/pending          — list passes awaiting this approver
router.get('/pending', requireApprover, ctrl.getPendingApprovals);

// POST /api/gatepass/approvals/:passId/approve  — approve current step
router.post('/:passId/approve', requireApprover, ctrl.approveStep);

// POST /api/gatepass/approvals/:passId/reject   — reject pass (terminates chain)
router.post('/:passId/reject', requireApprover, ctrl.rejectStep);

export default router;
