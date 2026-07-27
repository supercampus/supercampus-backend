import * as passService from '../services/pass.service.js';
import { validateCreatePass, validatePassId } from '../validators/pass.validator.js';

/**
 * POST /api/gatepass/passes
 * Submit a new gate pass request.
 */
export const createPass = async (req, res) => {
  const { valid, errors } = validateCreatePass(req.body);
  if (!valid) {
    return res.status(400).json({ error: 'Validation failed', details: errors });
  }

  try {
    const pass = await passService.createPass(req.user.id, req.body);
    return res.status(201).json({ message: 'Pass request submitted successfully', pass });
  } catch (err) {
    console.error('[PassController.createPass]', err);
    return res.status(500).json({ error: 'Failed to create pass request' });
  }
};

/**
 * GET /api/gatepass/passes
 * List passes — filtered by role (own passes for students/staff, all for admin).
 */
export const listPasses = async (req, res) => {
  const { status, actorType, page = 1, limit = 20 } = req.query;
  try {
    const passes = await passService.listPasses({
      requesterId: req.user.id,
      requesterRole: req.user.role,
      status,
      actorType,
      page: parseInt(page, 10),
      limit: parseInt(limit, 10),
    });
    return res.status(200).json(passes);
  } catch (err) {
    console.error('[PassController.listPasses]', err);
    return res.status(500).json({ error: 'Failed to fetch passes' });
  }
};

/**
 * GET /api/gatepass/passes/:id
 * Get a single pass with its full approval chain.
 */
export const getPass = async (req, res) => {
  const { id } = req.params;
  if (!validatePassId(id)) {
    return res.status(400).json({ error: 'Invalid pass ID format' });
  }

  try {
    const pass = await passService.getPassById(id, req.user.id, req.user.role);
    if (!pass) return res.status(404).json({ error: 'Pass not found' });
    return res.status(200).json(pass);
  } catch (err) {
    console.error('[PassController.getPass]', err);
    return res.status(500).json({ error: 'Failed to fetch pass' });
  }
};

/**
 * DELETE /api/gatepass/passes/:id
 * Cancel a pending pass (owner only).
 */
export const cancelPass = async (req, res) => {
  const { id } = req.params;
  if (!validatePassId(id)) {
    return res.status(400).json({ error: 'Invalid pass ID format' });
  }

  try {
    const result = await passService.cancelPass(id, req.user.id);
    if (result === null) return res.status(404).json({ error: 'Pass not found' });
    if (result === false) {
      return res.status(403).json({ error: 'Only the pass owner can cancel, and only while it is PENDING' });
    }
    return res.status(200).json({ message: 'Pass cancelled successfully' });
  } catch (err) {
    console.error('[PassController.cancelPass]', err);
    return res.status(500).json({ error: 'Failed to cancel pass' });
  }
};
