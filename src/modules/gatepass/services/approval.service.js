'use strict';

const prisma          = require('../../../lib/prisma');
const qrService       = require('./qr.service');
const notificationSvc = require('./notification.service');

/**
 * Approval Matrix — defines the sequential approver chain per outpass/exit type.
 *
 * Each entry is an ordered array of ApproverRole enum values.
 * The approval engine seeds one ApprovalStep per role, in order.
 */
const OUTPASS_APPROVAL_MATRIX = {
  DAY_OUT:    ['CLASS_ADVISOR', 'WARDEN'],
  HOME_VISIT: ['CLASS_ADVISOR', 'HOD', 'WARDEN', 'PRINCIPAL'],
  MEDICAL:    ['CLASS_ADVISOR', 'WARDEN', 'ADMIN'],
  EMERGENCY:  ['WARDEN', 'ADMIN'],
};

const STAFF_EXIT_MATRIX = {
  TEACHING:     ['HOD', 'PRINCIPAL'],
  NON_TEACHING: ['ADMIN'],
};

/**
 * Resolve the approval chain for a given pass context.
 * Returns an ordered array of ApproverRole strings.
 *
 * @param {string} actorType   - 'STUDENT' | 'STAFF' | 'VISITOR'
 * @param {string} outpassType - Relevant for STUDENT (HOSTELLER) and STAFF exits
 * @param {object} user        - User record (includes studentType, staffType)
 * @returns {string[]}
 */
const resolveApprovalChain = (actorType, outpassType, user) => {
  if (actorType === 'STUDENT') {
    if (user.studentType === 'HOSTELLER' && outpassType) {
      return OUTPASS_APPROVAL_MATRIX[outpassType] || [];
    }
    // DAY_SCHOLAR — no approval required; QR auto-generated
    return [];
  }

  if (actorType === 'STAFF' && outpassType) {
    // Staff early exit requires approval
    return STAFF_EXIT_MATRIX[user.staffType] || [];
  }

  // VISITOR passes are approved by Admin via the approval flow, but
  // visitor walk-ins go through a single-step ADMIN approval
  if (actorType === 'VISITOR') {
    return ['ADMIN'];
  }

  return [];
};

/**
 * Get all passes where the authenticated approver is the current pending step.
 *
 * The "current pending step" is the lowest-stepOrder ApprovalStep with status PENDING.
 * We match approver by role (and department for HOD matching).
 *
 * @param {string} approverId
 * @param {string} approverRole  - Role enum value from User
 * @param {string} department
 */
const getPendingForApprover = async (approverId, approverRole, department) => {
  // Map user Role to ApproverRole
  const roleMap = {
    TEACHER: 'CLASS_ADVISOR', // Default mapping; HOD determined by department
    STAFF:   'WARDEN',
    ADMIN:   'ADMIN',
  };

  // Collect all relevant ApproverRole values this user can fulfil
  const matchingApproverRoles = [];
  if (approverRole === 'TEACHER') {
    matchingApproverRoles.push('CLASS_ADVISOR');
    if (department) matchingApproverRoles.push('HOD');
  }
  if (approverRole === 'STAFF') {
    matchingApproverRoles.push('WARDEN');
    matchingApproverRoles.push('PRINCIPAL');
  }
  if (approverRole === 'ADMIN') {
    matchingApproverRoles.push('ADMIN');
  }

  if (matchingApproverRoles.length === 0) return [];

  // Find the minimum pending stepOrder per pass that matches this approver's roles
  const steps = await prisma.approvalStep.findMany({
    where: {
      approverRole: { in: matchingApproverRoles },
      status: 'PENDING',
      pass: { status: 'PENDING' },
    },
    include: {
      pass: {
        include: { user: { select: { id: true, name: true, rollNumber: true, department: true } } },
      },
    },
    orderBy: { stepOrder: 'asc' },
  });

  // Filter: only return steps that are the *current* (minimum order) pending step for their pass
  const passStepMap = {};
  steps.forEach((step) => {
    const prev = passStepMap[step.passId];
    if (!prev || step.stepOrder < prev.stepOrder) {
      passStepMap[step.passId] = step;
    }
  });

  return Object.values(passStepMap);
};

