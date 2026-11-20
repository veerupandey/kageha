# Messaging channels

Open **Settings → Channels** in the Web UI to configure the transport and review connection status.

## Telegram

Create a bot with BotFather. Enter its token and your numeric sender ID in the Telegram settings. Save the connection and use **Test connection**, then send the bot a message from the allowed account. Kageha polls the Bot API, so the bot can run without exposing the Web UI.

## WhatsApp QR

Enter your allowed WhatsApp number in international format using digits only. Save and scan the QR code from **WhatsApp → Linked devices**. Send a message from the allowed account and check that Kageha replies in the same conversation. This companion-device integration is experimental and intended for personal accounts.

## Kageha Personal

Run the [personal companion](../integrations/personal/README.md) for a private, single-owner assistant with WhatsApp, iMessage, and optional Twilio SMS and voice calls. The guide covers pairing, follow-ups, delivery, and replying to approval requests.
