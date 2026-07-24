'use strict';

const jwt    = require('jsonwebtoken');
const prisma = require('../../../lib/prisma');

/**
 * TTL (in minutes) per pass / actor type.
 */
const TTL_MINUTES = {
  DAY_SCHOLAR: 30,
  HOSTELLER:   480,  // 8 hours (departure window)
  STAFF:       600,  // 10 hours (working day)
  VISITOR:     120,  // 2 hours default; overridden at approval time
};

const GATEPASS_SECRET = process.env.GATEPASS_SECRET || 'gatepass-secret-change-in-prod';

/**
 * Generate a JWT-based QR token for an approved pass.
 * Stores it in the QRToken table for offline validation.
 *
 * @param {string} passId
 * @param {string} userId
 * @param {string} actorType    - 'STUDENT' | 'STAFF' | 'VISITOR'
 * @param {string} [outpassType]
 * @param {number} [customTtlMinutes] - Override TTL (e.g. for walk-in visitors)
 */
const generateQR = async (passId, userId, actorType, outpassType, customTtlMinutes) => {
  // Resolve TTL
  const ttl = customTtlMinutes || _resolveTTL(actorType, outpassType);
  const expiresAt = new Date(Date.now() + ttl * 60 * 1000);

  // Build JWT payload
  const payload = {
    passId,
    userId,
    actorType,
    outpassType: outpassType || null,
    type: 'GATEPASS',
  };

  const token = jwt.sign(payload, GATEPASS_SECRET, {
    expiresIn: `${ttl}m`,
    issuer: 'supercampus',
  });

  // Upsert in DB — regenerate replaces the old token
  const qrToken = await prisma.qRToken.upsert({
    where: { passId },
    create: { passId, token, expiresAt, status: 'APPROVED' },
    update: { token, expiresAt, usedAt: null, status: 'APPROVED' },
  });

  return { qrToken, token, expiresAt };
};

/**
 * Retrieve the QR token for a pass.
 * Non-admin users may only retrieve tokens for their own passes.
 *
 * @param {string} passId
 * @param {string} requesterId
 * @param {string} requesterRole
 */
const getQRByPassId = async (passId, requesterId, requesterRole) => {
  const pass = await prisma.gatePass.findUnique({
    where: { id: passId },
    include: { qrToken: true },
  });

  if (!pass || !pass.qrToken) return null;

  // Ownership check for non-privileged roles
  if (!['ADMIN', 'SECURITY'].includes(requesterRole) && pass.userId !== requesterId) {
    return null;
  }

  // Check token expiry
  if (new Date(pass.qrToken.expiresAt) < new Date()) {
    await prisma.qRToken.update({
      where: { passId },
      data: { status: 'EXPIRED' },
    });
    await prisma.gatePass.update({
      where: { id: passId },
      data: { status: 'EXPIRED' },
    });
    return { expired: true, message: 'QR token has expired' };
  }

  return pass.qrToken;
};

/**
 * Admin: force regenerate a QR token (e.g. after expiry).
 */
const regenerateQR = async (passId) => {
  const pass = await prisma.gatePass.findUnique({ where: { id: passId } });
  if (!pass) return null;

  return generateQR(passId, pass.userId, pass.actorType, pass.outpassType);
};

/**
 * Verify a JWT token offline (no DB call needed).
 * Returns the decoded payload or throws.
 *
 * @param {string} token
 */
const verifyToken = (token) => {
  return jwt.verify(token, GATEPASS_SECRET, { issuer: 'supercampus' });
};

// ─── Internal Helpers ─────────────────────────────────────────────────────────

const _resolveTTL = (actorType, outpassType) => {
  if (actorType === 'STUDENT') return TTL_MINUTES.DAY_SCHOLAR;
  if (actorType === 'STAFF')   return TTL_MINUTES.STAFF;
  if (actorType === 'VISITOR') return TTL_MINUTES.VISITOR;
  return 60;
};

module.exports = { generateQR, getQRByPassId, regenerateQR, verifyToken };
