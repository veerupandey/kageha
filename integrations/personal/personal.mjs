import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { randomUUID } from "node:crypto";
const cfg = JSON.parse(
  fs.readFileSync(
    process.env.KAGEHA_PERSONAL_CONFIG ||
      path.join(os.homedir(), ".kageha-personal", "personal.json"),
    "utf8",
  ),
);
const [command = "status", ...args] = process.argv.slice(2);
async function post(route, data) {
  const r = await fetch("http://127.0.0.1:" + cfg.port + "/owner/" + route, {
    method: "POST",
    redirect: "error",
    headers: {
      Authorization: "Bearer " + cfg.backendToken,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(data),
  });
  const response = await r.json();
  if (!r.ok) throw Error(response.error || "Personal instance unavailable");
  return response;
}
if (command === "ask") {
  console.log(
    (await post("message", { id: randomUUID(), text: args.join(" ") })).reply,
  );
} else if (command === "status") {
  const { tasks } = await post("tasks", {});
  for (const t of tasks)
    console.log(
      "#" + t.id,
      t.state,
      t.text,
      "\n",
      t.progress || "",
      t.session ? "\n  Kageha chat " + t.session : "",
    );
  if (!tasks.length) console.log("No tasks yet.");
} else if (command === "messages") {
  for (const m of (await post("delivery", { after: Number(args[0]) || 0 }))
    .messages)
    console.log(m.cursor, m.text);
} else throw Error("Use ask <message>, status, or messages [after cursor]");
