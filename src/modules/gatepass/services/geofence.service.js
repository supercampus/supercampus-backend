import prisma from '../../../lib/prisma.js';
import * as qrService from './qr.service.js';
import * as notificationSvc from './notification.service.js';

/**
 * Handle a campus geofence ENTRY event from the mobile app.
 *
 * - DAY_SCHOLAR → Creates a GatePass + QRToken (30-min TTL) and returns the token.
 * - HOSTELLER / STAFF → Records the geofence event only (no auto-QR).
 *
 * @param {{ userId, studentType, latitude, longitude }} params
 */
export const handleEntry = async ({ userId, studentType, latitude, longitude }) => {
  // Log the raw geofence event
  await prisma.geofenceEvent.create({
    data: {
      userId,
      type: 'ENTRY',
      latitude: latitude || null,
      longitude: longitude || null,
    },
  });

  // Only Day Scholars get an auto-generated QR on campus entry
  if (studentType !== 'DAY_SCHOLAR') {
    return { message: 'Geofence entry logged', qr: null };
  }

  // Create a new GatePass for this entry
  const pass = await prisma.gatePass.create({
    data: {
      userId,
      actorType: 'STUDENT',
      status: 'APPROVED',  // Auto-approved — no chain needed
      purpose: 'Campus Entry',
    },
  });

  // Generate QR with 30-minute TTL
  const { token, expiresAt } = await qrService.generateQR(
    pass.id,
    userId,
    'STUDENT',
    null,
    30, // 30-minute TTL
  );

  return {
    message: 'QR generated for campus entry — show this at the gate',
    passId: pass.id,
    token,
    expiresAt,
  };
};

/**
 * Handle a campus geofence EXIT event from the mobile app.
 *
 * - HOSTELLER → Sends a WhatsApp notification to the parent ("Left Campus").
 * - Others → Records the geofence event only.
 *
 * @param {{ userId, studentType, parentPhone, latitude, longitude }} params
 */
export const handleExit = async ({ userId, studentType, parentPhone, latitude, longitude }) => {
  // Log the raw geofence event
  await prisma.geofenceEvent.create({
    data: {
      userId,
      type: 'EXIT',
      latitude: latitude || null,
      longitude: longitude || null,
    },
  });

  if (studentType === 'HOSTELLER' && parentPhone) {
    const user = await prisma.user.findUnique({ where: { id: userId }, select: { name: true } });
    await notificationSvc.sendWhatsApp({
      to: parentPhone,
      templateName: 'left_campus',
      params: [user?.name || 'Your child'],
    });
    return { message: 'Geofence exit logged and parent notified via WhatsApp' };
  }

  return { message: 'Geofence exit logged' };
};
