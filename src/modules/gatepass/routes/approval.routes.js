'use strict';

const express    = require('express');
const router     = express.Router();
const ctrl       = require('../controllers/approval.controller');
const { requireApprover } = require('../middleware/gatepass.auth');

// GET  /api/gatepass/approvals/pending          — list passes awaiting this approver
router.get('/pending', requireApprover, ctrl.getPendingApprovals);

// POST /api/gatepass/approvals/:passId/approve  — approve current step
router.post('/:passId/approve', requireApprover, ctrl.approveStep);

// POST /api/gatepass/approvals/:passId/reject   — reject pass (terminates chain)
router.post('/:passId/reject', requireApprover, ctrl.rejectStep);

module.exports = router;
