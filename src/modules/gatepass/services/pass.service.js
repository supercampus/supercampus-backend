import prisma from '../../../lib/prisma.js';
import * as approvalService from './approval.service.js';
import * as qrService from './qr.service.js';

/**
 * Create a new gate pass and seed its approval steps.
 *
 * @param {string} userId - The requesting user's ID.
 * @param {object} data   - Validated request body.
 */
export const createPass = async (userId, data) => {
  const {
    actorType,
    outpassType,
    fromTime,
    backTime,
    purpose,
    remarks,
    // Visitor fields
    visitorName,
    visitorType,
    phone,
    whomToMeet,
    photoUrl,
  } = data;

  const pass = await prisma.$transaction(async (tx) => {
    // 1. Create the core GatePass record
    const newPass = await tx.gatePass.create({
      data: {
        userId,
        actorType,
        outpassType: outpassType || null,
        status: 'PENDING',
        fromTime: fromTime ? new Date(fromTime) : null,
        backTime: backTime ? new Date(backTime) : null,
        purpose: purpose || null,
        remarks: remarks || null,
      },
    });

    // 2. Create VisitorProfile if actor is VISITOR
    if (actorType === 'VISITOR') {
      await tx.visitorProfile.create({
        data: {
          passId: newPass.id,
          visitorName,
          visitorType,
          phone: phone || null,
          purpose: purpose || null,
          whomToMeet: whomToMeet || null,
          photoUrl: photoUrl || null,
        },
      });
    }

    // 3. Seed approval steps based on actor type and outpass type
    const user = await tx.user.findUnique({ where: { id: userId } });
    const steps = approvalService.resolveApprovalChain(actorType, outpassType, user);

    for (let i = 0; i < steps.length; i++) {
      await tx.approvalStep.create({
        data: {
          passId: newPass.id,
          approverRole: steps[i],
          stepOrder: i + 1,
          status: 'PENDING',
        },
      });
    }

    // 4. If no approval steps needed (Day Scholar / Staff regular exit),
    //    auto-approve and generate QR immediately.
    if (steps.length === 0) {
      await tx.gatePass.update({
        where: { id: newPass.id },
        data: { status: 'APPROVED' },
      });
    }

    return newPass;
  });

  // Auto-generate QR for passes that need no approval
  const finalPass = await prisma.gatePass.findUnique({ where: { id: pass.id } });
  if (finalPass.status === 'APPROVED') {
    await qrService.generateQR(pass.id, userId, actorType);
  }

  return prisma.gatePass.findUnique({
    where: { id: pass.id },
    include: { approvalSteps: { orderBy: { stepOrder: 'asc' } }, qrToken: true, visitorProfile: true },
  });
};

/**
 * List passes — scoped by role.
 * Admin / Security see all; Students / Staff see only their own.
 */
export const listPasses = async ({ requesterId, requesterRole, status, actorType, page, limit }) => {
  const skip = (page - 1) * limit;
  const where = {};

  if (!['ADMIN', 'SECURITY'].includes(requesterRole)) {
    where.userId = requesterId;
  }
  if (status)    where.status    = status;
  if (actorType) where.actorType = actorType;

  const [passes, total] = await Promise.all([
    prisma.gatePass.findMany({
      where,
      include: { approvalSteps: { orderBy: { stepOrder: 'asc' } }, qrToken: true, visitorProfile: true },
      orderBy: { createdAt: 'desc' },
      skip,
      take: limit,
    }),
    prisma.gatePass.count({ where }),
  ]);

  return { passes, total, page, limit };
};

/**
 * Get a single pass with its full approval chain.
 * Non-admin users can only see their own passes.
 */
export const getPassById = async (passId, requesterId, requesterRole) => {
  const pass = await prisma.gatePass.findUnique({
    where: { id: passId },
    include: {
      approvalSteps: { orderBy: { stepOrder: 'asc' }, include: { approver: { select: { id: true, name: true, role: true } } } },
      qrToken: true,
      visitorProfile: true,
      gateLogs: { orderBy: { scannedAt: 'desc' } },
    },
  });

  if (!pass) return null;

  // Non-admin users may only read their own passes
  if (!['ADMIN', 'SECURITY'].includes(requesterRole) && pass.userId !== requesterId) {
    return null;
  }

  return pass;
};

/**
 * Cancel a pending pass (owner only).
 * Returns null if not found, false if not cancellable, true on success.
 */
export const cancelPass = async (passId, userId) => {
  const pass = await prisma.gatePass.findUnique({ where: { id: passId } });
  if (!pass) return null;
  if (pass.userId !== userId || pass.status !== 'PENDING') return false;

  await prisma.gatePass.update({
    where: { id: passId },
    data: { status: 'REJECTED' },
  });

  return true;
};
