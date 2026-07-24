'use strict';

const prisma = require('../../../lib/prisma');

/**
 * Create a manual override log entry (system-down fallback).
 *
 * @param {{ recordedById, actorDesc, reason, photoUrl }} params
 */
const createOverride = async ({ recordedById, actorDesc, reason, photoUrl }) => {
  return prisma.manualOverride.create({
    data: {
      recordedById,
      actorDesc,
      reason: reason || 'SYSTEM_DOWN',
      photoUrl: photoUrl || null,
      isReviewed: false,
    },
    include: {
      recorder: { select: { id: true, name: true, role: true } },
    },
  });
};

/**
 * List manual overrides with optional reviewed filter.
 *
 * @param {{ page, limit, isReviewed?: boolean }} params
 */
const listOverrides = async ({ page, limit, isReviewed }) => {
  const skip = (page - 1) * limit;
  const where = {};
  if (isReviewed !== undefined) where.isReviewed = isReviewed;

  const [overrides, total] = await Promise.all([
    prisma.manualOverride.findMany({
      where,
      include: {
        recorder: { select: { id: true, name: true, role: true } },
        reviewer: { select: { id: true, name: true, role: true } },
        gateLog:  true,
      },
      orderBy: { createdAt: 'desc' },
      skip,
      take: limit,
    }),
    prisma.manualOverride.count({ where }),
  ]);

  return { overrides, total, page, limit };
};

/**
 * Mark a manual override as reviewed by an admin.
 *
 * @param {string} id          - Override ID
 * @param {string} reviewedById - Admin user ID
 */
const reviewOverride = async (id, reviewedById) => {
  const existing = await prisma.manualOverride.findUnique({ where: { id } });
  if (!existing) return null;

  return prisma.manualOverride.update({
    where: { id },
    data: {
      isReviewed:  true,
      reviewedAt:  new Date(),
      reviewedById,
    },
    include: {
      recorder: { select: { id: true, name: true } },
      reviewer: { select: { id: true, name: true } },
    },
  });
};

module.exports = { createOverride, listOverrides, reviewOverride };
