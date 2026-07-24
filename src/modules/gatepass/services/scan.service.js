'use strict';

const prisma          = require('../../../lib/prisma');
const qrService       = require('./qr.service');
const notificationSvc = require('./notification.service');

/**
 * Validate a scanned QR token and create a GateLog entry.
 *
 * Strategy:
 * 1. Verify JWT signature (offline — no DB needed for basic validity).
 * 2. Check QRToken in DB for status, expiry, and usage.
 * 3. Determine ENTRY vs EXIT from last GateLog entry.
 * 4. Log the scan result.
 * 5. If valid EXIT for a HOSTELLER — fire parent notification.
 *
 * @param {{ token: string, type?: string, scannedById: string }} params
 */
const validateAndScan = async ({ token, type, scannedById }) => {
  let decoded;

  // Step 1: Verify JWT signature
  try {
    decoded = qrService.verifyToken(token);
  } catch (jwtErr) {
    return _logAndReturn(null, scannedById, type || 'ENTRY', 'FLAGGED', 'Invalid or tampered QR token');
  }

  const { passId, userId, actorType } = decoded;

  // Step 2: Check DB record
  const qrToken = await prisma.qRToken.findUnique({
    where: { passId },
    include: {
      pass: {
        include: {
          user: true,
          gateLogs: { orderBy: { scannedAt: 'desc' }, take: 1 },
        },
      },
    },
  });

  if (!qrToken) {
    return _logAndReturn(passId, scannedById, type || 'ENTRY', 'FLAGGED', 'Token not found in database');
  }

  if (qrToken.status === 'USED') {
    return _logAndReturn(passId, scannedById, type || 'ENTRY', 'DENIED', 'QR already used');
  }

  if (qrToken.status === 'EXPIRED' || new Date(qrToken.expiresAt) < new Date()) {
    await prisma.qRToken.update({ where: { passId }, data: { status: 'EXPIRED' } });
    return _logAndReturn(passId, scannedById, type || 'ENTRY', 'DENIED', 'QR token expired');
  }

  if (qrToken.pass.status !== 'APPROVED') {
    return _logAndReturn(passId, scannedById, type || 'ENTRY', 'DENIED', `Pass status is ${qrToken.pass.status}`);
  }

  // Step 3: Auto-detect ENTRY/EXIT from last gate log
  const lastLog   = qrToken.pass.gateLogs[0];
  const scanType  = type || (lastLog?.type === 'ENTRY' ? 'EXIT' : 'ENTRY');

  // Step 4: Create GateLog
  const gateLog = await prisma.gateLog.create({
    data: {
      passId,
      scannedById: scannedById || null,
      type: scanType,
      result: 'ALLOWED',
      notes: null,
    },
  });

  // Step 5: Mark token USED after a completed ENTRY+EXIT cycle
  if (scanType === 'EXIT') {
    await prisma.qRToken.update({ where: { passId }, data: { status: 'USED', usedAt: new Date() } });
    await prisma.gatePass.update({ where: { id: passId }, data: { status: 'USED' } });

    // Fire parent notification for Hosteller exits
    if (actorType === 'STUDENT' && qrToken.pass.user?.studentType === 'HOSTELLER') {
      const parentPhone = qrToken.pass.user.parentPhone;
      if (parentPhone) {
        await notificationSvc.sendWhatsApp({
          to: parentPhone,
          templateName: 'left_campus',
          params: [qrToken.pass.user.name || 'Your child'],
        });
      }
    }
  }

  return {
    result: 'ALLOWED',
    scanType,
    passId,
    gateLogId: gateLog.id,
    scannedAt: gateLog.scannedAt,
    message: `${scanType} allowed`,
  };
};

/**
 * List gate logs with optional filters.
 */
const getGateLogs = async ({ page, limit, type, result }) => {
  const skip = (page - 1) * limit;
  const where = {};
  if (type)   where.type   = type;
  if (result) where.result = result;

  const [logs, total] = await Promise.all([
    prisma.gateLog.findMany({
      where,
      include: { pass: { include: { user: { select: { id: true, name: true, role: true } } } } },
      orderBy: { scannedAt: 'desc' },
      skip,
      take: limit,
    }),
    prisma.gateLog.count({ where }),
  ]);

  return { logs, total, page, limit };
};

/**
 * Get a single gate log by ID.
 */
const getGateLogById = async (id) => {
  return prisma.gateLog.findUnique({
    where: { id },
    include: {
      pass: { include: { user: { select: { id: true, name: true, role: true } }, visitorProfile: true } },
      manualOverride: true,
    },
  });
};

// ─── Internal Helpers ─────────────────────────────────────────────────────────

/**
 * Log a denied/flagged scan without creating a valid access event.
 */
const _logAndReturn = async (passId, scannedById, type, result, message) => {
  if (passId) {
    await prisma.gateLog.create({
      data: { passId, scannedById: scannedById || null, type, result, notes: message },
    }).catch(() => {}); // Non-fatal — don't throw if passId is invalid
  }
  return { result, message, passId };
};

module.exports = { validateAndScan, getGateLogs, getGateLogById };
