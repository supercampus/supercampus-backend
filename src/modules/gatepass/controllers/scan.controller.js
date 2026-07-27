import * as scanService from '../services/scan.service.js';
import { validateScan } from '../validators/scan.validator.js';

/**
 * POST /api/gatepass/scan/validate
 * Security device submits a scanned QR token.
 * Body: { token: string, type?: 'ENTRY' | 'EXIT' }
 */
export const validateQR = async (req, res) => {
  const { valid, errors } = validateScan(req.body);
  if (!valid) {
    return res.status(400).json({ error: 'Validation failed', details: errors });
  }

  try {
    const result = await scanService.validateAndScan({
      token: req.body.token,
      type: req.body.type,
      scannedById: req.user.id,
    });

    const status = result.result === 'ALLOWED' ? 200 : 403;
    return res.status(status).json(result);
  } catch (err) {
    console.error('[ScanController.validateQR]', err);
    return res.status(500).json({ error: 'Scan validation failed' });
  }
};

/**
 * GET /api/gatepass/scan/logs
 * Paginated gate log listing for security / admin.
 * Query: ?page=1&limit=20&type=ENTRY|EXIT&result=ALLOWED|DENIED|FLAGGED
 */
export const getGateLogs = async (req, res) => {
  const { page = 1, limit = 20, type, result } = req.query;
  try {
    const logs = await scanService.getGateLogs({
      page: parseInt(page, 10),
      limit: parseInt(limit, 10),
      type,
      result,
    });
    return res.status(200).json(logs);
  } catch (err) {
    console.error('[ScanController.getGateLogs]', err);
    return res.status(500).json({ error: 'Failed to fetch gate logs' });
  }
};

/**
 * GET /api/gatepass/scan/logs/:id
 * Single gate log detail (admin only).
 */
export const getGateLogById = async (req, res) => {
  const { id } = req.params;
  try {
    const log = await scanService.getGateLogById(id);
    if (!log) return res.status(404).json({ error: 'Gate log not found' });
    return res.status(200).json(log);
  } catch (err) {
    console.error('[ScanController.getGateLogById]', err);
    return res.status(500).json({ error: 'Failed to fetch gate log' });
  }
};
