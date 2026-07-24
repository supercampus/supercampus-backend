'use strict';

const approvalService = require('../services/approval.service');

/**
 * GET /api/gatepass/approvals/pending
 * Returns all passes where the authenticated approver is the current pending step.
 */
const getPendingApprovals = async (req, res) => {
  try {
    const pending = await approvalService.getPendingForApprover(
      req.user.id,
      req.user.role,
      req.user.department,
    );
    return res.status(200).json(pending);
  } catch (err) {
    console.error('[ApprovalController.getPendingApprovals]', err);
    return res.status(500).json({ error: 'Failed to fetch pending approvals' });
  }
};

/**
 * POST /api/gatepass/approvals/:passId/approve
 * Approve the current step for a pass.
 * Body: { remarks?: string }
 */
const approveStep = async (req, res) => {
  const { passId } = req.params;
  const { remarks } = req.body;

  try {
    const result = await approvalService.approveStep({
      passId,
      approverId: req.user.id,
      approverRole: req.user.role,
      department: req.user.department,
      remarks,
    });

    if (!result) return res.status(404).json({ error: 'Pass not found or no pending step for you' });

    return res.status(200).json({
      message: result.fullyApproved
        ? 'Pass fully approved — QR generated'
        : 'Step approved — forwarded to next approver',
      pass: result.pass,
    });
  } catch (err) {
    console.error('[ApprovalController.approveStep]', err);
    return res.status(500).json({ error: 'Failed to approve step' });
  }
};

/**
 * POST /api/gatepass/approvals/:passId/reject
 * Reject a pass — terminates the entire chain immediately.
 * Body: { remarks?: string }
 */
const rejectStep = async (req, res) => {
  const { passId } = req.params;
  const { remarks } = req.body;

  try {
    const result = await approvalService.rejectStep({
      passId,
      approverId: req.user.id,
      approverRole: req.user.role,
      department: req.user.department,
      remarks,
    });

    if (!result) return res.status(404).json({ error: 'Pass not found or no pending step for you' });

    return res.status(200).json({ message: 'Pass rejected', pass: result.pass });
  } catch (err) {
    console.error('[ApprovalController.rejectStep]', err);
    return res.status(500).json({ error: 'Failed to reject step' });
  }
};

module.exports = { getPendingApprovals, approveStep, rejectStep };
