'use strict';

/**
 * Notification Service — Meta Cloud API (WhatsApp Business)
 *
 * Docs: https://developers.facebook.com/docs/whatsapp/cloud-api/messages
 *
 * Required env vars:
 *   META_WHATSAPP_TOKEN       — Bearer token from Meta Developer Console
 *   META_WHATSAPP_PHONE_ID    — Phone number ID for the sender
 *   META_WHATSAPP_API_VERSION — e.g. 'v19.0' (defaults to v19.0)
 */

const META_TOKEN    = process.env.META_WHATSAPP_TOKEN;
const PHONE_ID      = process.env.META_WHATSAPP_PHONE_ID;
const API_VERSION   = process.env.META_WHATSAPP_API_VERSION || 'v19.0';
const META_API_BASE = `https://graph.facebook.com/${API_VERSION}/${PHONE_ID}/messages`;

/**
 * Send a WhatsApp template message via Meta Cloud API.
 *
 * @param {object} params
 * @param {string}   params.to           - Recipient phone number (E.164 format, e.g. '919876543210')
 * @param {string}   params.templateName - Pre-approved WhatsApp template name
 * @param {string[]} params.params       - Ordered list of body parameter values
 * @param {string}  [params.language]    - BCP-47 language code (default: 'en_US')
 */
const sendWhatsApp = async ({ to, templateName, params = [], language = 'en_US' }) => {
  if (!META_TOKEN || !PHONE_ID) {
    console.warn('[NotificationService] META_WHATSAPP_TOKEN or META_WHATSAPP_PHONE_ID not set — skipping WhatsApp send');
    return { skipped: true, reason: 'Missing Meta Cloud API credentials' };
  }

  const body = {
    messaging_product: 'whatsapp',
    to,
    type: 'template',
    template: {
      name: templateName,
      language: { code: language },
      components: params.length > 0
        ? [
            {
              type: 'body',
              parameters: params.map((value) => ({ type: 'text', text: String(value) })),
            },
          ]
        : [],
    },
  };

  try {
    const response = await fetch(META_API_BASE, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${META_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });

    const data = await response.json();

    if (!response.ok) {
      console.error('[NotificationService] Meta API error:', data);
      return { success: false, error: data };
    }

    console.log(`[NotificationService] WhatsApp sent — template: ${templateName}, to: ${to}`);
    return { success: true, data };
  } catch (err) {
    console.error('[NotificationService] Failed to send WhatsApp:', err.message);
    return { success: false, error: err.message };
  }
};

/**
 * Send a plain text WhatsApp message (for non-template use cases, HSM-exempt numbers only).
 *
 * @param {string} to      - Recipient phone (E.164)
 * @param {string} message - Plain text body
 */
const sendTextMessage = async (to, message) => {
  if (!META_TOKEN || !PHONE_ID) {
    console.warn('[NotificationService] Missing credentials — skipping text message');
    return { skipped: true };
  }

  const body = {
    messaging_product: 'whatsapp',
    to,
    type: 'text',
    text: { body: message },
  };

  try {
    const response = await fetch(META_API_BASE, {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${META_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify(body),
    });

    const data = await response.json();
    return response.ok ? { success: true, data } : { success: false, error: data };
  } catch (err) {
    console.error('[NotificationService] Failed to send text message:', err.message);
    return { success: false, error: err.message };
  }
};

module.exports = { sendWhatsApp, sendTextMessage };