/**
 * Approve the current pending step for a pass.
 * If this was the last step, the pass becomes APPROVED and a QR is generated.
 */
const approveStep = async ({ passId, approverId, approverRole, department, remarks }) => {
  const currentStep = await _findCurrentStep(passId, approverRole, department);
  if (!currentStep) return null;

  await prisma.approvalStep.update({
    where: { id: currentStep.id },
    data: {
      status: 'APPROVED',
      approverId,
      remarks: remarks || null,
      decidedAt: new Date(),
    },
  });

  // Check if all steps are now approved
  const remainingSteps = await prisma.approvalStep.count({
    where: { passId, status: 'PENDING' },
  });

  let fullyApproved = false;
  let qrToken = null;

  if (remainingSteps === 0) {
    fullyApproved = true;
    await prisma.gatePass.update({ where: { id: passId }, data: { status: 'APPROVED' } });
    const pass = await prisma.gatePass.findUnique({ where: { id: passId }, include: { user: true } });
    qrToken = await qrService.generateQR(passId, pass.userId, pass.actorType, pass.outpassType);

    // Notify the pass owner via WhatsApp
    if (pass.user && pass.user.parentPhone) {
      await notificationSvc.sendWhatsApp({
        to: pass.user.parentPhone,
        templateName: 'pass_approved',
        params: [pass.user.name || 'Student', pass.outpassType || 'pass'],
      });
    }
  }

  const updatedPass = await prisma.gatePass.findUnique({
    where: { id: passId },
    include: { approvalSteps: { orderBy: { stepOrder: 'asc' } }, qrToken: true },
  });

  return { fullyApproved, pass: updatedPass, qrToken };
};

/**
 * Reject a pass — terminates the approval chain immediately.
 */
const rejectStep = async ({ passId, approverId, approverRole, department, remarks }) => {
  const currentStep = await _findCurrentStep(passId, approverRole, department);
  if (!currentStep) return null;

  await prisma.$transaction([
    prisma.approvalStep.update({
      where: { id: currentStep.id },
      data: {
        status: 'REJECTED',
        approverId,
        remarks: remarks || null,
        decidedAt: new Date(),
      },
    }),
    prisma.gatePass.update({
      where: { id: passId },
      data: { status: 'REJECTED' },
    }),
  ]);

  const updatedPass = await prisma.gatePass.findUnique({
    where: { id: passId },
    include: { approvalSteps: { orderBy: { stepOrder: 'asc' } } },
  });

  return { pass: updatedPass };
};

// ─── Internal Helpers ─────────────────────────────────────────────────────────

/**
 * Find the current (lowest-order) pending step for a pass that this approver can act on.
 */
const _findCurrentStep = async (passId, approverRole, department) => {
  const matchingRoles = _mapToApproverRoles(approverRole, department);
  if (matchingRoles.length === 0) return null;

  const steps = await prisma.approvalStep.findMany({
    where: { passId, status: 'PENDING', approverRole: { in: matchingRoles } },
    orderBy: { stepOrder: 'asc' },
  });

  return steps[0] || null;
};

const _mapToApproverRoles = (role, department) => {
  switch (role) {
    case 'TEACHER': return department ? ['CLASS_ADVISOR', 'HOD'] : ['CLASS_ADVISOR'];
    case 'STAFF':   return ['WARDEN', 'PRINCIPAL'];
    case 'ADMIN':   return ['ADMIN'];
    default:        return [];
  }
};

module.exports = {
  resolveApprovalChain,
  getPendingForApprover,
  approveStep,
  rejectStep,
};
