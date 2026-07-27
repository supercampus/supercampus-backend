import * as qrService from '../services/qr.service.js';

/**
 * GET /api/gatepass/qr/:passId
 * Returns the QR token (and data URL) for an approved pass.
 */
export const getQR = async (req, res) => {
  const { passId } = req.params;
  try {
    const qr = await qrService.getQRByPassId(passId, req.user.id, req.user.role);
    if (!qr) return res.status(404).json({ error: 'QR not found or pass not yet approved' });
    return res.status(200).json(qr);
  } catch (err) {
    console.error('[QRController.getQR]', err);
    return res.status(500).json({ error: 'Failed to retrieve QR' });
  }
};

/**
 * POST /api/gatepass/qr/:passId/regenerate
 * Admin-only: force regenerate a QR token (e.g. after expiry).
 */
export const regenerateQR = async (req, res) => {
  const { passId } = req.params;
  try {
    const qr = await qrService.regenerateQR(passId);
    if (!qr) return res.status(404).json({ error: 'Pass not found' });
    return res.status(200).json({ message: 'QR regenerated successfully', qr });
  } catch (err) {
    console.error('[QRController.regenerateQR]', err);
    return res.status(500).json({ error: 'Failed to regenerate QR' });
  }
};
