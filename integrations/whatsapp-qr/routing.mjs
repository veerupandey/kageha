export const bareJid = (jid) => String(jid || "").split("@")[0].split(":")[0];

export function toWhatsAppJid(value) {
  const target = String(value || "");
  if (/^\d+$/.test(target)) return `${target}@s.whatsapp.net`;
  if (/^\d+(?:-\d+)?@g\.us$/.test(target) || /^\d+@s\.whatsapp\.net$/.test(target)) return target;
  throw new Error("Invalid WhatsApp destination");
}

export function routeInbound(message, ownJids, sentIds, groupMode = "addressed") {
  const key = message.key || {};
  const chat = key.remoteJid;
  if (!chat || chat.endsWith("@newsletter") || chat.endsWith("@broadcast")) return null;
  if (sentIds.has(key.id)) return null;
  const group = chat.endsWith("@g.us");
  const ownIds = ownJids.filter(Boolean).map(bareJid);
  const selfChat = [chat, key.remoteJidAlt].filter(Boolean).some((jid) => ownIds.includes(bareJid(jid)));
  if (key.fromMe && !selfChat && !group) return null;
  let content = message.message || {};
  for (let depth = 0; depth < 4; depth++) {
    const wrapped = content.ephemeralMessage || content.viewOnceMessage || content.viewOnceMessageV2;
    if (!wrapped?.message) break;
    content = wrapped.message;
  }
  const text = content.conversation || content.extendedTextMessage?.text ||
    content.imageMessage?.caption || content.documentMessage?.caption || content.videoMessage?.caption || "";
  if (!text.trim() && !content.imageMessage && !content.videoMessage && !content.audioMessage && !content.documentMessage) return null;
  const context = content.extendedTextMessage?.contextInfo || content.imageMessage?.contextInfo ||
    content.videoMessage?.contextInfo || content.documentMessage?.contextInfo || {};
  const mentioned = (context.mentionedJid || []).some((jid) => ownIds.includes(bareJid(jid)));
  const repliedToAgent = sentIds.has(context.stanzaId);
  const addressed = mentioned || repliedToAgent || /^\s*\/agent(?:\s|$)/i.test(text);
  if (group && (groupMode === "off" || (groupMode !== "all" && !addressed))) return null;
  const senderJid = key.fromMe || selfChat ? ownJids.find((jid) => jid?.endsWith("@s.whatsapp.net")) || ownJids[0]
    : group ? key.participantPn || key.senderPn || key.participantAlt || key.participant
    : key.senderPn || key.participantPn || chat;
  const sender = bareJid(senderJid);
  if (!sender) return null;
  return { sender, chat, group, content, addressed,
    text: text.replace(/^\s*\/agent(?:\s+|$)/i, "").trim() };
}
