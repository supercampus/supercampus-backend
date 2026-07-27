import * as overrideService from '../services/override.service.js';

/**
 * POST /api/gatepass/override
 * Log a manual entry/exit when the QR system is down.
 * Body: { actorDesc: string, reason?: 'SYSTEM_DOWN'|'OTHER', photoUrl?: string }
 */
export const createOverride = async (req, res) => {
  const { actorDesc, reason, photoUrl } = req.body;
  if (!actorDesc || actorDesc.trim().length === 0) {
    return res.status(400).json({ error: 'actorDesc is required' });
  }

  try {
    const override = await overrideService.createOverride({
      recordedById: req.user.id,
      actorDesc: actorDesc.trim(),
      reason: reason || 'SYSTEM_DOWN',
      photoUrl: photoUrl || null,
    });
    return res.status(201).json({ message: 'Manual override logged', override });
  } catch (err) {
    console.error('[OverrideController.createOverride]', err);
    return res.status(500).json({ error: 'Failed to log manual override' });
  }
};

/**
 * GET /api/gatepass/override
 * List overrides pending admin review.
 * Query: ?page=1&limit=20&isReviewed=false
 */
export const listOverrides = async (req, res) => {
  const { page = 1, limit = 20, isReviewed } = req.query;
  try {
    const overrides = await overrideService.listOverrides({
      page: parseInt(page, 10),
      limit: parseInt(limit, 10),
      isReviewed: isReviewed === 'true' ? true : isReviewed === 'false' ? false : undefined,
    });
    return res.status(200).json(overrides);
  } catch (err) {
    console.error('[OverrideController.listOverrides]', err);
    return res.status(500).json({ error: 'Failed to fetch overrides' });
  }
};

/**
 * PATCH /api/gatepass/override/:id/review
 * Admin marks an override as reviewed.
 */
export const reviewOverride = async (req, res) => {
  const { id } = req.params;
  try {
    const override = await overrideService.reviewOverride(id, req.user.id);
    if (!override) return res.status(404).json({ error: 'Override not found' });
    return res.status(200).json({ message: 'Override marked as reviewed', override });
  } catch (err) {
    console.error('[OverrideController.reviewOverride]', err);
    return res.status(500).json({ error: 'Failed to review override' });
  }
};
