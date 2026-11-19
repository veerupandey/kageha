// Convert Markdown to WhatsApp's native text conventions without changing code.
export function whatsappText(text, { plain = false } = {}) {
  const blocks = text.split(/(```[\s\S]*?(?:```|$)|`[^`\n]*`)/g);
  return blocks
    .map((block, i) => {
      if (i % 2) return block.replace(/^```[\w+-]+\n/, "```\n");
      return block
        .replace(/!?\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g, "$1 — $2")
        .replace(
          /!?\[([^\]\n]+)\]\((?:sandbox:|workspace\/|outputs\/|artifacts\/|\/api\/sessions\/)[^)]+\)/g,
          "$1",
        )
        .replace(
          /\*\*([^*\n]+)\*\*|__([^_\n]+)__/g,
          (_, a, b) => plain ? a || b : "*" + (a || b) + "*",
        )
        .replace(
          /^#{1,6}\s+(.+)$/gm,
          (_, title) => plain ? title : "*" + title.replace(/^\*|\*$/g, "") + "*",
        )
        .replace(/~~([^~\n]+)~~/g, plain ? "$&" : "~$1~")
        .replace(/^\s*\|(?:\s*:?-+:?\s*\|)+\s*$/gm, "")
        .replace(/^\|(.+)\|[ \t]*$/gm, (_, row) =>
          row
            .split("|")
            .map((s) => s.trim())
            .join(" · "),
        );
    })
    .join("");
}

export function whatsappMedia(bytes, name, mimetype) {
  if (["image/png", "image/jpeg", "image/webp"].includes(mimetype))
    return { image: bytes, mimetype };
  if (mimetype === "video/mp4") return { video: bytes, mimetype };
  if (["audio/mpeg", "audio/mp4", "audio/ogg"].includes(mimetype))
    return { audio: bytes, mimetype };
  return { document: bytes, fileName: name, mimetype };
}
