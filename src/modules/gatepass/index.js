import express from 'express';
import passRoutes from './routes/pass.routes.js';
import approvalRoutes from './routes/approval.routes.js';
import qrRoutes from './routes/qr.routes.js';
import scanRoutes from './routes/scan.routes.js';
import geofenceRoutes from './routes/geofence.routes.js';
import overrideRoutes from './routes/override.routes.js';

const router = express.Router();

router.use('/passes',   passRoutes);
router.use('/approvals', approvalRoutes);
router.use('/qr',       qrRoutes);
router.use('/scan',     scanRoutes);
router.use('/geofence', geofenceRoutes);
router.use('/override', overrideRoutes);

export default router;
